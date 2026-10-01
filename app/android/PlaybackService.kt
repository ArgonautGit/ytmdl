package dev.nick.ytmdl

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.Player
import androidx.media3.common.Timeline
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.PlayerMessage
import androidx.media3.session.DefaultMediaNotificationProvider
import androidx.media3.session.LibraryResult
import androidx.media3.session.MediaLibraryService
import androidx.media3.session.MediaLibraryService.LibraryParams
import androidx.media3.session.MediaLibraryService.MediaLibrarySession
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSession.MediaItemsWithStartPosition
import androidx.media3.session.SessionCommand
import androidx.media3.session.SessionError
import androidx.media3.session.SessionResult
import com.google.common.collect.ImmutableList
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import com.google.common.util.concurrent.ListeningExecutorService
import com.google.common.util.concurrent.MoreExecutors
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.util.concurrent.Callable
import java.util.concurrent.Executors
import kotlin.math.pow

/**
 * Plays the queue with the media notification, lock screen and headset
 * controls. Runs in its own ":player" process, so music keeps going when the
 * app's process ends (see MainActivity).
 *
 * A-B loops live here too, so they hold with the screen off: a song loop seeks
 * back to A on reaching B, a queue loop goes back to its first song when its
 * last one ends. So does the sleep timer. The app sets them with custom
 * commands and reads them back from the session extras.
 *
 * Skipped sections are read from the library here (they hold in the car and
 * with the app closed too): playing into one jumps to its end, and so does
 * landing in one (a seek, a song starting with one). A song loop over one
 * plays it, though.
 *
 * Every song played is logged to `files/listens.log` for the app's listening
 * stats (see crates/library/src/listens.rs), since the app may not be running.
 *
 * Cars (Android Auto) and other media browsers browse the library here and
 * play from it (see Browse), also without the app running.
 */
class PlaybackService : MediaLibraryService() {
    companion object {
        /** Args: "id" (media id), "a" and "b" (ms); no "id" clears. */
        const val SONG_LOOP = "dev.nick.ytmdl.SONG_LOOP"
        /** Args: "first" and "last" (media ids); none clears. */
        const val QUEUE_LOOP = "dev.nick.ytmdl.QUEUE_LOOP"
        /** Args: "at" (wall-clock ms to pause at) or "endOfSong"; neither clears. */
        const val SLEEP = "dev.nick.ytmdl.SLEEP"
        /** Args: "on", whether songs play at an even loudness. */
        const val NORMALIZE = "dev.nick.ytmdl.NORMALIZE"
        /** No args: the skipped sections changed in the library. */
        const val SKIPS = "dev.nick.ytmdl.SKIPS"
        /** A song's ReplayGain (dB) and peak, in its metadata extras. */
        const val GAIN = "gain"
        const val PEAK = "peak"

        private const val TAG = "ytmdl"
        /** The sleep timer fades the music out over its last seconds. */
        private const val FADE_MS = 15_000L
        /** Shorter listens (skips) aren't logged. */
        private const val MIN_LISTEN_MS = 1_000L
        /**
         * Added to the ReplayGain, which aims at -18 LUFS: -14 LUFS is what
         * streaming services play at. The gain only ever turns songs down
         * (the volume can't go past 1), so quieter songs play as they are.
         */
        private const val PREAMP_DB = 4.0
        private const val PREFS = "player"
    }

    private data class SongLoop(val id: String, val a: Long, val b: Long)
    private data class QueueLoop(val first: String, val last: String)
    /** A part of a song left out, in ms. */
    private data class Skip(val a: Long, val b: Long)

    private var session: MediaLibrarySession? = null
    private var player: ExoPlayer? = null
    private lateinit var browse: Browse
    /** Reads the library for browsers, off the main thread. */
    private val io: ListeningExecutorService = MoreExecutors.listeningDecorator(Executors.newSingleThreadExecutor())
    private var songLoop: SongLoop? = null
    private var queueLoop: QueueLoop? = null
    /** Skipped sections by track id (negative for cached songs, as in media ids). */
    private var skips: Map<Long, List<Skip>> = emptyMap()
    /** Positions are per queue index, so these are rebuilt when the queue changes. */
    private val loopMessages = ArrayList<PlayerMessage>()
    private var currentId: String? = null
    private var published: Bundle? = null

