package dev.nick.ytmdl

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.database.sqlite.SQLiteDatabase
import android.database.sqlite.SQLiteException
import android.net.Uri
import android.os.Bundle
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.Player
import androidx.media3.session.MediaConstants
import androidx.media3.session.MediaSession.MediaItemsWithStartPosition
import java.io.File
import java.io.FileNotFoundException
import org.json.JSONArray
import org.json.JSONObject

/**
 * The library as cars (Android Auto) and other media browsers see it:
 * playlists, albums, artists and songs, read from the app's library index
 * (crates/library, files/library.db) in the player's process. It only
 * reads; the app owns the database.
 *
 * Media ids: "root"; the tabs "playlists", "albums", "artists" and "songs";
 * a list, "playlist:<id>", "album:<album artist>/<title>" or "artist:<name>"
 * (names Uri-encoded); and a song in a list, "<list>|<track id>", which plays
 * the list from that song. "shuffle" plays every song shuffled. Queue entries
 * get the app's ids, "<track id>.<n>" (see YtmdlPlayer).
 */
class Browse(private val context: Context) {
    companion object {
        private const val TAG = "ytmdl"
        const val ROOT = "root"
        private const val PLAYLISTS = "playlists"
        private const val ALBUMS = "albums"
        private const val ARTISTS = "artists"
        private const val SONGS = "songs"
        private const val SHUFFLE = "shuffle"
        private const val SEARCH = "search"

        /** Cover sizes in the art cache (crates/library/src/art.rs). */
        private const val ART_SMALL = 240
        private const val ART_LARGE = 720

        private const val TRACK_COLUMNS =
            "t.id, t.path, t.title, t.artists, t.album, t.album_artist, t.disc_number, t.track_number, " +
                "t.duration, t.art, t.added_at, t.gain, t.peak"

        /** A queue entry, as the app and the car both make them. */
        fun queueItem(
            id: String,
            path: String,
            title: String,
            artist: String?,
            album: String?,
            art: Uri?,
            gain: Double?,
            peak: Double?,
        ): MediaItem {
            val extras = Bundle()
            if (gain != null) {
                extras.putDouble(PlaybackService.GAIN, gain)
                extras.putDouble(PlaybackService.PEAK, peak ?: 1.0)
            }
            val meta = MediaMetadata.Builder()
                .setTitle(title)
                .setArtist(artist?.ifEmpty { null })
                .setAlbumTitle(album?.ifEmpty { null })
                .setArtworkUri(art)
                .setIsPlayable(true)
                .setIsBrowsable(false)
                .setMediaType(MediaMetadata.MEDIA_TYPE_MUSIC)
                .setExtras(extras)
                .build()
            return MediaItem.Builder()
                .setMediaId(id)
                .setUri(Uri.fromFile(File(path)))
                .setMediaMetadata(meta)
                .build()
        }

        /** The `n` after the largest one in [player]'s queue, for new entry ids. */
        fun nextSerial(player: Player): Long {
            var max = -1L
            for (i in 0 until player.mediaItemCount) {
                val n = player.getMediaItemAt(i).mediaId.substringAfter('.', "").toLongOrNull() ?: continue
                max = maxOf(max, n)
            }
            return max + 1
        }
    }

    data class Track(
        val id: Long,
        val path: String,
        val title: String,
        val artists: List<String>,
        val album: String?,
        val albumArtist: String,
        val disc: Int?,
        val number: Int?,
        val duration: Double?,
        val art: String?,
        val addedAt: Long,
        val gain: Double?,
        val peak: Double?,
    )

    private class Playlist(val id: Long, val name: String, val count: Int, val art: String?)

    // ---- reading the library ----

    private fun <T> read(block: (SQLiteDatabase) -> T): T? {
        val file = File(context.filesDir, "library.db")
        if (!file.exists()) return null
        return try {
            val flags = SQLiteDatabase.OPEN_READONLY or SQLiteDatabase.NO_LOCALIZED_COLLATORS
            SQLiteDatabase.openDatabase(file.path, null, flags).use(block)
        } catch (e: SQLiteException) {
            Log.w(TAG, "reading the library", e)
            null
        }
    }

