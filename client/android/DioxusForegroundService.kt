package com.discordia.app

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
import android.os.ResultReceiver
import android.os.Handler
import android.os.Looper
import android.os.PowerManager

class DioxusForegroundService : Service() {
    private val handler = Handler(Looper.getMainLooper())
    private var wakeLock: PowerManager.WakeLock? = null
    private var activeTypes = 0
    private val renewWakeLock = object : Runnable {
        override fun run() {
            wakeLock?.acquire(10 * 60 * 1000L)
            handler.postDelayed(this, 5 * 60 * 1000L)
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        @Suppress("DEPRECATION")
        val ready = intent?.getParcelableExtra<ResultReceiver>("ready")
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, "Calls and sharing", NotificationManager.IMPORTANCE_LOW),
        )
        val microphone = intent?.getBooleanExtra("microphone", false) == true
        val camera = intent?.getBooleanExtra("camera", false) == true
        val projection = intent?.getBooleanExtra("projection", false) == true
        var types = 0
        if (microphone) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
        if (camera) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA
        if (projection) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
        if (types == 0) {
            stopSelf()
            return START_NOT_STICKY
        }
        val launch = packageManager.getLaunchIntentForPackage(packageName)
        val notification = Notification.Builder(this, CHANNEL)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(listOfNotNull(
                "Voice call".takeIf { microphone },
                "Camera active".takeIf { camera },
                "Sharing your screen".takeIf { projection },
            ).joinToString(" · "))
            .setContentIntent(launch?.let {
                PendingIntent.getActivity(this, 0, it, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
            })
            .setSmallIcon(R.mipmap.ic_launcher)
            .setOngoing(true)
            .build()
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                startForeground(
                    NOTIFICATION_ID,
                    notification,
                    types,
                )
            } else {
                startForeground(NOTIFICATION_ID, notification)
            }
            activeTypes = types
            if (wakeLock == null) {
                wakeLock = (getSystemService(Context.POWER_SERVICE) as PowerManager)
                    .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "discordia:call-media")
                    .apply { setReferenceCounted(false) }
                renewWakeLock.run()
            }
            ready?.send(1, null)
        } catch (error: Exception) {
            ready?.send(-1, null)
            // A rejected camera/projection addition must not drop an existing call.
            if (activeTypes == 0) stopSelf()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        handler.removeCallbacks(renewWakeLock)
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        super.onDestroy()
    }

    companion object {
        private const val CHANNEL = "discordia-call-media"
        private const val NOTIFICATION_ID = 41
    }
}
