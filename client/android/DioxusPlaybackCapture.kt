package com.discordia.app

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.projection.MediaProjection
import java.util.concurrent.atomic.AtomicBoolean

class DioxusPlaybackCapture private constructor(
    private val recorder: AudioRecord,
    private val onSamples: (ShortArray) -> Boolean,
    private val onError: (String) -> Unit,
) {
    private val active = AtomicBoolean(true)
    private val reader = Thread({
        val samples = ShortArray(480)
        var filled = 0
        try {
            android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_AUDIO)
            while (active.get()) {
                val count = recorder.read(samples, filled, samples.size - filled, AudioRecord.READ_BLOCKING)
                if (!active.get()) break
                check(count > 0) { "Playback audio read failed ($count)" }
                filled += count
                if (filled == samples.size) {
                    filled = 0
                    if (!onSamples(samples)) break
                }
            }
        } catch (error: Exception) {
            if (active.getAndSet(false)) onError(error.message ?: "Playback audio stopped")
        } finally {
            active.set(false)
            recorder.release()
        }
    }, "discordia-playback")

    fun isActive(): Boolean = active.get()

    fun stop() {
        if (!active.getAndSet(false)) return
        try {
            recorder.stop()
        } catch (_: IllegalStateException) {
        }
        reader.interrupt()
    }

    companion object {
        fun start(
            projection: MediaProjection,
            onSamples: (ShortArray) -> Boolean,
            onError: (String) -> Unit,
        ): DioxusPlaybackCapture {
            check(android.os.Build.VERSION.SDK_INT >= 29) { "Playback capture requires Android 10" }
            val configuration = AudioPlaybackCaptureConfiguration.Builder(projection)
                .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
                .addMatchingUsage(AudioAttributes.USAGE_GAME)
                .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
                // Do not send incoming calls or our own sounds back to the channel.
                .excludeUid(android.os.Process.myUid())
                .build()
            val minimum = AudioRecord.getMinBufferSize(48000, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
            check(minimum > 0) { "Playback capture format is not supported" }
            val recorder = AudioRecord.Builder()
                .setAudioFormat(AudioFormat.Builder().setSampleRate(48000)
                    .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT).build())
                .setBufferSizeInBytes(maxOf(minimum, 3840))
                .setAudioPlaybackCaptureConfig(configuration)
                .build()
            try {
                check(recorder.state == AudioRecord.STATE_INITIALIZED) { "Playback capture could not initialize" }
                recorder.startRecording()
                check(recorder.recordingState == AudioRecord.RECORDSTATE_RECORDING) { "Playback capture could not start" }
                return DioxusPlaybackCapture(recorder, onSamples, onError).also { it.reader.start() }
            } catch (error: Exception) {
                recorder.release()
                throw error
            }
        }
    }
}