    private fun Cursor.track(): Track {
        fun int(i: Int) = if (isNull(i)) null else getInt(i)
        fun double(i: Int) = if (isNull(i)) null else getDouble(i)
        val artists = try {
            val list = JSONArray(getString(3))
            (0 until list.length()).map { list.getString(it) }
        } catch (e: org.json.JSONException) {
            emptyList()
        }
        return Track(
            id = getLong(0),
            path = getString(1),
            title = getString(2),
            artists = artists,
            album = if (isNull(4)) null else getString(4),
            albumArtist = getString(5),
            disc = int(6),
            number = int(7),
            duration = double(8),
            art = if (isNull(9)) null else getString(9),
            addedAt = getLong(10),
            gain = double(11),
            peak = double(12),
        )
    }

    private fun queryTracks(sql: String, vararg args: String): List<Track> = read { db ->
        db.rawQuery(sql, args).use { c -> buildList { while (c.moveToNext()) add(c.track()) } }
    } ?: emptyList()

    /** Newest first, as the library's Songs tab lists them. */
    fun tracks(): List<Track> = queryTracks("SELECT $TRACK_COLUMNS FROM tracks t ORDER BY t.added_at DESC, t.id DESC")

    private fun playlists(): List<Playlist> = read { db ->
        val sql = "SELECT p.id, p.name, " +
            "(SELECT COUNT(*) FROM playlist_tracks pt WHERE pt.playlist_id = p.id), " +
            "(SELECT t.art FROM playlist_tracks pt JOIN tracks t ON t.id = pt.track_id " +
            " WHERE pt.playlist_id = p.id AND t.art IS NOT NULL ORDER BY pt.position LIMIT 1) " +
            "FROM playlists p ORDER BY p.created_at DESC, p.id DESC"
        db.rawQuery(sql, null).use { c ->
            buildList {
                while (c.moveToNext()) add(Playlist(c.getLong(0), c.getString(1), c.getInt(2), if (c.isNull(3)) null else c.getString(3)))
            }
        }
    } ?: emptyList()

    private fun playlistTracks(id: Long): List<Track> = queryTracks(
        "SELECT $TRACK_COLUMNS FROM playlist_tracks pt JOIN tracks t ON t.id = pt.track_id " +
            "WHERE pt.playlist_id = ? ORDER BY pt.position, pt.id",
        id.toString(),
    )

    /**
     * Skipped sections (see crates/library/src/sections.rs) as (a, b) ms, by
     * track id: negative for cached songs, as in media ids.
     */
    fun skips(): Map<Long, List<Pair<Long, Long>>> = read { db ->
        val sql = "SELECT t.id, s.a_ms, s.b_ms FROM sections s JOIN tracks t ON t.video_id = s.video_id WHERE s.skip " +
            "UNION ALL SELECT -c.id, s.a_ms, s.b_ms FROM sections s JOIN cached c ON c.video_id = s.video_id WHERE s.skip " +
            "ORDER BY 2"
        db.rawQuery(sql, null).use { c ->
            val skips = HashMap<Long, MutableList<Pair<Long, Long>>>()
            while (c.moveToNext()) skips.getOrPut(c.getLong(0)) { ArrayList() }.add(c.getLong(1) to c.getLong(2))
            skips
        }
    } ?: emptyMap()

    /** The queue the app saved last (see `Saved` in src/player.rs): tracks, index, position. */
    private fun savedQueue(): Triple<List<Track>, Int, Long>? {
        val json = read { db ->
            db.rawQuery("SELECT value FROM settings WHERE key = 'player'", null).use { c ->
                if (c.moveToFirst()) c.getString(0) else null
            }
        } ?: return null
        val saved = try {
            JSONObject(json)
        } catch (e: org.json.JSONException) {
            return null
        }
        val ids = saved.optJSONArray("ids") ?: return null
        val byId = tracks().associateBy { it.id }
        val wanted = (0 until ids.length()).map { ids.optLong(it) }
        val tracks = wanted.mapNotNull { byId[it] }
        if (tracks.isEmpty()) return null
        val current = wanted.getOrNull(saved.optInt("index"))?.let { id -> tracks.indexOfFirst { it.id == id } } ?: -1
        return if (current >= 0) Triple(tracks, current, saved.optLong("position_ms")) else Triple(tracks, 0, 0L)
    }

