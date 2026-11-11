package com.discordia.app

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging
import org.json.JSONObject

// Context only: a push may start a process in which no Activity ever runs.
object DioxusNotifier {
    const val DM_CHANNEL = "discordia-dm-v1"
    const val QUIET_CHANNEL = "discordia-dm-quiet-v1"
    const val MENTION_CHANNEL = "discordia-mention-v1"
    const val QUIET_MENTION_CHANNEL = "discordia-mention-quiet-v1"
    const val DM_PREFIX = "discordia-dm:"
    const val MENTION_PREFIX = "discordia-mention:"
    const val READ_TIME = "discordia-dm-at"
    private const val PING_TAG = "discordia-ping"
    private const val SHOWN_LIMIT = 20
    private const val DEDUP_CAP = 64
    private const val SEEN = "seen"
    private val FALLBACK = mapOf("dm" to "Private messages", "mention" to "Mentions", "ping" to "Notifications are working")
    private val ID = Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")

    @Volatile var resumed = false

    external fun onPushPlatform(token: String, packageName: String, enabled: Boolean)

    fun validKey(value: String) = value.length == 64 && value.all { it in '0'..'9' || it in 'a'..'f' }
    fun validOrigin(value: String) = value.isNotEmpty() && value.length <= 512 && value.none { it.isISOControl() }
    fun validId(value: String) = ID.matches(value)
    fun dmTag(account: String, peer: String) = "$DM_PREFIX$account:$peer"

    private fun manager(context: Context) = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
    private fun labels(context: Context) = context.getSharedPreferences("notification-labels", Context.MODE_PRIVATE)
    private fun alerted(context: Context) = context.getSharedPreferences("notification-alerted", Context.MODE_PRIVATE)
    private fun push(context: Context) = context.getSharedPreferences("push", Context.MODE_PRIVATE)

