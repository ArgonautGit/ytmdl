package dev.nick.ytmdl

import android.app.PendingIntent
import android.content.Intent
import android.os.Bundle
import android.os.Looper
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

/**
 * Plays the queue with the media notification, lock screen and headset
 * controls. Runs in its own ":player" process, so music keeps going when the
 * app's process ends (see MainActivity).
 *
 * A-B loops live here too, so they hold with the screen off: a song loop seeks
 * back to A on reaching B, a queue loop goes back to its first song when its
 * last one ends. The app sets them with custom commands and reads them back
 * from the session extras.
 */
class PlaybackService : MediaSessionService() {
    companion object {
        /** Args: "id" (media id), "a" and "b" (ms); no "id" clears. */
        const val SONG_LOOP = "dev.nick.ytmdl.SONG_LOOP"
        /** Args: "first" and "last" (media ids); none clears. */
        const val QUEUE_LOOP = "dev.nick.ytmdl.QUEUE_LOOP"
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

        override fun onPlaybackStateChanged(state: Int) {
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
                loopMessages += p.createMessage { _, _ -> if (songLoop == loop) p.seekTo(loop.a) }
                    .setPosition(i, loop.b)
                    .setLooper(main)
                    .setDeleteAfterDelivery(false)
                    .send()
            }
        }

        queueLoop?.let { loop ->
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
        publishLoops()
    }

    private fun publishLoops() {
        val extras = Bundle()
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