    // ---- lists ----

    private fun encode(s: String): String = Uri.encode(s)

    private fun decode(s: String): String = Uri.decode(s)

    private fun albumTracks(tracks: List<Track>): List<Track> = tracks.sortedWith(
        compareBy<Track>({ it.disc ?: 1 }, { it.number == null }, { it.number ?: 0 }, { it.addedAt }, { it.id })
    )

    /** An artist's songs, grouped by album (as the app's artist page lists them). */
    private fun artistTracks(tracks: List<Track>): List<Track> = tracks.sortedWith(
        compareBy<Track>(
            { it.album == null }, { it.album?.lowercase() }, { it.disc ?: 1 }, { it.number == null }, { it.number ?: 0 },
            { it.title.lowercase() },
        )
    )

    /** The songs of list [id], in order; null if there is no such list. */
    fun list(id: String): List<Track>? {
        val (kind, arg) = id.split(':', limit = 2).let { it[0] to it.getOrElse(1) { "" } }
        return when (kind) {
            SONGS -> tracks()
            SHUFFLE -> tracks().shuffled()
            "playlist" -> arg.toLongOrNull()?.let { playlistTracks(it) }
            "album" -> {
                val (artist, title) = arg.split('/', limit = 2).map(::decode).let { it[0] to it.getOrElse(1) { "" } }
                albumTracks(tracks().filter { it.albumArtist == artist && it.album == title })
            }
            "artist" -> decode(arg).let { name -> artistTracks(tracks().filter { name in it.artists }) }
            SEARCH -> search(decode(arg))
            else -> null
        }
    }

    /** Songs matching [query], best first: exact titles, artists and albums, then partial ones. */
    fun search(query: String): List<Track> {
        val q = query.trim().lowercase()
        if (q.isEmpty()) return emptyList()
        fun score(t: Track): Int {
            val title = t.title.lowercase()
            val artists = t.artists.map { it.lowercase() }
            val album = t.album?.lowercase()
            return when {
                title == q -> 0
                q in artists -> 1
                album == q -> 2
                title.contains(q) -> 3
                artists.any { it.contains(q) } -> 4
                album?.contains(q) == true -> 5
                else -> -1
            }
        }
        return tracks().map { it to score(it) }.filter { it.second >= 0 }.sortedBy { it.second }.map { it.first }
    }

    // ---- media items ----

    private fun art(key: String?, size: Int): Uri? = key?.let { ArtProvider.uri(context, "$it-$size.jpg") }

    private fun folder(id: String, title: String, type: Int, subtitle: String? = null, art: Uri? = null, grid: Boolean = false): MediaItem {
        val extras = Bundle()
        extras.putInt(
            MediaConstants.EXTRAS_KEY_CONTENT_STYLE_BROWSABLE,
            if (grid) MediaConstants.EXTRAS_VALUE_CONTENT_STYLE_GRID_ITEM else MediaConstants.EXTRAS_VALUE_CONTENT_STYLE_LIST_ITEM,
        )
        extras.putInt(MediaConstants.EXTRAS_KEY_CONTENT_STYLE_PLAYABLE, MediaConstants.EXTRAS_VALUE_CONTENT_STYLE_LIST_ITEM)
        val meta = MediaMetadata.Builder()
            .setTitle(title)
            .setSubtitle(subtitle)
            .setArtworkUri(art)
            .setIsBrowsable(true)
            .setIsPlayable(false)
            .setMediaType(type)
            .setExtras(extras)
            .build()
        return MediaItem.Builder().setMediaId(id).setMediaMetadata(meta).build()
    }

