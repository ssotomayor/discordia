package com.discordia.app

import android.app.RemoteInput
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import java.io.File

// A DM notification's Reply and Mark as read. The process may have no Activity.
class DioxusDmActions : BroadcastReceiver() {
    external fun onLiveAction(account: String, peer: String, reply: String?)

    // Takes its directory because Rust's own lookup of it goes through an Activity.
    external fun handleAction(context: Context, configDir: String, account: String, peer: String, reply: String?): Boolean

    override fun onReceive(context: Context, intent: Intent) {
        val parts = intent.data?.pathSegments ?: return
        if (intent.data?.scheme != "discordia" || parts.size != 2 || !parts.all { DioxusNotifier.validKey(it) }) return
        val (account, peer) = parts
        val reply = when (intent.action) {
            REPLY -> RemoteInput.getResultsFromIntent(intent)?.getCharSequence(REPLY_TEXT)?.toString()?.trim()
                ?.takeIf { it.isNotEmpty() } ?: return DioxusNotifier.settleDm(context, account, peer)
            READ -> null
            else -> return
        }
        if (DioxusNotifier.liveAccount == account) {
            onLiveAction(account, peer, reply)
            DioxusNotifier.dismissDm(context, account, peer)
            return
        }
        // Opening the key and reaching a relay take seconds: off the main thread, inside goAsync's window.
        val pending = goAsync()
        Thread {
            try {
                // Only WryActivity loads it otherwise; a second load is a no-op.
                System.loadLibrary("main")
                val dir = File(context.noBackupFilesDir, "discordia").absolutePath
                if (handleAction(context.applicationContext, dir, account, peer, reply)) {
                    DioxusNotifier.dismissDm(context, account, peer)
                } else {
                    DioxusNotifier.settleDm(context, account, peer)
                }
            } catch (error: Throwable) {
                android.util.Log.w("Discordia", "Notification action not handled: ${error.javaClass.simpleName}")
                DioxusNotifier.settleDm(context, account, peer)
            } finally {
                pending.finish()
            }
        }.start()
    }

    companion object {
        const val REPLY = "com.discordia.app.DM_REPLY"
        const val READ = "com.discordia.app.DM_READ"
        const val REPLY_TEXT = "discordia-reply"
    }
}
