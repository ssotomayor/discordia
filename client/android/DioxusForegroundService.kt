package com.discordia.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Person
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
import android.net.Uri
import java.lang.ref.WeakReference
import org.json.JSONObject

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

    external fun onCallAction(token: String, code: Int)

    override fun onCreate() {
        super.onCreate()
        current = WeakReference(this)
    }

    private fun label(key: String, fallback: String): String = controls?.optString(key)?.takeIf { it.isNotEmpty() } ?: fallback

    private fun actionIntent(token: String, code: Int): PendingIntent {
        val intent = Intent(this, DioxusForegroundService::class.java)
            .setAction(ACTION_CONTROL)
            .setData(Uri.Builder().scheme("discordia-call").authority("control").appendPath(token).appendPath(code.toString()).build())
            .putExtra("token", token).putExtra("code", code)
        return PendingIntent.getService(this, code, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }

    private fun notification(types: Int): Notification {
        val microphone = types and ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE != 0
        val camera = types and ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA != 0
        val projection = types and ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION != 0
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        manager.createNotificationChannel(NotificationChannel(CHANNEL, label("channel", "Calls and sharing"), NotificationManager.IMPORTANCE_LOW))
        val launch = packageManager.getLaunchIntentForPackage(packageName)
        val builder = Notification.Builder(this, CHANNEL)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(listOfNotNull(
                label("voice", "Voice call").takeIf { microphone },
                label("camera", "Camera active").takeIf { camera },
                label("screen", "Sharing your screen").takeIf { projection },
                label("deafened_status", "Call audio muted").takeIf { microphone && controls?.optBoolean("deafened") == true },
                label("muted_status", "Microphone muted").takeIf { microphone && controls?.optBoolean("muted") == true && controls?.optBoolean("deafened") != true },
            ).joinToString(" · "))
            .setContentIntent(launch?.let { PendingIntent.getActivity(this, 0, it, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT) })
            .setSmallIcon(R.mipmap.ic_launcher)
            .setCategory(Notification.CATEGORY_CALL)
            .setVisibility(Notification.VISIBILITY_PRIVATE)
            .setOnlyAlertOnce(true)
            .setOngoing(true)
        val token = controls?.optString("token")?.takeIf { it.isNotEmpty() && it != "null" }
        if (microphone && token != null) {
            val muted = controls?.optBoolean("muted") == true
            val deafened = controls?.optBoolean("deafened") == true
            builder.addAction(Notification.Action.Builder(
                if (muted) R.drawable.ic_call_mic_off else R.drawable.ic_call_mic,
                label("mute", if (muted) "Unmute mic" else "Mute mic"),
                if (deafened) null else actionIntent(token, if (muted) 2 else 1),
            ).build())
            builder.addAction(Notification.Action.Builder(
                if (deafened) R.drawable.ic_call_sound_off else R.drawable.ic_call_sound,
                label("sound", if (deafened) "Unmute call audio" else "Mute call audio"),
                actionIntent(token, if (deafened) 4 else 3),
            ).build())
            val hangup = actionIntent(token, 5)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                builder.setStyle(Notification.CallStyle.forOngoingCall(
                    Person.Builder().setName(getString(R.string.app_name)).build(), hangup,
                ))
            } else {
                builder.addAction(Notification.Action.Builder(R.drawable.ic_call_end, label("hangup", "End call"), hangup).build())
            }
        }
        return builder.build()
    }

    private fun refreshNotification() {
        if (activeTypes != 0) {
            (getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager).notify(NOTIFICATION_ID, notification(activeTypes))
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_CONTROL) {
            if (activeTypes == 0) {
                stopSelf()
                return START_NOT_STICKY
            }
            val token = intent.getStringExtra("token")
            val code = intent.getIntExtra("code", 0)
            if (token != null && token == controls?.optString("token") && code in 1..5) {
                try {
                    onCallAction(token, code)
                } catch (error: UnsatisfiedLinkError) {
                    android.util.Log.w("DiscordiaCall", "Call controls are unavailable", error)
                }
            }
            return START_NOT_STICKY
        }
        @Suppress("DEPRECATION")
        val ready = intent?.getParcelableExtra<ResultReceiver>("ready")
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
        val notification = notification(types)
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
        if (current?.get() === this) current = null
        activeTypes = 0
        handler.removeCallbacks(renewWakeLock)
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        super.onDestroy()
    }

    companion object {
        private var current: WeakReference<DioxusForegroundService>? = null
        private var controls: JSONObject? = null
        private const val ACTION_CONTROL = "com.discordia.app.CALL_CONTROL"

        fun updateControls(payload: String) {
            if (payload.length > 8192) return
            val updated = if (payload == "null") null else try { JSONObject(payload) } catch (_: Exception) { return }
            Handler(Looper.getMainLooper()).post {
                controls = updated
                current?.get()?.refreshNotification()
            }
        }

        private const val CHANNEL = "discordia-call-media"
        private const val NOTIFICATION_ID = 41
    }
}