    private fun song(listId: String, t: Track): MediaItem {
        val meta = MediaMetadata.Builder()
            .setTitle(t.title)
            .setArtist(t.artists.joinToString(", ").ifEmpty { null })
            .setAlbumTitle(t.album)
            .setArtworkUri(art(t.art, ART_SMALL))
            .setDurationMs(t.duration?.let { (it * 1000).toLong() })
            .setIsBrowsable(false)
            .setIsPlayable(true)
            .setMediaType(MediaMetadata.MEDIA_TYPE_MUSIC)
            .build()
        return MediaItem.Builder().setMediaId("$listId|${t.id}").setMediaMetadata(meta).build()
    }

    private fun queueItem(t: Track, n: Long): MediaItem = queueItem(
        "${t.id}.$n", t.path, t.title, t.artists.joinToString(", "), t.album, art(t.art, ART_LARGE), t.gain, t.peak,
    )

    fun root(): MediaItem = folder(ROOT, "ytmdl", MediaMetadata.MEDIA_TYPE_FOLDER_MIXED)

    private fun plural(n: Int, one: String, many: String) = "$n ${if (n == 1) one else many}"

    private fun tabs(): List<MediaItem> = listOf(
        folder(PLAYLISTS, "Playlists", MediaMetadata.MEDIA_TYPE_FOLDER_PLAYLISTS),
        folder(ALBUMS, "Albums", MediaMetadata.MEDIA_TYPE_FOLDER_ALBUMS, grid = true),
        folder(ARTISTS, "Artists", MediaMetadata.MEDIA_TYPE_FOLDER_ARTISTS, grid = true),
        folder(SONGS, "Songs", MediaMetadata.MEDIA_TYPE_FOLDER_MIXED),
    )

    /** What list [parent] holds, or null if there is no such list. */
    fun children(parent: String): List<MediaItem>? = when (parent) {
        ROOT -> tabs()
        PLAYLISTS -> playlists().map {
            folder("playlist:${it.id}", it.name, MediaMetadata.MEDIA_TYPE_PLAYLIST, plural(it.count, "song", "songs"), art(it.art, ART_SMALL))
        }
        ALBUMS -> tracks()
            .filter { !it.album.isNullOrEmpty() }
            .groupBy { it.albumArtist to it.album!! }
            .map { (key, songs) ->
                val (artist, title) = key
                folder(
                    "album:${encode(artist)}/${encode(title)}", title, MediaMetadata.MEDIA_TYPE_ALBUM, artist,
                    art(songs.firstNotNullOfOrNull { it.art }, ART_SMALL),
                )
            }
        ARTISTS -> tracks()
            .flatMap { t -> t.artists.map { it to t } }
            .groupBy({ it.first }, { it.second })
            .entries
            .sortedWith(compareBy(String.CASE_INSENSITIVE_ORDER) { it.key })
            .map { (name, songs) ->
                folder(
                    "artist:${encode(name)}", name, MediaMetadata.MEDIA_TYPE_ARTIST, plural(songs.size, "song", "songs"),
                    art(songs.firstNotNullOfOrNull { it.art }, ART_SMALL),
                )
            }
        SONGS -> {
            val tracks = tracks()
            val shuffle = MediaMetadata.Builder()
                .setTitle("Shuffle all")
                .setSubtitle(plural(tracks.size, "song", "songs"))
                .setIsBrowsable(false)
                .setIsPlayable(true)
                .setMediaType(MediaMetadata.MEDIA_TYPE_PLAYLIST)
                .build()
            listOf(MediaItem.Builder().setMediaId(SHUFFLE).setMediaMetadata(shuffle).build()) + tracks.map { song(SONGS, it) }
        }
        else -> list(parent)?.map { song(parent, it) }
    }

    /** The item with [id], for a browser asking about one. */
    fun item(id: String): MediaItem? {
        if (id == ROOT) return root()
        tabs().firstOrNull { it.mediaId == id }?.let { return it }
        val listId = id.substringBeforeLast('|', "")
        if (listId.isNotEmpty()) {
            val track = id.substringAfterLast('|').toLongOrNull()
            return list(listId)?.firstOrNull { it.id == track }?.let { song(listId, it) }
        }
        return children(parentOf(id))?.firstOrNull { it.mediaId == id }
    }

    private fun parentOf(id: String): String = when (id.substringBefore(':')) {
        "playlist" -> PLAYLISTS
        "album" -> ALBUMS
        "artist" -> ARTISTS
        else -> ROOT
    }