    private val main = Handler(Looper.getMainLooper())
    /** Wall-clock ms the sleep timer pauses at; 0 when not set. */
    private var sleepAt = 0L
    /** The sleep timer pauses at the end of the song instead. */
    private var sleepAtEnd = false
    /** The sleep timer's fade: the share of the volume left. */
    private var fade = 1f
    /** Songs play at an even loudness (the app's setting, kept for restarts). */
    private var normalize = true

    /** The song being listened to, for the log: its media id, when it started
     * (wall clock) and how long it has played so far. */
    private var listenId: String? = null
    private var listenStart = 0L
    private var listenedMs = 0L
    /** elapsedRealtime of the last count while playing; -1 while not. */
    private var playingSince = -1L

    override fun onCreate() {
        super.onCreate()
        val music = AudioAttributes.Builder()
            .setUsage(C.USAGE_MEDIA)
            .setContentType(C.AUDIO_CONTENT_TYPE_MUSIC)
            .build()
        val player = ExoPlayer.Builder(this)
            .setAudioAttributes(music, /* handleAudioFocus = */ true)
            .setHandleAudioBecomingNoisy(true)
            .setWakeMode(C.WAKE_MODE_LOCAL)
            .build()
        player.addListener(loopListener)
        player.addListener(listenListener)
        player.addListener(volumeListener)
        this.player = player
        browse = Browse(this)
        normalize = getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(NORMALIZE, true)
        loadSkips()

        setMediaNotificationProvider(
            DefaultMediaNotificationProvider.Builder(this).build().apply { setSmallIcon(R.drawable.ytmdl_notification) }
        )
        val builder = MediaLibrarySession.Builder(this, player, callback)
        // Tapping the notification brings the app back like the launcher does.
        packageManager.getLaunchIntentForPackage(packageName)?.let { open ->
            val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            builder.setSessionActivity(PendingIntent.getActivity(this, 0, open, flags))
        }
        session = builder.build()
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaLibrarySession? = session

    /** Swiping the app away keeps music that is playing; a paused player goes. */
    override fun onTaskRemoved(rootIntent: Intent?) {
        val player = session?.player
        if (player == null || !player.playWhenReady || player.mediaItemCount == 0) {
            stopSelf()
        }
    }

    override fun onDestroy() {
        io.shutdown()
        finishListen()
        main.removeCallbacks(sleepTick)
        loopMessages.forEach { it.cancel() }
        session?.run {
            player.release()
            release()
        }
        session = null
        player = null
        super.onDestroy()
    }

    private fun <T> library(work: () -> T): ListenableFuture<T> = io.submit(Callable { work() })

    private fun <T : Any> notFound(what: String): LibraryResult<T> =
        LibraryResult.ofError<T>(SessionError(SessionError.ERROR_BAD_VALUE, "$what is not in the library"))

    /** Page [page] of [items], [size] to a page. */
    private fun pageOf(items: List<MediaItem>, page: Int, size: Int): List<MediaItem> {
        if (page < 0 || size <= 0) return items
        val from = page.toLong() * size
        if (from >= items.size) return emptyList()
        return items.subList(from.toInt(), minOf(items.size.toLong(), from + size).toInt())
    }

    private val callback = object : MediaLibrarySession.Callback {
        override fun onConnect(
            session: MediaSession,
            controller: MediaSession.ControllerInfo,
        ): MediaSession.ConnectionResult {
            val result = MediaSession.ConnectionResult.AcceptedResultBuilder(session)
            if (controller.packageName == packageName) {
                result.setAvailableSessionCommands(
                    MediaSession.ConnectionResult.DEFAULT_SESSION_AND_LIBRARY_COMMANDS.buildUpon()
                        .add(SessionCommand(SONG_LOOP, Bundle.EMPTY))
                        .add(SessionCommand(QUEUE_LOOP, Bundle.EMPTY))
                        .add(SessionCommand(SLEEP, Bundle.EMPTY))
                        .add(SessionCommand(NORMALIZE, Bundle.EMPTY))
                        .add(SessionCommand(SKIPS, Bundle.EMPTY))
                        .build()
                )
            }
            return result.build()
        }

        override fun onCustomCommand(
            session: MediaSession,
            controller: MediaSession.ControllerInfo,
            customCommand: SessionCommand,
            args: Bundle,
        ): ListenableFuture<SessionResult> {
            when (customCommand.customAction) {
                SONG_LOOP -> songLoop = args.getString("id")
                    ?.let { SongLoop(it, args.getLong("a"), args.getLong("b")) }
                    ?.takeIf { it.b > it.a }
                QUEUE_LOOP -> {
                    val first = args.getString("first")
                    val last = args.getString("last")
                    queueLoop = if (first != null && last != null) QueueLoop(first, last) else null
                }
                SLEEP -> {
                    setSleep(args.getLong("at"), args.getBoolean("endOfSong"))
                    return Futures.immediateFuture(SessionResult(SessionResult.RESULT_SUCCESS))
                }
                NORMALIZE -> {
                    normalize = args.getBoolean("on", true)
                    getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putBoolean(NORMALIZE, normalize).apply()
                    applyVolume()
                    return Futures.immediateFuture(SessionResult(SessionResult.RESULT_SUCCESS))
                }
                SKIPS -> {
                    loadSkips()
                    return Futures.immediateFuture(SessionResult(SessionResult.RESULT_SUCCESS))
                }
            }
            armLoops()
            return Futures.immediateFuture(SessionResult(SessionResult.RESULT_SUCCESS))
        }

        // ---- browsing (Browse) ----

        override fun onGetLibraryRoot(
            session: MediaLibrarySession,
            browser: MediaSession.ControllerInfo,
            params: LibraryParams?,
        ): ListenableFuture<LibraryResult<MediaItem>> = Futures.immediateFuture(LibraryResult.ofItem(browse.root(), params))

        override fun onGetChildren(
            session: MediaLibrarySession,
            browser: MediaSession.ControllerInfo,
            parentId: String,
            page: Int,
            pageSize: Int,
            params: LibraryParams?,
        ): ListenableFuture<LibraryResult<ImmutableList<MediaItem>>> = library {
            val children = browse.children(parentId)
            if (children == null) notFound(parentId) else LibraryResult.ofItemList(pageOf(children, page, pageSize), params)
        }

        override fun onGetItem(
            session: MediaLibrarySession,
            browser: MediaSession.ControllerInfo,
            mediaId: String,
        ): ListenableFuture<LibraryResult<MediaItem>> = library {
            browse.item(mediaId)?.let { LibraryResult.ofItem(it, null) } ?: notFound(mediaId)
        }

        override fun onSearch(
            session: MediaLibrarySession,
            browser: MediaSession.ControllerInfo,
            query: String,
            params: LibraryParams?,
        ): ListenableFuture<LibraryResult<Void>> = library {
            val count = browse.search(query).size
            main.post { session.notifySearchResultChanged(browser, query, count, params) }
            LibraryResult.ofVoid()
        }

        override fun onGetSearchResult(
            session: MediaLibrarySession,
            browser: MediaSession.ControllerInfo,
            query: String,
            page: Int,
            pageSize: Int,
            params: LibraryParams?,
        ): ListenableFuture<LibraryResult<ImmutableList<MediaItem>>> = library {
            LibraryResult.ofItemList(pageOf(browse.searchResults(query), page, pageSize), params)
        }

        // ---- playing what a browser picked ----

        /** The app sends songs ready to play; browsers send what was picked. */
        override fun onAddMediaItems(
            mediaSession: MediaSession,
            controller: MediaSession.ControllerInfo,
            mediaItems: MutableList<MediaItem>,
        ): ListenableFuture<MutableList<MediaItem>> {
            if (mediaItems.all { it.localConfiguration != null }) return Futures.immediateFuture(mediaItems)
            val serial = Browse.nextSerial(mediaSession.player)
            return library { browse.resolveEach(mediaItems, serial).toMutableList() }
        }

        override fun onSetMediaItems(
            mediaSession: MediaSession,
            controller: MediaSession.ControllerInfo,
            mediaItems: MutableList<MediaItem>,
            startIndex: Int,
            startPositionMs: Long,
        ): ListenableFuture<MediaItemsWithStartPosition> {
            if (mediaItems.all { it.localConfiguration != null }) {
                return Futures.immediateFuture(MediaItemsWithStartPosition(mediaItems, startIndex, startPositionMs))
            }
            val serial = Browse.nextSerial(mediaSession.player)
            return library {
                browse.resolve(mediaItems, startIndex, startPositionMs, serial)
                    ?: throw UnsupportedOperationException("nothing to play in the library")
            }
        }

        /** Play with nothing queued (in the car, say) picks up the app's last queue. */
        override fun onPlaybackResumption(
            mediaSession: MediaSession,
            controller: MediaSession.ControllerInfo,
        ): ListenableFuture<MediaItemsWithStartPosition> {
            val serial = Browse.nextSerial(mediaSession.player)
            return library { browse.resumption(serial) ?: throw UnsupportedOperationException("no queue to resume") }
        }
    }

    private val loopListener = object : Player.Listener {
        override fun onTimelineChanged(timeline: Timeline, reason: Int) {
            if (reason == Player.TIMELINE_CHANGE_REASON_PLAYLIST_CHANGED) {
                armLoops()
                // A song downloaded again has a new track id.
                loadSkips()
            }
        }

        override fun onPositionDiscontinuity(old: Player.PositionInfo, new: Player.PositionInfo, reason: Int) =
            skipOut()

        override fun onMediaItemTransition(item: MediaItem?, reason: Int) {
            val left = currentId
            currentId = item?.mediaId
            // A song loop belongs to its song: skipping away ends it.
            if (songLoop != null && item?.mediaId != songLoop?.id) {
                songLoop = null
                armLoops()
            }
            skipOut()
            // The end-of-last-song message was missed (seeking right to its end,
            // say): catch the automatic move on from it instead.
            val loop = queueLoop ?: return
            val p = player ?: return
            if (reason == Player.MEDIA_ITEM_TRANSITION_REASON_AUTO && left == loop.last) {
                val first = indexOf(p, loop.first)
                if (first != C.INDEX_UNSET && first != p.currentMediaItemIndex) p.seekToDefaultPosition(first)
            }
        }

        override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) {
            if (reason == Player.PLAY_WHEN_READY_CHANGE_REASON_END_OF_MEDIA_ITEM && sleepAtEnd) clearSleep()
        }

        override fun onPlaybackStateChanged(state: Int) {
            if (state == Player.STATE_READY) skipOut()
            if (state == Player.STATE_ENDED && sleepAtEnd) clearSleep()
            // The loop's last song was also the queue's last.
            val loop = queueLoop ?: return
            val p = player ?: return
            if (state == Player.STATE_ENDED && p.currentMediaItem?.mediaId == loop.last) {
                val first = indexOf(p, loop.first)
                if (first != C.INDEX_UNSET) {
                    p.seekToDefaultPosition(first)
                    p.play()
                }
            }
        }
    }

