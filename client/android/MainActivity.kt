package dev.dioxus.main

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ResultReceiver
import android.view.WindowManager
import livekit.org.webrtc.Camera2Enumerator
import livekit.org.webrtc.CameraVideoCapturer
import livekit.org.webrtc.CapturerObserver
import livekit.org.webrtc.EglBase
import livekit.org.webrtc.ScreenCapturerAndroid
import livekit.org.webrtc.SurfaceTextureHelper
import livekit.org.webrtc.VideoCapturer
import livekit.org.webrtc.VideoFrame
import org.json.JSONArray
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.File
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import android.webkit.WebSettings
import android.webkit.WebView

typealias BuildConfig = com.discordia.app.BuildConfig

class MainActivity : WryActivity() {
    private var appUpdater: com.discordia.app.DioxusUpdater? = null
    private var networkCallback: ConnectivityManager.NetworkCallback? = null

    external fun onNetworkChanged()

    private fun watchDefaultNetwork() {
        val manager = getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) { onNetworkChanged() }
            override fun onLost(network: Network) { onNetworkChanged() }
            override fun onLinkPropertiesChanged(network: Network, properties: LinkProperties) {
                onNetworkChanged()
            }
        }
        try {
            manager.registerDefaultNetworkCallback(callback)
            networkCallback = callback
        } catch (error: Exception) {
            android.util.Log.w("Discordia", "Could not monitor network changes", error)
        }
    }

    @Suppress("DEPRECATION")
    fun installedUpdateVersion(): Long {
        if (BuildConfig.DEBUG || packageName != "com.discordia.app") return 0
        val info = packageManager.getPackageInfo(packageName, 0)
        return if (android.os.Build.VERSION.SDK_INT >= 28) info.longVersionCode else info.versionCode.toLong()
    }

    fun beginAppUpdate(path: String, version: Long): Boolean {
        val updater = appUpdater ?: com.discordia.app.DioxusUpdater(this) {
            voiceSlot < 0 && cameraSlot < 0 && !projectionActive
        }.also { appUpdater = it }
        return updater.begin(path, version)
    }

    fun appUpdateState(): String = appUpdater?.state() ?: "{}"

    fun cancelAppUpdate() { appUpdater?.cancel() }

    external fun initializeWebRtc(): Boolean

    external fun onVideoFrame(
        slot: Int,
        y: java.nio.ByteBuffer,
        u: java.nio.ByteBuffer,
        v: java.nio.ByteBuffer,
        strideY: Int,
        strideU: Int,
        strideV: Int,
        width: Int,
        height: Int,
        rotation: Int,
        timestampNs: Long,
    )

    external fun onCaptureStopped(slot: Int)

    external fun shouldCaptureVideoFrame(slot: Int): Boolean

    private var documentRequest: String? = null
    private var documentResult: String? = null
    private var documentBytes: ByteArray? = null
    private var documentPickerOpen = false
    private var documentRequestCode = 1000

    private var screenRequestCode = 700
    private var screenSlot = -1
    private var screenPending = false
    private var screenResult = 0
    private var screenWidth = 1280
    private var screenHeight = 720
    private var screenFps = 30
    private var screenMaxWidth = 1280
    private var screenMaxHeight = 720
    private var screenCapturer: VideoCapturer? = null
    private var screenHelper: SurfaceTextureHelper? = null
    private var screenEgl: EglBase? = null
    private var mediaProjection: MediaProjection? = null
    private var screenGrant: Intent? = null
    private var screenWanted = false

    private var cameraSlot = -1
    private var cameraCapturer: CameraVideoCapturer? = null
    private var cameraHelper: SurfaceTextureHelper? = null
    private var cameraEgl: EglBase? = null
    private var voiceSlot = -1
    private var voiceServiceResult = 0
    private var projectionActive = false

    private fun updateMediaService(ready: ResultReceiver? = null): Boolean {
        val intent = foregroundServiceIntent() ?: return false
        if (voiceSlot < 0 && cameraSlot < 0 && !projectionActive) {
            stopService(intent)
            return true
        }
        intent.putExtra("microphone", voiceSlot >= 0)
        intent.putExtra("camera", cameraSlot >= 0)
        intent.putExtra("projection", projectionActive)
        if (ready != null) intent.putExtra("ready", ready)
        return try {
            startForegroundService(intent)
            true
        } catch (_: Exception) {
            false
        }
    }

    fun startVoiceService(slot: Int): Boolean {
        voiceSlot = slot
        voiceServiceResult = 0
        return updateMediaService(object : ResultReceiver(Handler(Looper.getMainLooper())) {
            override fun onReceiveResult(resultCode: Int, resultData: Bundle?) {
                if (voiceSlot == slot) voiceServiceResult = resultCode
            }
        })
    }

    fun voiceServiceResult(slot: Int): Int = if (voiceSlot == slot) voiceServiceResult else -1

    fun stopVoiceService(slot: Int) {
        if (voiceSlot != slot) return
        voiceSlot = -1
        voiceServiceResult = 0
        updateMediaService()
    }

    private fun cameraEvents(slot: Int) = object : CameraVideoCapturer.CameraEventsHandler {
        override fun onCameraError(errorDescription: String) {
            runOnUiThread {
                if (cameraSlot != slot) return@runOnUiThread
                stopCameraCapture()
                if (slot >= 0) onCaptureStopped(slot)
            }
        }

        override fun onCameraDisconnected() = onCameraError("the camera disconnected")

        override fun onCameraFreezed(errorDescription: String) = onCameraError(errorDescription)

        override fun onCameraOpening(cameraName: String) {}

        override fun onFirstFrameAvailable() {}

        override fun onCameraClosed() {}
    }

    /// Every frame crosses into Rust as I420. The direct buffers the capturer
    /// hands out are read in place, so nothing is copied on this side.
    private fun observer(slot: Int): CapturerObserver = object : CapturerObserver {
        override fun onCapturerStarted(success: Boolean) {}

        override fun onCapturerStopped() {}

        override fun onFrameCaptured(frame: VideoFrame) {
            if (!shouldCaptureVideoFrame(slot)) return
            val source = frame.buffer
            val i420 = source.toI420() ?: return
            try {
                onVideoFrame(
                    slot, i420.dataY, i420.dataU, i420.dataV,
                    i420.strideY, i420.strideU, i420.strideV,
                    i420.width, i420.height, frame.rotation, frame.timestampNs,
                )
            } finally {
                // toI420 retains even when it returns the original buffer.
                i420.release()
            }
        }
    }

    fun displaySize(): String {
        val metrics = resources.displayMetrics
        return "${metrics.widthPixels}x${metrics.heightPixels}"
    }

    /// A MediaProjection virtual display is created once, so it is sized to the
    /// screen's current aspect inside the share's budget rather than letterboxed.
    private fun captureSize(maxWidth: Int, maxHeight: Int): Pair<Int, Int> {
        val metrics = resources.displayMetrics
        val displayWidth = metrics.widthPixels.coerceAtLeast(2)
        val displayHeight = metrics.heightPixels.coerceAtLeast(2)
        val longEdge = maxOf(maxWidth, maxHeight)
        val shortEdge = minOf(maxWidth, maxHeight)
        val portrait = displayWidth < displayHeight
        val scale = minOf(
            (if (portrait) shortEdge else longEdge).toDouble() / displayWidth,
            (if (portrait) longEdge else shortEdge).toDouble() / displayHeight,
            1.0,
        )
        val even = { value: Int -> (value / 2 * 2).coerceAtLeast(2) }
        return even((displayWidth * scale).toInt()) to even((displayHeight * scale).toInt())
    }

    /// Asks for the projection on its own, so the app can flip to "sharing" only
    /// once the person agreed instead of while the system dialog is still up.
    fun requestScreenCapture(): Boolean {
        if (screenGrant != null || screenPending) return screenGrant != null
        return launchScreenConsent()
    }

    private fun launchScreenConsent(): Boolean {
        val manager = getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
        screenResult = 0
        screenRequestCode = if (screenRequestCode == 65535) 700 else screenRequestCode + 1
        return try {
            screenPending = true
            startActivityForResult(manager.createScreenCaptureIntent(), screenRequestCode)
            true
        } catch (error: Exception) {
            screenPending = false
            screenResult = -1
            false
        }
    }

    fun startScreenShare(slot: Int, width: Int, height: Int, fps: Int): Boolean {
        if (screenCapturer != null || screenPending) return false
        val (captureWidth, captureHeight) = captureSize(width, height)
        screenSlot = slot
        screenMaxWidth = width
        screenMaxHeight = height
        screenWidth = captureWidth
        screenHeight = captureHeight
        screenFps = fps.coerceAtLeast(1)
        val granted = screenGrant
        if (granted != null) {
            screenGrant = null
            screenResult = 0
            return startProjectionService(granted)
        }
        screenWanted = true
        return launchScreenConsent()
    }

    // Keeping the old aspect ratio after rotation would letterbox the share.
    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        val capturer = screenCapturer ?: return
        val (width, height) = captureSize(screenMaxWidth, screenMaxHeight)
        if (width == screenWidth && height == screenHeight) return
        try {
            capturer.changeCaptureFormat(width, height, screenFps)
            screenWidth = width
            screenHeight = height
        } catch (_: Exception) {
        }
    }

    private fun startProjectionService(grant: Intent): Boolean {
        val slot = screenSlot
        screenPending = true
        projectionActive = true
        val started = updateMediaService(object : ResultReceiver(Handler(Looper.getMainLooper())) {
            override fun onReceiveResult(resultCode: Int, resultData: Bundle?) {
                if (screenSlot != slot || !screenPending) return
                screenPending = false
                if (resultCode == 1) {
                    startScreenCapturer(grant)
                } else {
                    stopScreenShare()
                    screenResult = -1
                }
            }
        })
        if (!started) {
            stopScreenShare()
            screenResult = -1
        }
        return started
    }

    private fun startScreenCapturer(grant: Intent): Boolean = try {
        val slot = screenSlot
        val egl = EglBase.create()
        screenEgl = egl
        val helper = SurfaceTextureHelper.create("discordia-screen", egl.eglBaseContext)
        screenHelper = helper
        val capturer = ScreenCapturerAndroid(grant, object : MediaProjection.Callback() {
            override fun onStop() {
                val stopped = slot
                // The framework delivers this on its own thread; the
                // capturer's own teardown belongs on the UI thread.
                runOnUiThread {
                    if (stopped < 0 || screenSlot != stopped) return@runOnUiThread
                    stopScreenShare()
                    if (stopped >= 0) onCaptureStopped(stopped)
                }
            }
        })
        screenCapturer = capturer
        capturer.initialize(helper, applicationContext, observer(screenSlot))
        capturer.startCapture(screenWidth, screenHeight, screenFps)
        screenGrant = null
        screenResult = 1
        true
    } catch (error: Exception) {
        stopScreenShare()
        screenResult = -1
        false
    }

    fun takeScreenShareResult(): Int = screenResult

    fun screenCapturePressure(): Int {
        val memory = android.app.ActivityManager.MemoryInfo()
        (getSystemService(Context.ACTIVITY_SERVICE) as android.app.ActivityManager).getMemoryInfo(memory)
        val thermal = if (android.os.Build.VERSION.SDK_INT >= 29) {
            (getSystemService(Context.POWER_SERVICE) as android.os.PowerManager).currentThermalStatus
        } else 0
        return thermal.coerceIn(0, 6) or (if (memory.lowMemory) 8 else 0)
    }

    fun resizeScreenShare(slot: Int, width: Int, height: Int, fps: Int): Boolean {
        if (screenSlot != slot || screenResult != 1 || width < 2 || height < 2 || fps !in 1..30) return false
        val capturer = screenCapturer ?: return false
        val (actualWidth, actualHeight) = captureSize(width, height)
        return try {
            capturer.changeCaptureFormat(actualWidth, actualHeight, fps)
            screenMaxWidth = width
            screenMaxHeight = height
            screenWidth = actualWidth
            screenHeight = actualHeight
            screenFps = fps
            true
        } catch (_: Exception) { false }
    }

    fun stopScreenShareSlot(slot: Int) {
        if (screenSlot == slot) stopScreenShare()
    }

    /// dx copies only this activity into its project, so the service it declares
    /// in the manifest is resolved by name instead of imported.
    private fun foregroundServiceIntent(): Intent? = try {
        Intent(this, Class.forName("com.discordia.app.DioxusForegroundService"))
    } catch (error: ClassNotFoundException) {
        null
    }

    fun stopScreenShare() {
        screenPending = false
        screenResult = 0
        screenSlot = -1
        screenWanted = false
        screenGrant = null
        screenCapturer?.let { capturer ->
            try {
                capturer.stopCapture()
            } catch (_: Exception) {
            }
            try {
                capturer.dispose()
            } catch (_: Exception) {
            }
        }
        screenCapturer = null
        screenHelper?.dispose()
        screenHelper = null
        screenEgl?.release()
        screenEgl = null
        val projection = mediaProjection
        mediaProjection = null
        try {
            projection?.stop()
        } catch (_: Exception) {
        }
        projectionActive = false
        updateMediaService()
    }

    /// The camera list is front-facing first, so "Default camera" is the one a
    /// person expects to see themselves in.
    fun cameraDevices(): String {
        val enumerator = Camera2Enumerator(this)
        val names = enumerator.deviceNames.sortedByDescending { enumerator.isFrontFacing(it) }
        val devices = JSONArray()
        for (name in names) {
            devices.put(
                JSONObject()
                    .put("id", name)
                    .put(
                        "label",
                        when {
                            enumerator.isFrontFacing(name) -> "Front camera"
                            enumerator.isBackFacing(name) -> "Back camera"
                            else -> name
                        },
                    ),
            )
        }
        return devices.toString()
    }

    fun startCameraCapture(slot: Int, device: String?, width: Int, height: Int, fps: Int): Boolean {
        if (cameraSlot >= 0) return false
        cameraSlot = slot
        val started = updateMediaService(object : ResultReceiver(Handler(Looper.getMainLooper())) {
            override fun onReceiveResult(resultCode: Int, resultData: Bundle?) {
                if (cameraSlot != slot) return
                if (resultCode != 1 || !startCameraCapturer(slot, device, width, height, fps)) {
                    stopCameraCapture()
                    onCaptureStopped(slot)
                }
            }
        })
        if (!started) stopCameraCapture()
        return started
    }

    private fun startCameraCapturer(slot: Int, device: String?, width: Int, height: Int, fps: Int): Boolean {
        return try {
            val enumerator = Camera2Enumerator(this)
            val name = device?.takeIf { enumerator.deviceNames.contains(it) }
                ?: enumerator.deviceNames.firstOrNull()
                ?: return false
            val capturer = enumerator.createCapturer(name, cameraEvents(slot)) ?: return false
            cameraSlot = slot
            cameraCapturer = capturer
            val egl = EglBase.create()
            cameraEgl = egl
            val helper = SurfaceTextureHelper.create("discordia-camera", egl.eglBaseContext)
            cameraHelper = helper
            capturer.initialize(helper, applicationContext, observer(slot))
            capturer.startCapture(width.coerceAtLeast(2), height.coerceAtLeast(2), fps.coerceAtLeast(1))
            true
        } catch (error: Exception) {
            stopCameraCapture()
            false
        }
    }

    fun stopCameraCapture() {
        cameraSlot = -1
        cameraCapturer?.let { capturer ->
            try {
                capturer.stopCapture()
            } catch (_: Exception) {
            }
            try {
                capturer.dispose()
            } catch (_: Exception) {
            }
        }
        cameraCapturer = null
        cameraHelper?.dispose()
        cameraHelper = null
        cameraEgl?.release()
        cameraEgl = null
        updateMediaService()
    }

    fun stopCameraCaptureSlot(slot: Int) {
        if (cameraSlot == slot) stopCameraCapture()
    }

    fun startDocument(request: String, name: String, extensions: String, bytes: ByteArray?) {
        check(documentRequest == null) { "A document request is already open" }
        documentRequest = request
        documentRequestCode = if (documentRequestCode == 65535) 1000 else documentRequestCode + 1
        documentResult = null
        documentBytes = bytes
        val intent = Intent(if (bytes == null) Intent.ACTION_OPEN_DOCUMENT else Intent.ACTION_CREATE_DOCUMENT)
        intent.addCategory(Intent.CATEGORY_OPENABLE)
        intent.type = "*/*"
        if (bytes != null) {
            intent.putExtra(Intent.EXTRA_TITLE, name.substringAfterLast('/').substringAfterLast('\\'))
            intent.type = "application/octet-stream"
        } else {
            val types = extensions.split(',').mapNotNull {
                MimeTypeMap.getSingleton().getMimeTypeFromExtension(it.lowercase())
            }.distinct().toTypedArray()
            if (types.isNotEmpty()) intent.putExtra(Intent.EXTRA_MIME_TYPES, types)
        }
        try {
            documentPickerOpen = true
            startActivityForResult(intent, documentRequestCode)
        } catch (error: Exception) {
            documentPickerOpen = false
            documentResult = JSONObject().put("error", "Android document picker unavailable").toString()
            documentBytes = null
        }
    }

    fun takeDocumentResult(request: String): String? {
        if (documentRequest != request) return JSONObject().put("error", "Document request interrupted").toString()
        val result = documentResult ?: return null
        documentRequest = null
        documentResult = null
        documentBytes = null
        return result
    }

    fun cancelDocument(request: String) {
        if (documentRequest != request) return
        if (documentPickerOpen) finishActivity(documentRequestCode)
        documentPickerOpen = false
        documentResult?.let { result ->
            JSONObject(result).optString("path").takeIf { it.isNotEmpty() }?.let { File(it).parentFile?.deleteRecursively() }
        }
        documentRequest = null
        documentResult = null
        documentBytes = null
    }

    @Deprecated("Android platform callback")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == com.discordia.app.DioxusUpdater.PERMISSION_REQUEST && appUpdater?.permissionReturned() == true) {
            return
        }
        if (requestCode == screenRequestCode) {
            if (!screenPending) return
            screenPending = false
            if (resultCode != RESULT_OK || data == null) {
                stopScreenShare()
                screenResult = -1
                return
            }
            // A plain request keeps the grant for the share that follows; a
            // share already waiting for it opens the capturer here.
            if (screenWanted) {
                screenWanted = false
                startProjectionService(data)
            } else {
                screenGrant = data
                screenResult = 1
            }
            return
        }
        if (requestCode != documentRequestCode) return
        documentPickerOpen = false
        val request = documentRequest ?: return
        val uri = data?.data
        if (resultCode != RESULT_OK || uri == null) {
            documentResult = JSONObject().put("cancelled", true).toString()
            documentBytes = null
            return
        }
        val bytes = documentBytes
        documentBytes = null
        Thread {
            var directory: File? = null
            val result = try {
                require(uri.scheme == "content") { "Unsupported document provider" }
                if (bytes != null) {
                    contentResolver.openOutputStream(uri, "wt")?.use { it.write(bytes) }
                        ?: error("Destination unavailable")
                    JSONObject().put("saved", true)
                } else {
                    val displayName = contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use {
                        if (it.moveToFirst()) it.getString(0) else null
                    } ?: "attachment"
                    val name = displayName.substringAfterLast('/').substringAfterLast('\\')
                        .filter { it >= ' ' && it != '\u007f' }.take(200)
                        .takeUnless { it.isBlank() || it == "." || it == ".." } ?: "attachment"
                    directory = File(cacheDir, "document-${java.util.UUID.randomUUID()}").apply { check(mkdir()) }
                    val file = File(directory, name)
                    contentResolver.openInputStream(uri)?.use { input ->
                        file.outputStream().use { output ->
                            val buffer = ByteArray(8192)
                            var total = 0
                            while (true) {
                                val count = input.read(buffer)
                                if (count < 0) break
                                total += count
                                require(total <= 32 * 1024 * 1024) { "Selected file exceeds 32 MiB" }
                                output.write(buffer, 0, count)
                            }
                        }
                    } ?: error("Selected file unavailable")
                    JSONObject().put("path", file.absolutePath)
                }
            } catch (error: Exception) {
                directory?.deleteRecursively()
                JSONObject().put("error", error.message ?: "Document operation failed")
            }
            runOnUiThread {
                if (documentRequest == request) documentResult = result.toString()
                else directory?.deleteRecursively()
            }
        }.start()
    }

    fun systemLanguage(): String = resources.configuration.locales[0].toLanguageTag()

    fun copyText(text: String): Boolean {
        val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        clipboard.setPrimaryClip(ClipData.newPlainText("", text))
        return true
    }

    fun readClipboard(): String {
        val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val clip = clipboard.primaryClip ?: return JSONObject().put("ok", false).toString()
        if (clip.itemCount == 0) return JSONObject().put("ok", false).toString()
        val item = clip.getItemAt(0)
        return when {
            item.text != null -> JSONObject().put("ok", true).put("text", item.text.toString())
            item.uri != null && clip.description.hasMimeType("image/*") ->
                JSONObject().put("ok", true).put("image", true)
            else -> JSONObject().put("ok", false)
        }.toString()
    }

    fun readClipboardImage(): ByteArray {
        val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val clip = clipboard.primaryClip ?: error("No clipboard image")
        require(clip.itemCount > 0 && clip.description.hasMimeType("image/*")) { "No clipboard image" }
        val uri = clip.getItemAt(0).uri ?: error("No clipboard image")
        require(uri.scheme == "content") { "Unsupported clipboard image" }
        return contentResolver.openInputStream(uri)?.use { input ->
            val output = ByteArrayOutputStream()
            val buffer = ByteArray(8192)
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                require(output.size() + count <= 2_097_152) { "Clipboard image exceeds 2 MiB" }
                output.write(buffer, 0, count)
            }
            output.toByteArray()
        } ?: error("Clipboard image unavailable")
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        watchDefaultNetwork()
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
    }

    override fun onDestroy() {
        networkCallback?.let { callback ->
            try {
                (getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager)
                    .unregisterNetworkCallback(callback)
            } catch (error: Exception) {
                android.util.Log.w("Discordia", "Could not unregister network monitor", error)
            }
        }
        networkCallback = null
        appUpdater?.cancel()
        voiceSlot = -1
        stopScreenShare()
        stopCameraCapture()
        documentRequest?.let { cancelDocument(it) }
        super.onDestroy()
    }

    // The page is served over https (dioxus.index.html) while the gateway mints the
    // LiveKit signaling URL as ws://, which Chromium blocks as mixed content. Desktop
    // dodges the same rule with --unsafely-treat-insecure-origin-as-secure.
    override fun onWebViewCreate(webView: WebView) {
        super.onWebViewCreate(webView)
        webView.settings.mixedContentMode = WebSettings.MIXED_CONTENT_ALWAYS_ALLOW
    }

    @Volatile var microphonePermissionResult: Int = 0
        private set

    fun requestMicrophonePermission() {
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            microphonePermissionResult = 1
            return
        }
        microphonePermissionResult = 0
        requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO), MICROPHONE_REQUEST)
    }

    @Volatile var cameraPermissionResult: Int = 0
        private set

    fun requestCameraPermission() {
        if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            cameraPermissionResult = 1
            return
        }
        cameraPermissionResult = 0
        requestPermissions(arrayOf(Manifest.permission.CAMERA), CAMERA_REQUEST)
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == MICROPHONE_REQUEST) {
            microphonePermissionResult = if (grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED) 1 else -1
        }
        if (requestCode == CAMERA_REQUEST) {
            cameraPermissionResult = if (grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED) 1 else -1
        }
    }

    companion object {
        private const val MICROPHONE_REQUEST = 601
        private const val CAMERA_REQUEST = 602
    }
}
