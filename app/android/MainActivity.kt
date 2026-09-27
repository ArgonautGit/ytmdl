package dev.dioxus.main

import android.os.Process

typealias BuildConfig = dev.nick.ytmdl.BuildConfig

/** Replaces the MainActivity dx generates (tools/patch-gradle-project copies it in). */
class MainActivity : WryActivity() {
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
