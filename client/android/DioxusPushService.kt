package com.discordia.app

import android.content.Context
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import org.json.JSONObject
import java.io.File

class DioxusPushService : FirebaseMessagingService() {
    // Takes its directory and language because the process may have no Activity,
    // and Rust's own lookups of both go through one.
    external fun handlePush(context: Context, configDir: String, language: String, data: String): String?

    override fun onNewToken(token: String) {
        DioxusNotifier.storeToken(this, token)
        DioxusNotifier.reportPlatform(this)
    }

    override fun onMessageReceived(message: RemoteMessage) {
        try {
            // A resumed app shows its own DMs; a server pushes a mention only when
            // nobody is looking at that server, so one that arrives is still news.
            val shownLive = DioxusNotifier.resumed && message.data["t"] == "dm"
            if (shownLive || !DioxusNotifier.allowed(this)) return
            val data = JSONObject(message.data).toString()
            // Only WryActivity loads it otherwise; a second load is a no-op.
            System.loadLibrary("main")
            val alert = handlePush(
                applicationContext,
                File(noBackupFilesDir, "discordia").absolutePath,
                resources.configuration.locales[0].toLanguageTag(),
                data,
            ) ?: return
            DioxusNotifier.post(this, JSONObject(alert))
        } catch (error: Throwable) {
            android.util.Log.w("DiscordiaPush", "Push not handled: ${error.javaClass.simpleName}")
        }
    }
}
