package dev.nick.ytmdl

import android.content.ComponentName
import android.content.Context
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.session.MediaController
import androidx.media3.session.SessionCommand
import androidx.media3.session.SessionToken
import com.google.common.util.concurrent.MoreExecutors
import java.io.File
import org.json.JSONArray
import org.json.JSONObject

/**
 * The app's end of playback, called from Rust over JNI (src/platform/android.rs).
 * Commands go to [PlaybackService] through a MediaController on the main thread;
 * every change comes back to Rust as a JSON snapshot through [nativeChanged].
 *
 * Media ids are "<track id>.<n>", unique within the queue, so one song can be
 * queued twice and loops can name queue entries.
 */
object YtmdlPlayer {
    private const val TAG = "ytmdl"
    private val main = Handler(Looper.getMainLooper())
    private var connecting = false
    private var controller: MediaController? = null
    /** For the cover art URIs (ArtProvider). */
    private var app: Context? = null
    /** Commands sent before the controller connected. */
    private val waiting = ArrayList<(MediaController) -> Unit>()
    private var error: String? = null

    /** Registered by Rust before [connect]. */
    @JvmStatic
    external fun nativeChanged(state: String)

    @JvmStatic
    fun connect(context: Context) {
        val app = context.applicationContext
        this.app = app
        main.post {
            if (connecting) return@post
            connecting = true
            val token = SessionToken(app, ComponentName(app, PlaybackService::class.java))
            val future = MediaController.Builder(app, token)
                .setListener(object : MediaController.Listener {
                    override fun onExtrasChanged(controller: MediaController, extras: Bundle) = publish()
                })
                .buildAsync()
            future.addListener({
                val c = try {
                    future.get()
                } catch (e: Exception) {
                    Log.w(TAG, "connecting to the player", e)
                    connecting = false
                    return@addListener
                }
                controller = c
                c.addListener(object : Player.Listener {
                    override fun onPlayerError(e: PlaybackException) {
                        error = e.message ?: e.errorCodeName
                    }

                    override fun onMediaItemTransition(item: MediaItem?, reason: Int) {
                        error = null
                    }

                    override fun onIsPlayingChanged(isPlaying: Boolean) {
                        if (isPlaying) error = null
                    }

                    override fun onEvents(player: Player, events: Player.Events) = publish()
                })
                waiting.forEach { it(c) }
                waiting.clear()
                publish()
            }, MoreExecutors.directExecutor())
        }
    }

    private fun command(action: (MediaController) -> Unit) {
        main.post {
            val c = controller
            if (c != null) action(c) else waiting.add(action)
        }
    }

    /**
     * `[{id, path, title, artist, album, art, gain, peak}]`, art being a cover
     * file path or "", gain (dB) and peak the song's ReplayGain or null.
     */
    private fun mediaItems(json: String): List<MediaItem> {
        val list = JSONArray(json)
        return (0 until list.length()).map { i ->
            val o = list.getJSONObject(i)
            // Served by ArtProvider, so the car and the system can show it too.
            val art = o.optString("art").ifEmpty { null }?.let { path ->
                app?.let { ArtProvider.uri(it, File(path).name) } ?: Uri.fromFile(File(path))
            }
            Browse.queueItem(
                o.getString("id"),
                o.getString("path"),
                o.getString("title"),
                o.optString("artist"),
                o.optString("album"),
                art,
                if (o.isNull("gain")) null else o.getDouble("gain"),
                o.optDouble("peak", 1.0),
            )
        }
    }

    /** Replaces the queue (loops go with the old entries). */
    @JvmStatic
    fun setQueue(items: String, index: Int, positionMs: Long, play: Boolean) = command { c ->
        error = null
        c.setMediaItems(mediaItems(items), index, positionMs)
        c.prepare()
        c.playWhenReady = play
    }

    /** Adds songs after the current one ([next]) or at the end; starts a queue if there is none. */
    @JvmStatic
    fun insert(items: String, next: Boolean) = command { c ->
        val media = mediaItems(items)
        if (c.mediaItemCount == 0) {
            c.setMediaItems(media)
            c.prepare()
            c.play()
        } else {
            c.addMediaItems(if (next) c.currentMediaItemIndex + 1 else c.mediaItemCount, media)
        }
    }

    /** Moves the entry at [from] to [to] (queue positions). */
    @JvmStatic
    fun move(from: Int, to: Int) = command { c ->
        if (from in 0 until c.mediaItemCount && to in 0 until c.mediaItemCount) c.moveMediaItem(from, to)
    }