    private fun indexOf(p: Player, id: String): Int {
        for (i in 0 until p.mediaItemCount) {
            if (p.getMediaItemAt(i).mediaId == id) return i
        }
        return C.INDEX_UNSET
    }

    /** The skipped sections of queue entry [index], less any the song loop plays. */
    private fun skipsAt(p: Player, index: Int): List<Skip> {
        val id = p.getMediaItemAt(index).mediaId
        val found = id.substringBefore('.').toLongOrNull()?.let { skips[it] } ?: return emptyList()
        val loop = songLoop?.takeIf { it.id == id } ?: return found
        return found.filter { it.b <= loop.a || it.a >= loop.b }
    }

    /** Where skip [s] of the song playing ends: at the song's end, at most. */
    private fun endOf(p: Player, s: Skip): Long {
        val duration = p.duration
        return if (duration == C.TIME_UNSET) s.b else minOf(s.b, duration)
    }

    /** Jumps to the end of skip [s] of the song playing, unless it is the whole song. */
    private fun skipPast(p: Player, s: Skip) {
        val end = endOf(p, s)
        if (s.a <= 0 && end == p.duration) return
        p.seekTo(end)
    }

    /** Leaves a skipped section the song is in. */
    private fun skipOut() {
        val p = player ?: return
        val index = p.currentMediaItemIndex
        if (index == C.INDEX_UNSET || index >= p.mediaItemCount) return
        val at = p.currentPosition
        skipsAt(p, index).firstOrNull { at >= it.a && at < endOf(p, it) }?.let { skipPast(p, it) }
    }