    fun allowed(context: Context): Boolean {
        if (!context.getSharedPreferences("dm-notifications", Context.MODE_PRIVATE).getBoolean("enabled", false)) return false
        if (Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return false
        return manager(context).areNotificationsEnabled()
    }

    fun saveLabels(context: Context, payload: String) {
        val value = try { JSONObject(payload) } catch (_: Exception) { return }
        val editor = labels(context).edit()
        for (key in FALLBACK.keys) value.optString(key).takeIf { it.isNotBlank() }?.let { editor.putString(key, it.take(200)) }
        editor.apply()
    }

    fun label(preferences: SharedPreferences, key: String): String =
        preferences.getString(key, null)?.takeIf { it.isNotBlank() } ?: FALLBACK.getValue(key)

    fun ensureChannels(context: Context) {
        val manager = manager(context)
        val dm = label(labels(context), "dm")
        val mention = label(labels(context), "mention")
        manager.createNotificationChannel(NotificationChannel(DM_CHANNEL, dm, NotificationManager.IMPORTANCE_HIGH))
        manager.createNotificationChannel(quiet(QUIET_CHANNEL, dm))
        manager.createNotificationChannel(NotificationChannel(MENTION_CHANNEL, mention, NotificationManager.IMPORTANCE_HIGH))
        manager.createNotificationChannel(quiet(QUIET_MENTION_CHANNEL, mention))
    }

    private fun quiet(id: String, name: String) = NotificationChannel(id, name, NotificationManager.IMPORTANCE_LOW).apply {
        setSound(null, null)
        enableVibration(false)
    }

    fun decodeSeen(text: String?): LinkedHashMap<String, Long> {
        val seen = LinkedHashMap<String, Long>()
        for (line in text.orEmpty().split('\n')) {
            val space = line.indexOf(' ')
            if (space <= 0) continue
            val at = line.substring(0, space).toLongOrNull() ?: continue
            seen[line.substring(space + 1)] = at
        }
        return seen
    }

    fun encodeSeen(seen: Map<String, Long>): String = seen.entries.joinToString("\n") { "${it.value} ${it.key}" }

    fun admit(seen: LinkedHashMap<String, Long>, tag: String, at: Long): Boolean {
        val last = seen[tag]
        if (last != null && at <= last) return false
        seen.remove(tag)
        seen[tag] = at
        while (seen.size > DEDUP_CAP) seen.remove(seen.keys.first())
        return true
    }

    // A DM can arrive both live and by push; the last alerted `at` per tag makes it alert once.
    @Synchronized
    fun remember(preferences: SharedPreferences, tag: String, at: Long): Boolean {
        if (tag.contains('\n')) return false
        val seen = decodeSeen(preferences.getString(SEEN, null))
        if (!admit(seen, tag, at)) return false
        preferences.edit().putString(SEEN, encodeSeen(seen)).commit()
        return true
    }

    private fun launch(context: Context, uri: Uri?): PendingIntent? {
        val launch = context.packageManager.getLaunchIntentForPackage(context.packageName) ?: return null
        if (uri != null) {
            launch.action = Intent.ACTION_VIEW
            launch.data = uri
            launch.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        }
        return PendingIntent.getActivity(context, 0, launch, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }

    private fun publish(context: Context, tag: String, prefix: String, notification: Notification) {
        val manager = manager(context)
        try {
            manager.notify(tag, 0, notification)
            manager.activeNotifications.filter { it.tag?.startsWith(prefix) == true }
                .sortedByDescending { it.postTime }.drop(SHOWN_LIMIT)
                .forEach { manager.cancel(it.tag, it.id) }
        } catch (error: SecurityException) {
            android.util.Log.w("Discordia", "Notification permission changed", error)
        }
    }

    fun cancel(context: Context, prefix: String) {
        val manager = manager(context)
        manager.activeNotifications.filter { it.tag?.startsWith(prefix) == true }
            .forEach { manager.cancel(it.tag, it.id) }
    }

    fun post(context: Context, alert: JSONObject) {
        when (alert.optString("kind")) {
            "dm" -> showDm(context, alert)
            "mention" -> showMention(context, alert)
            "ping" -> showPing(context)
        }
    }

    fun showDm(context: Context, value: JSONObject) {
        val owner = value.getString("account")
        val peer = value.getString("peer")
        if (!validKey(owner) || !validKey(peer)) return
        val at = value.getLong("at")
        val tag = dmTag(owner, peer)
        val pending = launch(context, Uri.Builder().scheme("discordia").authority("dm").appendPath(owner).appendPath(peer).build()) ?: return
        if (!remember(alerted(context), tag, at)) return
        ensureChannels(context)
        val text = value.getString("message").take(301)
        val channel = if (value.optBoolean("silent", false)) QUIET_CHANNEL else DM_CHANNEL
        // `message` may be the message itself; a locked screen gets the line that never is.
        val publicVersion = Notification.Builder(context, channel)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle(context.getString(R.string.app_name))
            .setContentText(value.optString("public", label(labels(context), "dm")))
            .build()
        val notification = Notification.Builder(context, channel)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle(value.getString("sender").take(100))
            .setContentText(text)
            .setStyle(Notification.BigTextStyle().bigText(text))
            .setCategory(Notification.CATEGORY_MESSAGE)
            .setVisibility(Notification.VISIBILITY_PRIVATE)
            .setPublicVersion(publicVersion)
            .setContentIntent(pending)
            .setAutoCancel(true)
            .setExtras(Bundle().apply { putLong(READ_TIME, at) })
            .setOnlyAlertOnce(false)
            .build()
        publish(context, tag, DM_PREFIX, notification)
    }

    fun showMention(context: Context, value: JSONObject) {
        val account = value.getString("account")
        val origin = value.getString("origin")
        val guild = value.getString("guild")
        val channel = value.getString("channel")
        val message = value.getString("message_id")
        if (!validKey(account) || !validOrigin(origin) || !listOf(guild, channel, message).all { validId(it) }) return
        val tag = "$MENTION_PREFIX$account:$origin:$channel"
        // appendPath percent-encodes the origin's `/`, `:` and `@`; pathSegments decodes them.
        val uri = Uri.Builder().scheme("discordia").authority("mention").appendPath(account).appendPath(origin)
            .appendPath(guild).appendPath(channel).appendPath(message).build()
        val pending = launch(context, uri) ?: return
        if (!remember(alerted(context), tag, value.getLong("at"))) return
        ensureChannels(context)
        val notificationChannel = if (value.optBoolean("silent", false)) QUIET_MENTION_CHANNEL else MENTION_CHANNEL
        val publicVersion = Notification.Builder(context, notificationChannel)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle(context.getString(R.string.app_name))
            .setContentText(label(labels(context), "mention"))
            .build()
        val notification = Notification.Builder(context, notificationChannel)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle("${value.getString("guild_name")} · ${value.getString("channel_name")}".take(200))
            .setContentText(value.getString("text").take(300))
            .setCategory(Notification.CATEGORY_MESSAGE)
            .setVisibility(Notification.VISIBILITY_PRIVATE)
            .setPublicVersion(publicVersion)
            .setContentIntent(pending)
            .setAutoCancel(true)
            .build()
        publish(context, tag, MENTION_PREFIX, notification)
    }

    private fun showPing(context: Context) {
        ensureChannels(context)
        val notification = Notification.Builder(context, DM_CHANNEL)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle(context.getString(R.string.app_name))
            .setContentText(label(labels(context), "ping"))
            .setContentIntent(launch(context, null))
            .setAutoCancel(true)
            .build()
        publish(context, PING_TAG, PING_TAG, notification)
    }

    fun storeToken(context: Context, token: String) {
        push(context).edit().putString("token", token).commit()
    }

    fun reportPlatform(context: Context) {
        val token = push(context).getString("token", null).orEmpty()
        try {
            onPushPlatform(token, context.packageName, allowed(context))
        } catch (_: UnsatisfiedLinkError) {
            // A process started for a push alone has no Rust UI to tell; the Activity reports on start.
        }
    }

    fun pushAvailable(context: Context): Boolean = try {
        FirebaseApp.getApps(context).isNotEmpty()
    } catch (_: Throwable) {
        false
    }

    fun requestToken(context: Context) {
        val app = context.applicationContext
        reportPlatform(app)
        if (!pushAvailable(app)) return
        try {
            FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
                if (task.isSuccessful) {
                    task.result?.let { token ->
                        storeToken(app, token)
                        reportPlatform(app)
                    }
                }
            }
        } catch (error: Exception) {
            android.util.Log.w("Discordia", "Push token unavailable", error)
        }
    }
}
