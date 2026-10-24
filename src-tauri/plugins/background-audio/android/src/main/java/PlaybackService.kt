package io.github.acroath.quietoverworld.background

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.Process
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat

/**
 * A media-playback foreground service. The music itself plays in the app's
 * Rust code; this service only tells Android that the app is playing audio,
 * so it is not frozen or killed while the screen is off.
 */
class PlaybackService : Service() {
    companion object {
        const val EXTRA_TITLE = "title"
        const val EXTRA_TEXT = "text"
        private const val ACTION_QUIT = "io.github.acroath.quietoverworld.QUIT"
        private const val CHANNEL = "playback"
        private const val NOTIFICATION_ID = 1

        @Volatile
        var running = false
            private set

        fun update(context: Context, title: String, text: String) {
            context.getSystemService(NotificationManager::class.java)
                ?.notify(NOTIFICATION_ID, build(context, title, text))
        }

        private fun build(context: Context, title: String, text: String): Notification {
            val flags = PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
            val open = context.packageManager.getLaunchIntentForPackage(context.packageName)
                ?.let { PendingIntent.getActivity(context, 0, it, flags) }
            val quit = PendingIntent.getService(
                context,
                1,
                Intent(context, PlaybackService::class.java).setAction(ACTION_QUIT),
                flags,
            )
            return NotificationCompat.Builder(context, CHANNEL)
                .setSmallIcon(R.drawable.ic_stat_note)
                .setContentTitle(title)
                .setContentText(text)
                .setContentIntent(open)
                .addAction(0, "Quit", quit)
                .setOngoing(true)
                .setSilent(true)
                .setShowWhen(false)
                .setCategory(NotificationCompat.CATEGORY_SERVICE)
                .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
                .build()
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        running = true
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(CHANNEL, "Playing music", NotificationManager.IMPORTANCE_LOW)
            channel.setShowBadge(false)
            getSystemService(NotificationManager::class.java)?.createNotificationChannel(channel)
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_QUIT) {
            // Like "Quit" in the desktop tray: end the app, music included.
            ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
            stopSelf()
            Process.killProcess(Process.myPid())
            return START_NOT_STICKY
        }
        val notification = build(
            this,
            intent?.getStringExtra(EXTRA_TITLE) ?: "Quiet Overworld",
            intent?.getStringExtra(EXTRA_TEXT) ?: "",
        )
        val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
        } else {
            0
        }
        ServiceCompat.startForeground(this, NOTIFICATION_ID, notification, type)
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        super.onDestroy()
    }
}