    /** Reads the skipped sections from the library, off the main thread. */
    private fun loadSkips() {
        if (io.isShutdown) return
        io.execute {
            val found = browse.skips().mapValues { (_, l) -> l.map { (a, b) -> Skip(a, b) } }
            main.post {
                if (found != skips) {
                    skips = found
                    armLoops()
                    skipOut()
                }
            }
        }
    }

    /**
     * Recreates the messages behind the loops and skips, dropping loops whose
     * songs left the queue.
     */
    private fun armLoops() {
        val p = player ?: return
        loopMessages.forEach { it.cancel() }
        loopMessages.clear()
        val main = Looper.getMainLooper()

        songLoop?.let { loop ->
            val i = indexOf(p, loop.id)
            if (i == C.INDEX_UNSET) {
                songLoop = null
            } else {
                loopMessages += p.createMessage { _, _ ->
                    if (songLoop == loop) {
                        p.seekTo(loop.a)
                        // A looping song never ends, so B is where it stops.
                        if (sleepAtEnd) {
                            p.pause()
                            clearSleep()
                        }
                    }
                }
                    .setPosition(i, loop.b)
                    .setLooper(main)
                    .setDeleteAfterDelivery(false)
                    .send()
            }
        }

        queueLoop?.let { found ->
            // Moving songs can put the loop's first song after its last.
            val loop = if (indexOf(p, found.first) > indexOf(p, found.last)) QueueLoop(found.last, found.first) else found
            queueLoop = loop
            val last = indexOf(p, loop.last)
            if (last == C.INDEX_UNSET || indexOf(p, loop.first) == C.INDEX_UNSET) {
                queueLoop = null
            } else {
                loopMessages += p.createMessage { _, _ ->
                    // Repeat-one keeps the song itself going.
                    if (queueLoop == loop && p.repeatMode != Player.REPEAT_MODE_ONE) {
                        val first = indexOf(p, loop.first)
                        if (first != C.INDEX_UNSET) p.seekToDefaultPosition(first)
                    }
                }
                    .setPosition(last, C.TIME_END_OF_SOURCE)
                    .setLooper(main)
                    .setDeleteAfterDelivery(false)
                    .send()
            }
        }

        if (skips.isNotEmpty()) {
            for (i in 0 until p.mediaItemCount) {
                for (skip in skipsAt(p, i)) {
                    loopMessages += p.createMessage { _, _ ->
                        // Still skipped (a song loop may have started over it since).
                        if (p.currentMediaItemIndex == i && skip in skipsAt(p, i)) skipPast(p, skip)
                    }
                        .setPosition(i, skip.a)
                        .setLooper(main)
                        .setDeleteAfterDelivery(false)
                        .send()
                }
            }
        }
        publish()
    }

