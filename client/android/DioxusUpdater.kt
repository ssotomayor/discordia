package com.discordia.app

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageInfo
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import org.json.JSONObject
import java.io.File

class DioxusUpdater(private val activity: Activity, private val canInstall: () -> Boolean) {
    companion object { const val PERMISSION_REQUEST = 9300 }
    @Volatile private var result = "idle"
    @Volatile private var message = ""
    private var pending: File? = null
    private var sessionId: Int? = null
    private var receiver: BroadcastReceiver? = null
    @Volatile private var generation = 0

    fun state(): String = JSONObject().put("status", result).put("message", message).toString()

    fun begin(path: String, expectedVersion: Long): Boolean {
        if (BuildConfig.DEBUG || activity.packageName != "com.discordia.app" ||
            result in listOf("checking", "permission", "installing")) return false
        result = "checking"
        message = ""
        val operation = ++generation
        Thread {
            try {
                val installer = activity.packageManager.packageInstaller
                for (orphan in installer.mySessions) {
                    if (generation != operation) return@Thread
                    try { installer.abandonSession(orphan.sessionId) } catch (_: Exception) {}
                }
                val file = File(path).canonicalFile
                val directory = File(activity.noBackupFilesDir, "discordia/updates").canonicalFile
                require(file.parentFile == directory && file.name == "Discordia-android-arm64.apk" && file.isFile) {
                    "Invalid Android update file."
                }
                val pm = activity.packageManager
                @Suppress("DEPRECATION")
                val flags = if (Build.VERSION.SDK_INT >= 28) PackageManager.GET_SIGNING_CERTIFICATES else PackageManager.GET_SIGNATURES
                val installed = pm.getPackageInfo(activity.packageName, flags)
                val update = pm.getPackageArchiveInfo(file.path, flags) ?: error("Invalid Android update package.")
                require(update.packageName == activity.packageName) { "The update belongs to another app." }
                require(version(update) == expectedVersion && expectedVersion > version(installed)) { "The update version is not newer." }
                val signatures = certificates(installed)
                require(signatures.isNotEmpty() && signatures == certificates(update)) { "The update signing certificate does not match." }
                activity.runOnUiThread {
                    if (generation != operation) return@runOnUiThread
                    pending = file
                    try {
                        if (pm.canRequestPackageInstalls()) install()
                        else {
                            result = "permission"
                            activity.startActivityForResult(
                                Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${activity.packageName}")),
                                PERMISSION_REQUEST,
                            )
                        }
                    } catch (error: Exception) { fail(error) }
                }
            } catch (error: Exception) { activity.runOnUiThread { if (generation == operation) fail(error) } }
        }.start()
        return true
    }

    fun permissionReturned(): Boolean {
        if (result != "permission") return false
        try {
            if (activity.packageManager.canRequestPackageInstalls()) install()
            else { result = "cancelled"; pending = null }
        } catch (error: Exception) { fail(error) }
        return true
    }

    private fun install() {
        val file = pending ?: return
        result = "installing"
        val operation = generation
        try {
            check(canInstall()) { "Finish your call before installing. The download is ready." }
            val action = "${activity.packageName}.UPDATE_RESULT.${java.util.UUID.randomUUID()}"
            val callback = object : BroadcastReceiver() {
                override fun onReceive(context: Context, intent: Intent) {
                    if (generation != operation) return
                    val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)
                    if (status == PackageInstaller.STATUS_PENDING_USER_ACTION) {
                        try {
                            @Suppress("DEPRECATION")
                            val confirm = intent.getParcelableExtra<Intent>(Intent.EXTRA_INTENT) ?: error("Android installer confirmation is unavailable.")
                            activity.startActivity(confirm)
                        } catch (error: Exception) { fail(error) }
                        return
                    }
                    val outcome = when (status) {
                        PackageInstaller.STATUS_SUCCESS -> "installed"
                        PackageInstaller.STATUS_FAILURE_ABORTED -> "cancelled"
                        else -> "failed"
                    }
                    message = when (status) {
                        PackageInstaller.STATUS_FAILURE_STORAGE -> "Not enough free space to install the update."
                        PackageInstaller.STATUS_FAILURE_INCOMPATIBLE -> "This update is not compatible with this device."
                        else -> if (outcome == "failed") "Android could not install the update ($status)." else ""
                    }
                    closeReceiver()
                    pending = null
                    sessionId = null
                    result = outcome
                }
            }
            receiver = callback
            if (Build.VERSION.SDK_INT >= 33) activity.registerReceiver(callback, IntentFilter(action), Context.RECEIVER_NOT_EXPORTED)
            else {
                @Suppress("DEPRECATION")
                activity.registerReceiver(callback, IntentFilter(action))
            }
            val installer = activity.packageManager.packageInstaller
            val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
            params.setAppPackageName(activity.packageName)
            params.setSize(file.length())
            if (Build.VERSION.SDK_INT >= 31) params.setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_REQUIRED)
            val id = installer.createSession(params)
            sessionId = id
            val flags = PendingIntent.FLAG_UPDATE_CURRENT or if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0
            val resultIntent = PendingIntent.getBroadcast(activity, id, Intent(action).setPackage(activity.packageName), flags)
            Thread {
                try {
                    installer.openSession(id).use { session ->
                        session.openWrite("base.apk", 0, file.length()).use { output ->
                            file.inputStream().use { it.copyTo(output) }
                            session.fsync(output)
                        }
                        if (generation == operation) session.commit(resultIntent.intentSender)
                    }
                } catch (error: Exception) { activity.runOnUiThread { if (generation == operation) fail(error) } }
            }.start()
        } catch (error: Exception) { fail(error) }
    }

    fun cancel() {
        generation++
        sessionId?.let { try { activity.packageManager.packageInstaller.abandonSession(it) } catch (_: Exception) {} }
        sessionId = null
        closeReceiver()
        pending = null
        result = "cancelled"
    }

    private fun fail(error: Exception) {
        cancel()
        message = error.message ?: "Android could not install the update."
        result = "failed"
    }

    private fun closeReceiver() {
        receiver?.let { try { activity.unregisterReceiver(it) } catch (_: Exception) {} }
        receiver = null
    }

    @Suppress("DEPRECATION")
    private fun version(info: PackageInfo): Long = if (Build.VERSION.SDK_INT >= 28) info.longVersionCode else info.versionCode.toLong()

    @Suppress("DEPRECATION")
    private fun certificates(info: PackageInfo): Set<String> =
        (if (Build.VERSION.SDK_INT >= 28) info.signingInfo?.apkContentsSigners else info.signatures)
            ?.map { it.toCharsString() }?.toSet() ?: emptySet()
}
