package dev.dioxus.main

import android.content.Intent
import android.os.Bundle
import android.os.Process
import dev.nick.ytmdl.YtmdlShare

typealias BuildConfig = dev.nick.ytmdl.BuildConfig

/**
 * Replaces the MainActivity dx generates (tools/patch-gradle-project copies it
 * in). The manifest makes it singleTask, so a link shared to the app reaches
 * this one activity through [onNewIntent] rather than starting a second one.
 */
class MainActivity : WryActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // A restored activity carries the intent it was first started with.
        if (savedInstanceState == null) YtmdlShare.received(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        YtmdlShare.received(intent)
    }

    /** Back on the root page leaves the app running, so downloads continue. */
    @Deprecated("Back presses still arrive here when targeting SDK 34")
    override fun onBackPressed() {
        moveTaskToBack(true)
    }

    /**
     * The Rust side starts once per process and can't attach to a second activity,
     * so the process ends with the activity. Playback runs in the separate
     * ":player" process and continues; unfinished downloads resume on next start.
     */
    override fun onDestroy() {
        super.onDestroy()
        Process.killProcess(Process.myPid())
    }
}