    // ---- volume ----

    /**
     * The song's ReplayGain as a volume: songs play at an even loudness. The
     * change lands when the song starts (a transition is reported as the new
     * song becomes audible), so its first few milliseconds may play at the
     * previous song's level.
     */
    private val volumeListener = object : Player.Listener {
        override fun onMediaItemTransition(item: MediaItem?, reason: Int) = applyVolume()
    }

    private fun applyVolume() {
        val p = player ?: return
        p.volume = fade * songVolume(p.currentMediaItem)
    }

    private fun songVolume(item: MediaItem?): Float {
        val extras = item?.mediaMetadata?.extras
        if (!normalize || extras == null || !extras.containsKey(GAIN)) return 1f
        var volume = 10.0.pow((extras.getDouble(GAIN) + PREAMP_DB) / 20)
        // Never past the level where the song's loudest sample clips.
        val peak = extras.getDouble(PEAK, 1.0)
        if (peak > 0) volume = minOf(volume, 1 / peak)
        return volume.coerceIn(0.0, 1.0).toFloat()
    }

    // ---- sleep timer ----

    private fun setSleep(at: Long, endOfSong: Boolean) {
        main.removeCallbacks(sleepTick)
        fade = 1f
        applyVolume()
        sleepAt = if (endOfSong) 0L else at
        sleepAtEnd = endOfSong
        player?.pauseAtEndOfMediaItems = endOfSong
        if (sleepAt > 0) main.post(sleepTick)
        publish()
    }

