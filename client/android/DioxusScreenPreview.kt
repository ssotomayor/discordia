package com.discordia.app

import android.app.Activity
import android.view.ViewGroup
import android.webkit.WebView
import android.widget.FrameLayout
import livekit.org.webrtc.EglBase
import livekit.org.webrtc.RendererCommon
import livekit.org.webrtc.SurfaceViewRenderer
import livekit.org.webrtc.VideoFrame
import org.json.JSONObject
import kotlin.math.roundToInt

class DioxusScreenPreview(private val activity: Activity, private val webView: () -> WebView?) {
    @Volatile private var renderer: SurfaceViewRenderer? = null
    @Volatile private var slot = -1
    private var context: EglBase.Context? = null
    private var bounds: JSONObject? = null
    private var revision = -1L
    private var foreground = true

    fun configure(payload: String) {
        if (payload.length > 1024) return
        activity.runOnUiThread {
            val value = try { JSONObject(payload) } catch (_: Exception) { return@runOnUiThread }
            val next = value.optLong("revision", -1)
            if (next < revision || next < 0) return@runOnUiThread
            revision = next
            bounds = value.takeIf { it.optBoolean("visible", false) }
            applyBounds()
        }
    }

    fun start(slot: Int, context: EglBase.Context) {
        releaseRenderer()
        this.slot = slot
        this.context = context
        applyBounds()
    }

    fun frame(slot: Int, frame: VideoFrame) {
        val current = renderer
        if (slot != this.slot || current == null) return
        try { current.onFrame(frame) } catch (error: RuntimeException) {
            activity.runOnUiThread { if (renderer === current) releaseRenderer() }
            android.util.Log.w("Discordia", "Local preview frame failed", error)
        }
    }

    fun setForeground(value: Boolean) {
        foreground = value
        applyBounds()
    }

    fun stop() {
        slot = -1
        releaseRenderer()
        context = null
    }

    private fun releaseRenderer() {
        val previous = renderer
        renderer = null
        try { previous?.release() } catch (error: RuntimeException) {
            android.util.Log.w("Discordia", "Local preview release failed", error)
        }
        (previous?.parent as? ViewGroup)?.removeView(previous)
    }

    private fun applyBounds() {
        val value = bounds
        val egl = context
        val web = webView()
        if (!foreground || slot < 0 || value == null || egl == null || web == null) {
            releaseRenderer()
            return
        }
        val viewportWidth = value.optDouble("viewportWidth", 0.0)
        val viewportHeight = value.optDouble("viewportHeight", 0.0)
        val left = value.optDouble("left", -1.0)
        val top = value.optDouble("top", -1.0)
        val width = value.optDouble("width", 0.0)
        val height = value.optDouble("height", 0.0)
        if (!listOf(viewportWidth, viewportHeight, left, top, width, height).all { it.isFinite() }
            || viewportWidth < 1 || viewportHeight < 1 || viewportWidth > 32768 || viewportHeight > 32768 || width <= 0 || height <= 0
            || left < -0.5 || top < -0.5 || left + width > viewportWidth + 0.5 || top + height > viewportHeight + 0.5
            || web.width <= 0 || web.height <= 0) {
            releaseRenderer()
            return
        }
        val root = activity.findViewById<FrameLayout>(android.R.id.content) ?: return
        val webPosition = IntArray(2)
        val rootPosition = IntArray(2)
        web.getLocationInWindow(webPosition)
        root.getLocationInWindow(rootPosition)
        val scaleX = web.width / viewportWidth
        val scaleY = web.height / viewportHeight
        try {
            var view = renderer
            if (view == null) {
                view = SurfaceViewRenderer(activity)
                renderer = view
                view.init(egl, null)
                view.setScalingType(RendererCommon.ScalingType.SCALE_ASPECT_FIT)
                view.setFpsReduction(15f)
                view.setEnableHardwareScaler(true)
                view.setZOrderOnTop(true)
                view.isClickable = false
                root.addView(view)
            }
            val layout = FrameLayout.LayoutParams((width * scaleX).roundToInt().coerceIn(1, web.width), (height * scaleY).roundToInt().coerceIn(1, web.height))
            layout.leftMargin = webPosition[0] - rootPosition[0] + (left * scaleX).roundToInt()
            layout.topMargin = webPosition[1] - rootPosition[1] + (top * scaleY).roundToInt()
            view.layoutParams = layout
        } catch (error: Exception) {
            releaseRenderer()
            android.util.Log.w("Discordia", "Local screen preview is unavailable", error)
        }
    }
}