    /** Removes the entry with media id [key], or every entry of track [key]. */
    @JvmStatic
    fun remove(key: String) = command { c ->
        for (i in c.mediaItemCount - 1 downTo 0) {
            val id = c.getMediaItemAt(i).mediaId
            if (id == key || id.startsWith("$key.")) c.removeMediaItem(i)
        }
    }

    @JvmStatic
    fun play() = command { c ->
        when (c.playbackState) {
            Player.STATE_ENDED -> c.seekToDefaultPosition(0)
            Player.STATE_IDLE -> c.prepare()
        }
        c.play()
    }

    @JvmStatic
    fun pause() = command { it.pause() }

    @JvmStatic
    fun next() = command { it.seekToNextMediaItem() }

    /** Restarts the song, or goes to the previous one near its start. */
    @JvmStatic
    fun previous() = command { it.seekToPrevious() }

    @JvmStatic
    fun seekTo(positionMs: Long) = command { it.seekTo(positionMs) }

    @JvmStatic
    fun skipTo(index: Int) = command { c ->
        c.seekToDefaultPosition(index)
        c.play()
    }

    /** Player.REPEAT_MODE_OFF, _ONE or _ALL. */
    @JvmStatic
    fun setRepeat(mode: Int) = command { it.repeatMode = mode }

    /** Loops entry [id] between [a] and [b] ms; an empty [id] clears the loop. */
    @JvmStatic
    fun setSongLoop(id: String, a: Long, b: Long) = command { c ->
        val args = Bundle()
        if (id.isNotEmpty()) {
            args.putString("id", id)
            args.putLong("a", a)
            args.putLong("b", b)
        }
        c.sendCustomCommand(SessionCommand(PlaybackService.SONG_LOOP, Bundle.EMPTY), args)
    }

    /** Goes back to entry [first] whenever entry [last] ends; empty ids clear the loop. */
    @JvmStatic
    fun setQueueLoop(first: String, last: String) = command { c ->
        val args = Bundle()
        if (first.isNotEmpty() && last.isNotEmpty()) {
            args.putString("first", first)
            args.putString("last", last)
        }
        c.sendCustomCommand(SessionCommand(PlaybackService.QUEUE_LOOP, Bundle.EMPTY), args)
    }

    /** Pauses at wall-clock ms [at], or at the end of the song; neither clears. */
    @JvmStatic
    fun setSleep(at: Long, endOfSong: Boolean) = command { c ->
        val args = Bundle()
        if (endOfSong) args.putBoolean("endOfSong", true) else if (at > 0) args.putLong("at", at)
        c.sendCustomCommand(SessionCommand(PlaybackService.SLEEP, Bundle.EMPTY), args)
    }

    /** Evens out loudness with the songs' ReplayGain, or plays them as they are. */
    @JvmStatic
    fun setNormalize(on: Boolean) = command { c ->
        val args = Bundle()
        args.putBoolean("on", on)
        c.sendCustomCommand(SessionCommand(PlaybackService.NORMALIZE, Bundle.EMPTY), args)
    }

    private fun publish() {
        val c = controller ?: return
        val ids = JSONArray()
        for (i in 0 until c.mediaItemCount) ids.put(c.getMediaItemAt(i).mediaId)
        val state = JSONObject()
            .put("playing", c.isPlaying)
            .put("playWhenReady", c.playWhenReady)
            .put("buffering", c.playbackState == Player.STATE_BUFFERING)
            .put("ended", c.playbackState == Player.STATE_ENDED)
            .put("index", c.currentMediaItemIndex)
            .put("positionMs", c.currentPosition)
            .put("durationMs", if (c.duration == C.TIME_UNSET) -1 else c.duration)
            .put("repeat", c.repeatMode)
            .put("ids", ids)
            .put("error", error ?: JSONObject.NULL)
        val extras = c.sessionExtras
        extras.getString("songLoopId")?.let { id ->
            state.put(
                "songLoop",
                JSONObject().put("id", id).put("a", extras.getLong("songLoopA")).put("b", extras.getLong("songLoopB")),
            )
        }
        if (extras.containsKey("sleepAt")) state.put("sleepAt", extras.getLong("sleepAt"))
        if (extras.getBoolean("sleepAtEnd")) state.put("sleepAtEnd", true)
        extras.getString("queueLoopFirst")?.let { first ->
            state.put("queueLoop", JSONObject().put("first", first).put("last", extras.getString("queueLoopLast")))
        }
        try {
            nativeChanged(state.toString())
        } catch (e: UnsatisfiedLinkError) {
            Log.w(TAG, "player state not delivered", e)
        }
    }
}