    private fun clearSleep() = setSleep(0L, false)

    /** Checks the clock rather than trusting a long delay, which stops while the
     * phone sleeps; turns the volume down over the last [FADE_MS]. */
    private val sleepTick = object : Runnable {
        override fun run() {
            val p = player ?: return
            val left = sleepAt - System.currentTimeMillis()
            when {
                sleepAt == 0L -> {}
                left <= 0 -> {
                    p.pause()
                    clearSleep()
                }
                left <= FADE_MS -> {
                    fade = left.toFloat() / FADE_MS
                    applyVolume()
                    main.postDelayed(this, 200)
                }
                else -> main.postDelayed(this, minOf(left - FADE_MS, 60_000L))
            }
        }
    }

    // ---- listening log ----

    private val listenListener = object : Player.Listener {
        override fun onIsPlayingChanged(isPlaying: Boolean) {
            if (isPlaying) {
                if (listenId == null) startListen(player?.currentMediaItem?.mediaId)
                // A song queued a while before it plays started listening now.
                if (listenedMs == 0L) listenStart = System.currentTimeMillis()
                playingSince = SystemClock.elapsedRealtime()
            } else {
                countListen()
                playingSince = -1L
            }
        }

        override fun onMediaItemTransition(item: MediaItem?, reason: Int) = startListen(item?.mediaId)
    }

    private fun countListen() {
        if (playingSince < 0) return
        val now = SystemClock.elapsedRealtime()
        listenedMs += now - playingSince
        playingSince = now
    }

    private fun startListen(id: String?) {
        finishListen()
        listenId = id
        listenStart = System.currentTimeMillis()
    }

    /** Logs the song being listened to: `<started, unix s>\t<track id>\t<ms>`. */
    private fun finishListen() {
        countListen()
        val id = listenId
        val ms = listenedMs
        listenId = null
        listenedMs = 0L
        if (id == null || ms < MIN_LISTEN_MS) return
        val line = "${listenStart / 1000}\t${id.substringBefore('.')}\t$ms\n"
        try {
            FileOutputStream(File(filesDir, "listens.log"), true).use { it.write(line.toByteArray()) }
        } catch (e: IOException) {
            Log.w(TAG, "logging a listen", e)
        }
    }

    // ---- session extras ----

    private fun publish() {
        val extras = Bundle()
        if (sleepAt > 0) extras.putLong("sleepAt", sleepAt)
        if (sleepAtEnd) extras.putBoolean("sleepAtEnd", true)
        songLoop?.let {
            extras.putString("songLoopId", it.id)
            extras.putLong("songLoopA", it.a)
            extras.putLong("songLoopB", it.b)
        }
        queueLoop?.let {
            extras.putString("queueLoopFirst", it.first)
            extras.putString("queueLoopLast", it.last)
        }
        val old = published
        val same = old != null && old.keySet() == extras.keySet() &&
            extras.keySet().all { @Suppress("DEPRECATION") (old.get(it) == extras.get(it)) }
        if (!same) {
            published = extras
            session?.setSessionExtras(extras)
        }
    }
}
