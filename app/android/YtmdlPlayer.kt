package dev.nick.ytmdl

import android.content.ComponentName
import android.content.Context
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.session.MediaController
import androidx.media3.session.SessionToken
import com.google.common.util.concurrent.MoreExecutors
import java.io.File
import org.json.JSONArray
import org.json.JSONObject

/**
 * The app's end of playback, called from Rust over JNI (src/platform/android.rs).
 * Commands go to [PlaybackService] through a MediaController on the main thread;
 * every change comes back to Rust as a JSON snapshot through [nativeChanged].
 */
object YtmdlPlayer {
    private const val TAG = "ytmdl"
    private val main = Handler(Looper.getMainLooper())
    private var connecting = false
    private var controller: MediaController? = null
    /** Commands sent before the controller connected. */
    private val waiting = ArrayList<(MediaController) -> Unit>()
    private var error: String? = null

    /** Registered by Rust before [connect]. */
    @JvmStatic
    external fun nativeChanged(state: String)

    @JvmStatic
    fun connect(context: Context) {
        val app = context.applicationContext
        main.post {
            if (connecting) return@post
            connecting = true
            val token = SessionToken(app, ComponentName(app, PlaybackService::class.java))
            val future = MediaController.Builder(app, token).buildAsync()
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
     * Replaces the queue. [items] is a JSON array of
     * `{id, path, title, artist, album, art}` (art: a cover file path or "").
     */
    @JvmStatic
    fun setQueue(items: String, index: Int, positionMs: Long, play: Boolean) = command { c ->
        val list = JSONArray(items)
        val media = (0 until list.length()).map { i ->
            val o = list.getJSONObject(i)
            val meta = MediaMetadata.Builder()
                .setTitle(o.getString("title"))
                .setArtist(o.optString("artist").ifEmpty { null })
                .setAlbumTitle(o.optString("album").ifEmpty { null })
                .setArtworkUri(o.optString("art").ifEmpty { null }?.let { Uri.fromFile(File(it)) })
                .setIsPlayable(true)
                .setIsBrowsable(false)
                .build()
            MediaItem.Builder()
                .setMediaId(o.getString("id"))
                .setUri(Uri.fromFile(File(o.getString("path"))))
                .setMediaMetadata(meta)
                .build()
        }
        error = null
        c.setMediaItems(media, index, positionMs)
        c.prepare()
        c.playWhenReady = play
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
        try {
            nativeChanged(state.toString())
        } catch (e: UnsatisfiedLinkError) {
            Log.w(TAG, "player state not delivered", e)
        }
    }
}
