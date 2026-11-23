package com.discordia.app

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.RemoteInput
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.graphics.drawable.Icon
import android.net.Uri
import android.os.Build
import android.os.Bundle
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging
import org.json.JSONObject

// Context only: a push may start a process in which no Activity ever runs.
object DioxusNotifier {
    // A channel's vibration is fixed once created, so a new pattern needs new ids.
    const val DM_CHANNEL = "discordia-dm-v2"
    const val QUIET_CHANNEL = "discordia-dm-quiet-v1"
    const val MENTION_CHANNEL = "discordia-mention-v2"
    const val QUIET_MENTION_CHANNEL = "discordia-mention-quiet-v1"
    const val DM_PREFIX = "discordia-dm:"
    const val MENTION_PREFIX = "discordia-mention:"
    const val READ_TIME = "discordia-dm-at"
    private const val LINE_TIMES = "discordia-dm-line-times"
    private const val LINE_TEXTS = "discordia-dm-line-texts"
    private const val DM_LINES = 5
    private const val PING_TAG = "discordia-ping"
    private const val SHOWN_LIMIT = 20
    private const val DEDUP_CAP = 64
    private const val SEEN = "seen"
    private val FALLBACK = mapOf("dm" to "Private messages", "mention" to "Mentions", "ping" to "Notifications are working", "reply" to "Reply", "read" to "Mark as read")
    private val RETIRED = mapOf("discordia-dm-v1" to DM_CHANNEL, "discordia-mention-v1" to MENTION_CHANNEL)
    // Two long pulses; the default's two short taps went unnoticed in a pocket.
    private val VIBRATION = longArrayOf(0, 400, 200, 400)
    private val ID = Regex("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")

    @Volatile var resumed = false
    // The account whose Rust UI is running; its buttons go to that, not to a headless send.
    @Volatile var liveAccount: String? = null

    external fun onPushPlatform(token: String, packageName: String, enabled: Boolean)

    fun validKey(value: String) = value.length == 64 && value.all { it in '0'..'9' || it in 'a'..'f' }
    fun validOrigin(value: String) = value.isNotEmpty() && value.length <= 512 && value.none { it.isISOControl() }
    fun validId(value: String) = ID.matches(value)
    fun dmTag(account: String, peer: String) = "$DM_PREFIX$account:$peer"

    private fun manager(context: Context) = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
    private fun labels(context: Context) = context.getSharedPreferences("notification-labels", Context.MODE_PRIVATE)
    private fun alerted(context: Context) = context.getSharedPreferences("notification-alerted", Context.MODE_PRIVATE)
    private fun push(context: Context) = context.getSharedPreferences("push", Context.MODE_PRIVATE)

    // On until the person turns it off: what is stored is only a choice they made.
    fun wanted(preferences: SharedPreferences) = preferences.getBoolean("enabled", true)

    // Android 13 grants nothing until asked and lets an app ask twice; one of those is spent unprompted.
    fun asksUnprompted(wanted: Boolean, asked: Boolean, sdk: Int, granted: Boolean) = wanted && !asked && sdk >= 33 && !granted

    fun allowed(context: Context): Boolean {
        if (!wanted(context.getSharedPreferences("dm-notifications", Context.MODE_PRIVATE))) return false
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
        // A channel the person switched off stays off under its new id.
        val off = RETIRED.filter { (old, _) -> manager.getNotificationChannel(old)?.importance == NotificationManager.IMPORTANCE_NONE }.values.toSet()
        manager.createNotificationChannel(loud(DM_CHANNEL, dm, DM_CHANNEL in off))
        manager.createNotificationChannel(quiet(QUIET_CHANNEL, dm))
        manager.createNotificationChannel(loud(MENTION_CHANNEL, mention, MENTION_CHANNEL in off))
        manager.createNotificationChannel(quiet(QUIET_MENTION_CHANNEL, mention))
        RETIRED.keys.forEach { manager.deleteNotificationChannel(it) }
    }

