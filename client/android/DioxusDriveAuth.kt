package com.discordia.app

import android.app.Activity
import android.content.Intent
import com.google.android.gms.auth.api.identity.AuthorizationRequest
import com.google.android.gms.auth.api.identity.AuthorizationResult
import com.google.android.gms.auth.api.identity.Identity
import com.google.android.gms.common.api.Scope
import org.json.JSONObject

// Google matches the app by package and signing certificate, so no client id
// ships in the APK. Rust polls `take()`, which hands the token over once.
class DioxusDriveAuth(private val activity: Activity) {
    companion object {
        const val REQUEST = 9400
        private const val SCOPE = "https://www.googleapis.com/auth/drive.appdata"
    }

    @Volatile private var status = "idle"
    @Volatile private var token = ""
    @Volatile private var message = ""
    @Volatile private var generation = 0

    fun begin(): Boolean {
        if (status == "pending") return false
        status = "pending"
        token = ""
        message = ""
        val operation = ++generation
        val request = AuthorizationRequest.builder()
            .setRequestedScopes(listOf(Scope(SCOPE)))
            .build()
        return try {
            Identity.getAuthorizationClient(activity).authorize(request)
                .addOnSuccessListener { result ->
                    if (generation != operation) return@addOnSuccessListener
                    val pending = result.pendingIntent
                    if (result.hasResolution() && pending != null) {
                        try {
                            activity.startIntentSenderForResult(pending.intentSender, REQUEST, null, 0, 0, 0)
                        } catch (error: Exception) {
                            fail("error", error.message ?: "")
                        }
                    } else {
                        finish(result)
                    }
                }
                .addOnFailureListener { error ->
                    if (generation == operation) fail("error", error.message ?: "")
                }
            true
        } catch (error: Exception) {
            fail("error", error.message ?: "")
            false
        }
    }

    fun activityResult(requestCode: Int, resultCode: Int, data: Intent?): Boolean {
        if (requestCode != REQUEST) return false
        if (status != "pending") return true
        if (resultCode != Activity.RESULT_OK || data == null) {
            fail("cancelled", "")
            return true
        }
        try {
            finish(Identity.getAuthorizationClient(activity).getAuthorizationResultFromIntent(data))
        } catch (error: Exception) {
            fail("error", error.message ?: "")
        }
        return true
    }

    private fun finish(result: AuthorizationResult) {
        val granted = result.grantedScopes.any { it == SCOPE }
        val access = result.accessToken
        if (!granted || access.isNullOrEmpty()) {
            fail("denied", "")
            return
        }
        token = access
        status = "ok"
    }

    private fun fail(kind: String, detail: String) {
        token = ""
        message = detail
        status = kind
    }

    fun take(): String {
        val current = status
        val reply = JSONObject().put("status", current).put("token", token).put("message", message).toString()
        if (current != "pending") {
            status = "idle"
            token = ""
            message = ""
        }
        return reply
    }

    fun cancel() {
        generation++
        fail("idle", "")
    }
}
