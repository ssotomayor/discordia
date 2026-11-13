package com.discordia.app

import android.Manifest
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
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
    var resumed: Boolean
        get() = DioxusNotifier.resumed
        set(value) { DioxusNotifier.resumed = value }

    fun setAccount(value: String, labels: String) {
        if (account != value) cancelAll()
        if (account != value) viewedPeer = null
        account = value.takeIf { validKey(it) }
        DioxusNotifier.saveLabels(activity, labels)
        DioxusNotifier.ensureChannels(activity)
        if (account != null) askUnprompted()
    }

    // An account is the first moment there is anything to be told about.
    private fun askUnprompted() {
        if (Build.VERSION.SDK_INT < 33) return
        val granted = activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
        val asked = preferences.getBoolean("asked", false)
        if (!DioxusNotifier.asksUnprompted(DioxusNotifier.wanted(preferences), asked, Build.VERSION.SDK_INT, granted)) return
        preferences.edit().putBoolean("asked", true).apply()
        permissionPending = true
        activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), PERMISSION_REQUEST)
    }

    fun clearAccount(value: String) {
        if (account != value) return
        cancelAll()
        account = null
    }

    fun state(): String {
        if (permissionPending || settingsPending) return "pending"
        if (!DioxusNotifier.wanted(preferences)) return "disabled"
        val granted = Build.VERSION.SDK_INT < 33 || activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
        return if (granted && manager.areNotificationsEnabled() && manager.getNotificationChannel(DioxusNotifier.DM_CHANNEL)?.importance != NotificationManager.IMPORTANCE_NONE) "enabled" else "blocked"
    }

    fun enable(enabled: Boolean): String {
        val result = change(enabled)
        DioxusNotifier.reportPlatform(activity)
        return result
    }

    private fun change(enabled: Boolean): String {
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
                if (!globalBlocked) intent.putExtra(Settings.EXTRA_CHANNEL_ID, DioxusNotifier.DM_CHANNEL)
                activity.startActivity(intent)
            } catch (error: Exception) {
                settingsPending = false
                android.util.Log.w("Discordia", "Notification settings unavailable", error)
            }
        }
        return state()
    }

    fun permissionReturned() {
        permissionPending = false
        DioxusNotifier.reportPlatform(activity)
        activity.onDmNotificationSettingsChanged()
    }
    fun onResume() {
        resumed = true
        val returnedFromSettings = settingsPending
        settingsPending = false
        DioxusNotifier.reportPlatform(activity)
        if (returnedFromSettings) activity.onDmNotificationSettingsChanged()
        viewedPeer?.let { dismiss(it) }
    }

    fun show(payload: String) {
        val value = JSONObject(payload)
        val owner = value.getString("account")
        val peer = value.getString("peer")
        if (owner != account || !validKey(peer) || state() != "enabled") return
        // The visible app already plays its DM sound; Android owns background alerts.
        if (resumed) return
        DioxusNotifier.showDm(activity, value)
    }

    fun dismiss(peer: String) {
        viewedPeer = peer.takeIf { validKey(it) }
        if (resumed) account?.let { owner -> viewedPeer?.let { manager.cancel(DioxusNotifier.dmTag(owner, it), 0) } }
    }

    fun dismissRead(payload: String) {
        val value = JSONObject(payload)
        val owner = value.getString("account")
        val peer = value.getString("peer")
        if (owner != account || !validKey(peer)) return
        val tag = DioxusNotifier.dmTag(owner, peer)
        val notification = manager.activeNotifications.firstOrNull { it.tag == tag } ?: return
        val extras = notification.notification.extras
        if (extras.containsKey(DioxusNotifier.READ_TIME) && extras.getLong(DioxusNotifier.READ_TIME) <= value.getLong("at")) {
            manager.cancel(tag, 0)
        }
    }

    fun open(intent: Intent?) {
        val uri = intent?.data ?: return
        if (intent.action != Intent.ACTION_VIEW || uri.scheme != "discordia") return
        val parts = uri.pathSegments
        when (uri.authority) {
            "dm" -> if (parts.size == 2 && parts.all { validKey(it) }) {
                activity.onDmNotificationOpened(parts[0], parts[1])
            }
            "mention" -> if (parts.size == 5 && validKey(parts[0]) && DioxusNotifier.validOrigin(parts[1]) && parts.drop(2).all { DioxusNotifier.validId(it) }) {
                activity.onMentionNotificationOpened(parts[0], parts[1], parts[2], parts[3], parts[4])
            }
            else -> return
        }
        intent.data = null
    }

    private fun cancelAll() {
        DioxusNotifier.cancel(activity, DioxusNotifier.DM_PREFIX)
        DioxusNotifier.cancel(activity, DioxusNotifier.MENTION_PREFIX)
    }

    companion object {
        const val PERMISSION_REQUEST = 43
        private fun validKey(value: String) = DioxusNotifier.validKey(value)
    }
}