    private fun loud(id: String, name: String, off: Boolean) =
        NotificationChannel(id, name, if (off) NotificationManager.IMPORTANCE_NONE else NotificationManager.IMPORTANCE_HIGH).apply {
            enableVibration(true)
            vibrationPattern = VIBRATION
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

    // The notification on screen is the memory: read or swiped away, the next one starts over.
    fun unseen(times: LongArray?, texts: Array<String>?, at: Long, text: String): List<Pair<Long, String>> {
        val earlier = if (times != null && texts != null && times.size == texts.size) times.toList().zip(texts.toList()) else emptyList()
        return (earlier + (at to text)).takeLast(DM_LINES)
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

    private fun dmAction(context: Context, action: String, owner: String, peer: String, label: String, mutable: Boolean): PendingIntent {
        val intent = Intent(context, DioxusDmActions::class.java).setAction(action)
            .setData(Uri.Builder().scheme("discordia").authority("dm").appendPath(owner).appendPath(peer).build())
        // A reply's text is written into the intent, so only that one may be mutable.
        val flags = if (mutable && Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else PendingIntent.FLAG_IMMUTABLE
        return PendingIntent.getBroadcast(context, label.hashCode(), intent, flags or PendingIntent.FLAG_UPDATE_CURRENT)
    }

    private fun dmActions(context: Context, owner: String, peer: String): List<Notification.Action> {
        val names = labels(context)
        val replyLabel = label(names, "reply")
        val input = RemoteInput.Builder(DioxusDmActions.REPLY_TEXT).setLabel(replyLabel).build()
        val reply = Notification.Action.Builder(
            Icon.createWithResource(context, android.R.drawable.ic_menu_send), replyLabel,
            dmAction(context, DioxusDmActions.REPLY, owner, peer, "reply", true),
        ).addRemoteInput(input)
        val read = Notification.Action.Builder(
            Icon.createWithResource(context, android.R.drawable.ic_menu_view), label(names, "read"),
            dmAction(context, DioxusDmActions.READ, owner, peer, "read", false),
        )
        if (Build.VERSION.SDK_INT >= 28) {
            reply.setSemanticAction(Notification.Action.SEMANTIC_ACTION_REPLY)
            read.setSemanticAction(Notification.Action.SEMANTIC_ACTION_MARK_AS_READ)
        }
        return listOf(reply.build(), read.build())
    }

    fun dismissDm(context: Context, owner: String, peer: String) {
        manager(context).cancel(dmTag(owner, peer), 0)
    }

    // A reply that could not go leaves the notification as it was, minus the field's spinner.
    fun settleDm(context: Context, owner: String, peer: String) {
        val tag = dmTag(owner, peer)
        val manager = manager(context)
        val shown = manager.activeNotifications.firstOrNull { it.tag == tag }?.notification ?: return
        val again = Notification.Builder.recoverBuilder(context, shown).setOnlyAlertOnce(true).build()
        try {
            manager.notify(tag, 0, again)
        } catch (error: SecurityException) {
            android.util.Log.w("Discordia", "Notification permission changed", error)
        }
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

    @Suppress("DEPRECATION")
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
        val sender = value.getString("sender").take(100)
        val channel = if (value.optBoolean("silent", false)) QUIET_CHANNEL else DM_CHANNEL
        val shown = try {
            manager(context).activeNotifications.firstOrNull { it.tag == tag }?.notification?.extras
        } catch (_: Exception) {
            null
        }
        val lines = unseen(shown?.getLongArray(LINE_TIMES), shown?.getStringArray(LINE_TEXTS), at, text)
        // The names, not `Person`: that needs API 28 and this runs from 26.
        val style = Notification.MessagingStyle(context.getString(R.string.app_name))
        for ((time, line) in lines) style.addMessage(line, time * 1000, sender)
        // `message` may be the message itself; a locked screen gets the line that never is.
        val publicVersion = Notification.Builder(context, channel)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle(context.getString(R.string.app_name))
            .setContentText(value.optString("public", label(labels(context), "dm")))
            .build()
        val notification = Notification.Builder(context, channel)
            .setSmallIcon(android.R.drawable.ic_dialog_email)
            .setContentTitle(sender)
            .setContentText(text)
            .setStyle(style)
            .setCategory(Notification.CATEGORY_MESSAGE)
            .setVisibility(Notification.VISIBILITY_PRIVATE)
            .setPublicVersion(publicVersion)
            .setContentIntent(pending)
            .setAutoCancel(true)
            .setActions(*dmActions(context, owner, peer).toTypedArray())
            .setExtras(Bundle().apply {
                putLong(READ_TIME, at)
                putLongArray(LINE_TIMES, lines.map { it.first }.toLongArray())
                putStringArray(LINE_TEXTS, lines.map { it.second }.toTypedArray())
            })
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