    /** Search results, each playing all of them from itself. */
    fun searchResults(query: String): List<MediaItem> = search(query).map { song("$SEARCH:${encode(query)}", it) }

    // ---- playing what was picked ----

    /**
     * The queue that playing [items] makes, from [start]: a song picked in a
     * list plays the list from that song, a list plays from its start, and a
     * spoken search plays what it finds (or everything shuffled, for "play
     * music"). Null if none of it is in the library.
     */
    fun resolve(items: List<MediaItem>, start: Int, positionMs: Long, serial: Long): MediaItemsWithStartPosition? {
        if (items.size == 1) {
            val (tracks, index) = expand(items[0]) ?: return null
            if (tracks.isEmpty()) return null
            val queue = tracks.mapIndexed { i, t -> queueItem(t, serial + i) }
            // The position asked for is in the song picked.
            return MediaItemsWithStartPosition(queue, index, positionMs)
        }
        val queue = resolveEach(items, serial)
        if (queue.isEmpty()) return null
        return MediaItemsWithStartPosition(queue, start.coerceIn(0, queue.size - 1), positionMs)
    }

    /** Each of [items] as queue entries: a song itself, a list all of it. Songs
     * the app sent (ready to play) stay as they are. */
    fun resolveEach(items: List<MediaItem>, serial: Long): List<MediaItem> {
        var n = serial
        return items.flatMap { item ->
            if (item.localConfiguration != null) return@flatMap listOf(item)
            val listId = item.mediaId.substringBeforeLast('|', "")
            val tracks = if (listId.isNotEmpty()) {
                val id = item.mediaId.substringAfterLast('|').toLongOrNull()
                list(listId)?.filter { it.id == id }.orEmpty()
            } else {
                expand(item)?.first.orEmpty()
            }
            tracks.map { queueItem(it, n++) }
        }
    }

    private fun expand(item: MediaItem): Pair<List<Track>, Int>? {
        val id = item.mediaId
        if (id.isEmpty()) {
            val query = item.requestMetadata.searchQuery ?: return null
            return (if (query.isBlank()) tracks().shuffled() else search(query)) to 0
        }
        val listId = id.substringBeforeLast('|', "")
        if (listId.isEmpty()) return list(id)?.let { it to 0 }
        val tracks = list(listId) ?: return null
        val track = id.substringAfterLast('|').toLongOrNull()
        return tracks to tracks.indexOfFirst { it.id == track }.coerceAtLeast(0)
    }

    /** The queue the app last had, to pick up where it left off. */
    fun resumption(serial: Long): MediaItemsWithStartPosition? {
        val (tracks, index, position) = savedQueue() ?: return null
        return MediaItemsWithStartPosition(tracks.mapIndexed { i, t -> queueItem(t, serial + i) }, index, position)
    }
}

/**
 * Serves cover art from the art cache as `content://<package>.art/<name>`, for
 * the car and the system's media controls, which can't read the app's files.
 * Only art cache names are served, read-only.
 */
class ArtProvider : ContentProvider() {
    companion object {
        private val NAME = Regex("[0-9a-f]{16}-\\d+\\.jpg")

        fun uri(context: Context, name: String): Uri =
            Uri.Builder().scheme("content").authority("${context.packageName}.art").appendPath(name).build()
    }

    override fun onCreate() = true

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        if (mode != "r") throw SecurityException("cover art is read-only")
        val name = uri.lastPathSegment?.takeIf { NAME.matches(it) } ?: throw FileNotFoundException(uri.toString())
        val file = File(File(context!!.filesDir, "art"), name)
        return ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
    }

    override fun getType(uri: Uri) = "image/jpeg"

    override fun query(uri: Uri, projection: Array<String>?, selection: String?, args: Array<String>?, sort: String?): Cursor? = null

    override fun insert(uri: Uri, values: ContentValues?): Uri? = null

    override fun update(uri: Uri, values: ContentValues?, selection: String?, args: Array<String>?) = 0

    override fun delete(uri: Uri, selection: String?, args: Array<String>?) = 0
}
