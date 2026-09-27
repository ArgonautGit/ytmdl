package dev.nick.ytmdl

import android.app.PendingIntent
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
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService
import androidx.media3.session.SessionCommand
import androidx.media3.session.SessionResult
import com.google.common.util.concurrent.Futures
import com.google.common.util.concurrent.ListenableFuture
import java.io.File
import java.io.FileOutputStream
import java.io.IOException

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
 * Every song played is logged to `files/listens.log` for the app's listening
 * stats (see crates/library/src/listens.rs), since the app may not be running.
 */
class PlaybackService : MediaSessionService() {
    companion object {
        /** Args: "id" (media id), "a" and "b" (ms); no "id" clears. */
        const val SONG_LOOP = "dev.nick.ytmdl.SONG_LOOP"
        /** Args: "first" and "last" (media ids); none clears. */
        const val QUEUE_LOOP = "dev.nick.ytmdl.QUEUE_LOOP"
        /** Args: "at" (wall-clock ms to pause at) or "endOfSong"; neither clears. */
        const val SLEEP = "dev.nick.ytmdl.SLEEP"

        private const val TAG = "ytmdl"
        /** The sleep timer fades the music out over its last seconds. */
        private const val FADE_MS = 15_000L
        /** Shorter listens (skips) aren't logged. */
        private const val MIN_LISTEN_MS = 1_000L
    }

    private data class SongLoop(val id: String, val a: Long, val b: Long)
    private data class QueueLoop(val first: String, val last: String)

    private var session: MediaSession? = null
    private var player: ExoPlayer? = null
    private var songLoop: SongLoop? = null
    private var queueLoop: QueueLoop? = null
    /** Positions are per queue index, so these are rebuilt when the queue changes. */
    private val loopMessages = ArrayList<PlayerMessage>()
    private var currentId: String? = null
    private var published: Bundle? = null

    private val main = Handler(Looper.getMainLooper())
    /** Wall-clock ms the sleep timer pauses at; 0 when not set. */
    private var sleepAt = 0L
    /** The sleep timer pauses at the end of the song instead. */
    private var sleepAtEnd = false

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
        this.player = player

        setMediaNotificationProvider(
            DefaultMediaNotificationProvider.Builder(this).build().apply { setSmallIcon(R.drawable.ytmdl_notification) }
        )
        val builder = MediaSession.Builder(this, player).setCallback(callback)
        // Tapping the notification brings the app back like the launcher does.
        packageManager.getLaunchIntentForPackage(packageName)?.let { open ->
            val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            builder.setSessionActivity(PendingIntent.getActivity(this, 0, open, flags))
        }
        session = builder.build()
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? = session

    /** Swiping the app away keeps music that is playing; a paused player goes. */
    override fun onTaskRemoved(rootIntent: Intent?) {
        val player = session?.player
        if (player == null || !player.playWhenReady || player.mediaItemCount == 0) {
            stopSelf()
        }
    }

    override fun onDestroy() {
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

    private val callback = object : MediaSession.Callback {
        override fun onConnect(
            session: MediaSession,
            controller: MediaSession.ControllerInfo,
        ): MediaSession.ConnectionResult {
            val result = MediaSession.ConnectionResult.AcceptedResultBuilder(session)
            if (controller.packageName == packageName) {
                result.setAvailableSessionCommands(
                    MediaSession.ConnectionResult.DEFAULT_SESSION_COMMANDS.buildUpon()
                        .add(SessionCommand(SONG_LOOP, Bundle.EMPTY))
                        .add(SessionCommand(QUEUE_LOOP, Bundle.EMPTY))
                        .add(SessionCommand(SLEEP, Bundle.EMPTY))
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
            }
            armLoops()
            return Futures.immediateFuture(SessionResult(SessionResult.RESULT_SUCCESS))
        }
    }

    private val loopListener = object : Player.Listener {
        override fun onTimelineChanged(timeline: Timeline, reason: Int) {
            if (reason == Player.TIMELINE_CHANGE_REASON_PLAYLIST_CHANGED) armLoops()
        }

        override fun onMediaItemTransition(item: MediaItem?, reason: Int) {
            val left = currentId
            currentId = item?.mediaId
            // A song loop belongs to its song: skipping away ends it.
            if (songLoop != null && item?.mediaId != songLoop?.id) {
                songLoop = null
                armLoops()
            }
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

    /** Recreates the messages behind the loops, dropping loops whose songs left the queue. */
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
        publish()
    }

    // ---- sleep timer ----

    private fun setSleep(at: Long, endOfSong: Boolean) {
        main.removeCallbacks(sleepTick)
        player?.volume = 1f
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
                    p.volume = left.toFloat() / FADE_MS
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
