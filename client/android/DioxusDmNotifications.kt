package com.discordia.app

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import dev.dioxus.main.MainActivity
import org.json.JSONObject

class DioxusDmNotifications(private val activity: MainActivity) {
    private val manager = activity.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
    private val preferences = activity.getSharedPreferences("dm-notifications", Context.MODE_PRIVATE)
    private var account: String? = null
    private var viewedPeer: String? = null
    private var permissionPending = false
    private var settingsPending = false
    var resumed = false

    fun setAccount(value: String, channelName: String) {
        if (account != value) cancelAll()
        if (account != value) viewedPeer = null
        account = value.takeIf { validKey(it) }
        manager.createNotificationChannel(NotificationChannel(CHANNEL, channelName, NotificationManager.IMPORTANCE_HIGH))
        val quiet = NotificationChannel(QUIET_CHANNEL, channelName, NotificationManager.IMPORTANCE_LOW)
        quiet.setSound(null, null)
        quiet.enableVibration(false)
        manager.createNotificationChannel(quiet)
    }

    fun clearAccount(value: String) {
        if (account != value) return
        cancelAll()
        account = null
    }

    fun state(): String {
        if (permissionPending || settingsPending) return "pending"
        if (!preferences.getBoolean("enabled", false)) return "disabled"
        val granted = Build.VERSION.SDK_INT < 33 || activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
        return if (granted && manager.areNotificationsEnabled() && manager.getNotificationChannel(CHANNEL)?.importance != NotificationManager.IMPORTANCE_NONE) "enabled" else "blocked"
    }

    fun enable(enabled: Boolean): String {
        preferences.edit().putBoolean("enabled", enabled).apply()
        if (!enabled) {
            cancelAll()
            return state()
        }
        if (Build.VERSION.SDK_INT >= 33 && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            if (!preferences.getBoolean("asked", false) || activity.shouldShowRequestPermissionRationale(Manifest.permission.POST_NOTIFICATIONS)) {
                preferences.edit().putBoolean("asked", true).apply()
                permissionPending = true
                activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), PERMISSION_REQUEST)
                return state()
            }
        }
        if (state() == "blocked") {
            settingsPending = true
            try {
                val globalBlocked = !manager.areNotificationsEnabled() || (Build.VERSION.SDK_INT >= 33 && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED)
                val intent = Intent(if (globalBlocked) Settings.ACTION_APP_NOTIFICATION_SETTINGS else Settings.ACTION_CHANNEL_NOTIFICATION_SETTINGS)
                    .putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName)
                if (!globalBlocked) intent.putExtra(Settings.EXTRA_CHANNEL_ID, CHANNEL)
                activity.startActivity(intent)
            } catch (error: Exception) {
                settingsPending = false
                android.util.Log.w("Discordia", "Notification settings unavailable", error)
            }
        }
        return state()
    }

    fun permissionReturned() { permissionPending = false }
    fun onResume() {
        resumed = true
        settingsPending = false
        viewedPeer?.let { dismiss(it) }
    }

    fun show(payload: String) {
        val value = JSONObject(payload)
        val owner = value.getString("account")
        val peer = value.getString("peer")
        if (owner != account || !validKey(peer) || state() != "enabled") return
        // The visible app already plays its DM sound; Android owns background alerts.
        if (resumed) return
        val tag = tag(owner, peer)
        val launch = activity.packageManager.getLaunchIntentForPackage(activity.packageName) ?: return
        launch.action = Intent.ACTION_VIEW
        launch.data = Uri.Builder().scheme("discordia").authority("dm").appendPath(owner).appendPath(peer).build()
        launch.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        val pending = PendingIntent.getActivity(activity, 0, launch, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        val text = value.getString("message")
        val channel = if (value.optBoolean("silent", false)) QUIET_CHANNEL else CHANNEL
        val publicVersion = Notification.Builder(activity, channel)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle(activity.getString(R.string.app_name))
            .setContentText(text)
            .build()
        val notification = Notification.Builder(activity, channel)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle(value.getString("sender").take(100))
            .setContentText(text)
            .setCategory(Notification.CATEGORY_MESSAGE)
            .setVisibility(Notification.VISIBILITY_PRIVATE)
            .setPublicVersion(publicVersion)
            .setContentIntent(pending)
            .setAutoCancel(true)
            .setOnlyAlertOnce(false)
            .build()
        try {
            manager.notify(tag, 0, notification)
            manager.activeNotifications.filter { it.tag?.startsWith(PREFIX) == true }
                .sortedByDescending { it.postTime }.drop(20)
                .forEach { manager.cancel(it.tag, it.id) }
        } catch (error: SecurityException) {
            android.util.Log.w("Discordia", "DM notification permission changed", error)
        }
    }

    fun dismiss(peer: String) {
        viewedPeer = peer.takeIf { validKey(it) }
        if (resumed) account?.let { owner -> viewedPeer?.let { manager.cancel(tag(owner, it), 0) } }
    }

    fun open(intent: Intent?) {
        val uri = intent?.data ?: return
        if (intent.action != Intent.ACTION_VIEW || uri.scheme != "discordia" || uri.authority != "dm") return
        val parts = uri.pathSegments
        if (parts.size == 2 && parts.all { validKey(it) }) {
            activity.onDmNotificationOpened(parts[0], parts[1])
        }
        intent.data = null
    }

    private fun cancelAll() {
        manager.activeNotifications.filter { it.tag?.startsWith(PREFIX) == true }
            .forEach { manager.cancel(it.tag, it.id) }
    }

    companion object {
        const val PERMISSION_REQUEST = 43
        private const val CHANNEL = "discordia-dm-v1"
        private const val QUIET_CHANNEL = "discordia-dm-quiet-v1"
        private const val PREFIX = "discordia-dm:"
        private fun validKey(value: String) = value.length == 64 && value.all { it in '0'..'9' || it in 'a'..'f' }
        private fun tag(account: String, peer: String) = "$PREFIX$account:$peer"
    }
}
