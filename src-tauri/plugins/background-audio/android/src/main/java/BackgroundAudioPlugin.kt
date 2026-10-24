package io.github.acroath.quietoverworld.background

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin

@InvokeArg
class NoticeArgs {
    var title: String = "Quiet Overworld"
    var text: String = ""
}

@TauriPlugin
class BackgroundAudioPlugin(private val activity: Activity) : Plugin(activity) {
    private var askedForNotifications = false

    /** Starts the foreground service, or updates its notification. */
    @Command
    fun start(invoke: Invoke) {
        val args = invoke.parseArgs(NoticeArgs::class.java)
        try {
            if (PlaybackService.running) {
                PlaybackService.update(activity, args.title, args.text)
            } else {
                askForNotifications()
                val intent = Intent(activity, PlaybackService::class.java)
                    .putExtra(PlaybackService.EXTRA_TITLE, args.title)
                    .putExtra(PlaybackService.EXTRA_TEXT, args.text)
                ContextCompat.startForegroundService(activity, intent)
            }
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: "Could not keep playing in the background")
        }
    }

    /** Lets Android stop the app again, e.g. while music is paused. */
    @Command
    fun stop(invoke: Invoke) {
        activity.stopService(Intent(activity, PlaybackService::class.java))
        invoke.resolve()
    }

    // Without this permission (Android 13+) the service still runs, but its
    // notification, and with it the Quit button, is hidden.
    private fun askForNotifications() {
        if (askedForNotifications || Build.VERSION.SDK_INT < 33) return
        askedForNotifications = true
        val permission = Manifest.permission.POST_NOTIFICATIONS
        if (ContextCompat.checkSelfPermission(activity, permission) != PackageManager.PERMISSION_GRANTED) {
            activity.runOnUiThread {
                ActivityCompat.requestPermissions(activity, arrayOf(permission), 0x51)
            }
        }
    }
}
