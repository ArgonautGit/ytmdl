package dev.nick.ytmdl

import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.util.Log

/**
 * Links shared to the app ("Share" in YouTube Music, then ytmdl). MainActivity
 * hands over each ACTION_SEND intent; the text goes to Rust through
 * [nativeShared] once Rust is listening (src/platform/android.rs), and waits
 * until then when the share is what started the app.
 */
object YtmdlShare {
    private const val TAG = "ytmdl"
    private val main = Handler(Looper.getMainLooper())
    private var listening = false
    private val waiting = ArrayList<String>()

    /** Registered by Rust before [listen]. */
    @JvmStatic
    external fun nativeShared(text: String)

    /** From MainActivity, for the intent that started it and each new one. */
    fun received(intent: Intent?) {
        if (intent?.action != Intent.ACTION_SEND) return
        // Reopening the app from recents repeats the intent that started it.
        if (intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY != 0) return
        val text = intent.getStringExtra(Intent.EXTRA_TEXT) ?: return
        main.post { if (listening) deliver(text) else waiting.add(text) }
    }

    @JvmStatic
    fun listen() {
        main.post {
            listening = true
            waiting.forEach(::deliver)
            waiting.clear()
        }
    }

    private fun deliver(text: String) {
        try {
            nativeShared(text)
        } catch (e: UnsatisfiedLinkError) {
            Log.w(TAG, "shared link not delivered", e)
        }
    }
}
