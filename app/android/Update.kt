package dev.nick.ytmdl

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.core.content.IntentCompat
import java.io.File
import kotlin.concurrent.thread

/**
 * Installs an app update, an APK Rust downloaded and checked (src/ui/app_update.rs),
 * through a PackageInstaller session. Android 12+ installs it without asking
 * once "Install unknown apps" is allowed for ytmdl, since the app updates
 * itself; otherwise [UpdateReceiver] shows Android's confirmation. A finished
 * install ends the app's processes. Called from Rust over JNI
 * (src/platform/android.rs).
 */
object YtmdlUpdate {
    internal const val TAG = "ytmdl"
    private val main = Handler(Looper.getMainLooper())

    /**
     * Registered by Rust. An install that did not replace the app:
     * a [PackageInstaller] status (STATUS_FAILURE_*) and Android's message.
     */
    @JvmStatic
    external fun nativeStatus(status: Int, message: String)

    @JvmStatic
    fun install(activity: Activity, path: String) {
        val app = activity.applicationContext
        // Copying the APK into the session takes a moment; not on the caller's thread.
        thread(name = "ytmdl-install") {
            try {
                val apk = File(path)
                val installer = app.packageManager.packageInstaller
                val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
                params.setAppPackageName(app.packageName)
                params.setSize(apk.length())
                if (Build.VERSION.SDK_INT >= 31) {
                    params.setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
                }
                val id = installer.createSession(params)
                installer.openSession(id).use { session ->
                    try {
                        session.openWrite("ytmdl.apk", 0, apk.length()).use { out ->
                            apk.inputStream().use { it.copyTo(out) }
                            session.fsync(out)
                        }
                        val flags = PendingIntent.FLAG_UPDATE_CURRENT or
                            (if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0)
                        val done = PendingIntent.getBroadcast(app, id, Intent(app, UpdateReceiver::class.java), flags)
                        session.commit(done.intentSender)
                    } catch (e: Exception) {
                        session.abandon()
                        throw e
                    }
                }
            } catch (e: Exception) {
                Log.w(TAG, "installing the update", e)
                report(PackageInstaller.STATUS_FAILURE, e.message ?: e.toString())
            }
        }
    }

    internal fun report(status: Int, message: String) {
        main.post {
            try {
                nativeStatus(status, message)
            } catch (e: UnsatisfiedLinkError) {
                Log.w(TAG, "update status $status not delivered: $message", e)
            }
        }
    }
}

/** Where the install session reports; declared by tools/patch-gradle-project. */
class UpdateReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
        val message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE) ?: ""
        when (status) {
            // Android wants the user to confirm: show its dialog.
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                val confirm = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_INTENT, Intent::class.java)
                if (confirm == null) {
                    YtmdlUpdate.report(PackageInstaller.STATUS_FAILURE, "no confirmation to show")
                    return
                }
                confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                try {
                    context.startActivity(confirm)
                } catch (e: RuntimeException) {
                    Log.w(YtmdlUpdate.TAG, "showing the install confirmation", e)
                    YtmdlUpdate.report(PackageInstaller.STATUS_FAILURE, e.message ?: e.toString())
                }
            }
            // The new build replaced this one; its processes are ending.
            PackageInstaller.STATUS_SUCCESS -> Log.i(YtmdlUpdate.TAG, "update installed")
            else -> YtmdlUpdate.report(status, message)
        }
    }
}
