package dev.nick.ytmdl

import android.Manifest
import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat

/**
 * Keeps the app's process running while downloads do: without a foreground
 * service Android freezes a backgrounded app within seconds. The downloads
 * themselves run in Rust; this only holds the process and shows their progress.
 */
class DownloadService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        try {
            ServiceCompat.startForeground(
                this,
                YtmdlDownloads.NOTIFICATION,
                YtmdlDownloads.notification(this),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
            )
        } catch (e: RuntimeException) {
            Log.w(YtmdlDownloads.TAG, "downloads service refused", e)
            YtmdlDownloads.stopped()
            stopSelf()
        }
        return START_NOT_STICKY
    }

    /** Android 15+ limits data-sync services to 6 hours a day. */
    override fun onTimeout(startId: Int, fgsType: Int) {
        YtmdlDownloads.stopped()
        stopSelf()
    }
}

/** Called from Rust over JNI (src/platform/android.rs). */
object YtmdlDownloads {
    internal const val TAG = "ytmdl"
    internal const val NOTIFICATION = 2
    private const val CHANNEL = "downloads"
    private val main = Handler(Looper.getMainLooper())
    private var running = false
    private var title = ""
    private var text = ""

    /** Starts the service, or updates its notification. */
    @JvmStatic
    fun update(context: Context, title: String, text: String) {
        val app = context.applicationContext
        main.post {
            this.title = title
            this.text = text
            if (!running) {
                // Refused when the app isn't in the foreground (Android 12+); the
                // next update from the foreground tries again.
                try {
                    ContextCompat.startForegroundService(app, Intent(app, DownloadService::class.java))
                    running = true
                } catch (e: RuntimeException) {
                    Log.w(TAG, "starting the downloads service", e)
                }
            } else if (Build.VERSION.SDK_INT < 33 ||
                app.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
            ) {
                app.getSystemService(NotificationManager::class.java).notify(NOTIFICATION, notification(app))
            }
        }
    }

    @JvmStatic
    fun stop(context: Context) {
        val app = context.applicationContext
        main.post {
            if (running) {
                running = false
                app.stopService(Intent(app, DownloadService::class.java))
            }
        }
    }

    /** The service ended on its own. */
    internal fun stopped() {
        main.post { running = false }
    }

    /** Asks once for the notification permission (Android 13+), for download progress. */
    @JvmStatic
    fun askNotifications(activity: Activity) {
        if (Build.VERSION.SDK_INT < 33) return
        activity.runOnUiThread {
            if (activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
                activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 7)
            }
        }
    }

    internal fun notification(context: Context): Notification {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel(CHANNEL, "Downloads", NotificationManager.IMPORTANCE_LOW))
        val open = context.packageManager.getLaunchIntentForPackage(context.packageName)?.let {
            PendingIntent.getActivity(context, 1, it, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        }
        return NotificationCompat.Builder(context, CHANNEL)
            .setSmallIcon(R.drawable.ytmdl_notification)
            .setContentTitle(title)
            .setContentText(text)
            .setContentIntent(open)
            .setProgress(0, 0, true)
            .setOngoing(true)
            .setSilent(true)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
    }
}
