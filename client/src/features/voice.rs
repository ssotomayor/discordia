//! Voice service. Owns the LiveKit Room, the microphone capture pipeline,
//! and the remote-audio playback pipeline.
//!
//! LiveKit is a libwebrtc-based SFU. Connecting to a room gives us libwebrtc's
//! AEC3 / NS / AGC / Opus / congestion control end-to-end. The Rust SDK
//! exposes only PCM frames at the edges; cpal handles the actual mic and
//! speaker devices.

use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use dioxus::prelude::*;
use futures_util::StreamExt;
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::prelude::RtcAudioSource;
use parking_lot::Mutex;
use rubato::{FftFixedIn, Resampler};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::protocol::Id;
use crate::state::{AppState, VoicePhase};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u32 = 1;
const FRAME_MS: u32 = 10;
const FRAME_SAMPLES: usize = (SAMPLE_RATE / 1000 * FRAME_MS) as usize;

/// Chunk size for the rubato resampler. Must be a power-of-two-friendly value
/// for FFT efficiency. 512 is a good balance of latency (~10ms @ 48k) and CPU.
const RESAMPLER_CHUNK: usize = 512;

/// Playback jitter buffer cap, as a divisor of the device sample rate: 5 =>
/// ~200ms. Must stay above the drift-compensation overrun threshold (150ms) so
/// the two mechanisms don't fight — see `PlaybackMixer::new`.
const PLAYBACK_CAP_DIVISOR: u32 = 5;

pub enum VoiceCmd {
    Connect {
        livekit_url: String,
        token: String,
        channel_id: Id,
    },
    Disconnect,
    /// Request the voice service to enumerate devices and populate AppState lists.
    ListDevices,
    /// Set desired devices (names). None = leave unchanged / use default.
    SetDevices {
        input: Option<String>,
        output: Option<String>,
    },
    SetMute {
        muted: bool,
    },
    /// Set the microphone speaking-detection threshold (1..=200).
    SetSensitivity {
        threshold: u32,
    },
}

/// Convenience wrapper provided via Dioxus context for UI components to send
/// voice commands without needing to plumb the channel themselves.
#[derive(Clone)]
pub struct VoiceTx(pub UnboundedSender<VoiceCmd>);

impl VoiceTx {
    pub fn send(&self, cmd: VoiceCmd) {
        let _ = self.0.send(cmd);
    }
}

pub fn use_voice_tx() -> VoiceTx {
    use_context::<VoiceTx>()
}

/// Spawn the voice service. Returns the command sender.
pub fn spawn_voice_service(state: Signal<AppState>) -> UnboundedSender<VoiceCmd> {
    let (tx, rx) = unbounded_channel::<VoiceCmd>();
    spawn(async move {
        if let Err(e) = service_loop(rx, state).await {
            tracing::error!(error = %e, "voice service loop ended");
        }
    });
    tx
}

async fn service_loop(
    mut rx: UnboundedReceiver<VoiceCmd>,
    mut state: Signal<AppState>,
) -> Result<(), String> {
    let mut session: Option<ActiveVoice> = None;
    // Remember last connect parameters so we can reconnect on device changes.
    let mut last_connect: Option<(String, String, Id)> = None;

    while let Some(cmd) = rx.recv().await {
        match cmd {
            VoiceCmd::Connect {
                livekit_url,
                token,
                channel_id,
            } => {
                eprintln!("[voice] Connect to {livekit_url} channel={channel_id}");
                // store params for possible reconnects later
                last_connect = Some((livekit_url.clone(), token.clone(), channel_id));
                if let Some(prev) = session.take() {
                    eprintln!("[voice] shutting down previous session");
                    prev.shutdown().await;
                }
                match ActiveVoice::connect(&livekit_url, &token, channel_id, state.clone()).await {
                    Ok(active) => {
                        eprintln!("[voice] connected ok");
                        state.write().voice.phase = VoicePhase::Connected;
                        session = Some(active);
                    }
                    Err(e) => {
                        eprintln!("[voice] connect FAILED: {e}");
                        let mut s = state.write();
                        s.voice.phase = VoicePhase::Error;
                        s.voice.error = Some(e);
                        s.voice.channel_id = None;
                    }
                }
            }
            VoiceCmd::Disconnect => {
                eprintln!("[voice] Disconnect");
                if let Some(prev) = session.take() {
                    prev.shutdown().await;
                }
                let mut s = state.write();
                s.voice.phase = VoicePhase::Idle;
                s.voice.channel_id = None;
                s.voice.error = None;
                // clear last_connect — user intentionally disconnected
                last_connect = None;
            }
            VoiceCmd::ListDevices => {
                eprintln!("[voice] ListDevices request");
                // Enumerate devices and populate AppState lists.
                let host = cpal::default_host();
                let mut inputs = Vec::new();
                let mut outputs = Vec::new();
                if let Ok(devs) = host.devices() {
                    for d in devs {
                        if let Ok(name) = d.name() {
                            let is_input = d.default_input_config().is_ok();
                            let is_output = d.default_output_config().is_ok();
                            if is_input {
                                inputs.push(name.clone());
                            }
                            if is_output {
                                outputs.push(name);
                            }
                        }
                    }
                }
                let mut s = state.write();
                s.available_input_devices = inputs;
                s.available_output_devices = outputs;
            }
            VoiceCmd::SetDevices { input, output } => {
                eprintln!("[voice] SetDevices input={:?} output={:?}", input, output);
                // Update AppState selections in a tight scope so the write lock
                // is dropped before we attempt to reconnect (which awaits).
                {
                    let mut s = state.write();
                    if let Some(i) = input.clone() {
                        s.selected_input_device = Some(i);
                    }
                    if let Some(o) = output.clone() {
                        s.selected_output_device = Some(o);
                    }
                }
                // If we're currently connected, restart the voice session so mic/playback
                // are recreated using the newly selected devices.
                if session.is_some() {
                    if let Some((ref url, ref tok, cid)) = last_connect.clone() {
                        eprintln!("[voice] Reconnecting to apply device changes");
                        if let Some(prev) = session.take() {
                            prev.shutdown().await;
                        }
                        match ActiveVoice::connect(url, tok, cid, state.clone()).await {
                            Ok(active) => {
                                eprintln!("[voice] reconnected ok");
                                // update phase in its own small scope
                                {
                                    let mut s = state.write();
                                    s.voice.phase = VoicePhase::Connected;
                                }
                                session = Some(active);
                            }
                            Err(e) => {
                                eprintln!("[voice] reconnect FAILED: {e}");
                                let mut s = state.write();
                                s.voice.phase = VoicePhase::Error;
                                s.voice.error = Some(format!("reconnect after device change: {e}"));
                                s.voice.channel_id = None;
                            }
                        }
                    }
                }
            }
            VoiceCmd::SetMute { muted } => {
                eprintln!("[voice] SetMute muted={muted}");
                if let Some(active) = session.as_mut() {
                    active.set_muted(muted).await;
                }
                state.write().voice.muted = muted;
            }
            VoiceCmd::SetSensitivity { threshold } => {
                eprintln!("[voice] SetSensitivity threshold={threshold}");
                state.write().mic_sensitivity = threshold;
            }
        }
    }
    eprintln!("[voice] service loop ended (channel closed)");
    Ok(())
}

/// Active voice session: holds the LiveKit Room plus the audio I/O streams.
struct ActiveVoice {
    room: Arc<Room>,
    mic: MicCapture,
    local_audio: LocalAudioTrack,
    _playback: PlaybackMixer,
    event_task: tokio::task::JoinHandle<()>,
}

impl ActiveVoice {
    async fn connect(
        livekit_url: &str,
        token: &str,
        _channel_id: Id,
        state: Signal<AppState>,
    ) -> Result<Self, String> {
        let (room, mut events) = Room::connect(livekit_url, token, RoomOptions::default())
            .await
            .map_err(|e| format!("livekit connect: {e}"))?;
        let room = Arc::new(room);

        // Microphone publish pipeline. APM (AEC + NS + AGC) is enabled.
        // Same-machine testing: AEC will be a pass-through because we don't
        // wire a render reference signal to libwebrtc — but NS and AGC still
        // help. Real two-machine deployments get the full benefit.
        let source = NativeAudioSource::new(
            AudioSourceOptions {
                echo_cancellation: true,
                noise_suppression: true,
                auto_gain_control: true,
            },
            SAMPLE_RATE,
            CHANNELS,
            1000,
        );
        let local_audio =
            LocalAudioTrack::create_audio_track("mic", RtcAudioSource::Native(source.clone()));
        let local_audio_for_mute = local_audio.clone();
        room.local_participant()
            .publish_track(
                LocalTrack::Audio(local_audio),
                TrackPublishOptions {
                    source: TrackSource::Microphone,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("publish mic: {e}"))?;

        // `NativeAudioSource::capture_frame` is async — calling it from the
        // sync cpal audio thread and dropping the future means it never
        // runs. Funnel frames through an mpsc channel to a tokio task that
        // properly awaits.
        let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let publish_source = source.clone();
        tokio::spawn(async move {
            let mut sent = 0u64;
            while let Some(samples) = frame_rx.recv().await {
                let frame = AudioFrame {
                    data: samples.into(),
                    sample_rate: SAMPLE_RATE,
                    num_channels: CHANNELS,
                    samples_per_channel: FRAME_SAMPLES as u32,
                };
                if let Err(e) = publish_source.capture_frame(&frame).await {
                    eprintln!("[voice] capture_frame error: {e:?}");
                }
                sent += 1;
                if sent % 500 == 0 {
                    eprintln!("[voice] publish: {sent} frames forwarded to libwebrtc");
                }
            }
            eprintln!("[voice] publish task ended after {sent} frames");
        });
        let mic = MicCapture::start(frame_tx, state.clone())?;

        // Remote audio mixer.
        let playback = PlaybackMixer::start(state.clone())?;
        let mixer_handle = playback.handle();

        // Event task: subscribe to room events, hook up remote tracks.
        let event_task = tokio::spawn({
            let mixer_handle = mixer_handle.clone();
            async move {
                while let Some(ev) = events.recv().await {
                    match &ev {
                        RoomEvent::ParticipantConnected(p) => {
                            eprintln!("[voice] participant connected: {}", p.identity().0);
                        }
                        RoomEvent::ParticipantDisconnected(p) => {
                            eprintln!("[voice] participant left: {}", p.identity().0);
                        }
                        RoomEvent::TrackPublished {
                            participant,
                            publication,
                        } => {
                            eprintln!(
                                "[voice] track published by {}: {:?}",
                                participant.identity().0,
                                publication.kind()
                            );
                        }
                        RoomEvent::TrackSubscribed {
                            track, participant, ..
                        } => {
                            eprintln!(
                                "[voice] track SUBSCRIBED from {}: kind={:?}",
                                participant.identity().0,
                                track.kind()
                            );
                        }
                        RoomEvent::TrackUnsubscribed { participant, .. } => {
                            eprintln!(
                                "[voice] track unsubscribed from {}",
                                participant.identity().0
                            );
                        }
                        RoomEvent::Disconnected { reason } => {
                            eprintln!("[voice] room disconnected: {reason:?}");
                        }
                        _ => {}
                    }
                    if let RoomEvent::TrackSubscribed { track, .. } = ev {
                        if let RemoteTrack::Audio(audio) = track {
                            let stream = NativeAudioStream::new(
                                audio.rtc_track(),
                                SAMPLE_RATE as i32,
                                CHANNELS as i32,
                            );
                            let mixer_handle = mixer_handle.clone();
                            tokio::spawn(consume_remote_track(stream, mixer_handle));
                        }
                    }
                }
                eprintln!("[voice] event stream ended");
            }
        });

        Ok(Self {
            room,
            mic,
            local_audio: local_audio_for_mute,
            _playback: playback,
            event_task,
        })
    }

    async fn set_muted(&mut self, muted: bool) {
        self.mic.set_muted(muted);
        // Disabling the track stops audio from being sent and signals other
        // peers via the muted-track event.
        self.local_audio.rtc_track().set_enabled(!muted);
    }

    async fn shutdown(self) {
        self.event_task.abort();
        self.mic.stop();
        // Dropping the Arc<Room> triggers disconnect.
        drop(self.room);
    }
}

// ---------------------------------------------------------------------------
// Microphone capture: cpal -> AudioFrame -> NativeAudioSource
// ---------------------------------------------------------------------------

struct MicCapture {
    _stream: cpal::Stream,
    muted: Arc<Mutex<bool>>,
}

impl MicCapture {
    fn start(
        frame_tx: tokio::sync::mpsc::UnboundedSender<Vec<i16>>,
        mut state: Signal<AppState>,
    ) -> Result<Self, String> {
        let host = cpal::default_host();
        // Prefer user-selected device (by name) if present in AppState.
        let selected = state.read().selected_input_device.clone();
        let device = if let Some(sel_name) = selected {
            // Try to find a device whose name matches the selected name.
            let mut found = None;
            if let Ok(devs) = host.input_devices() {
                for d in devs {
                    if let Ok(name) = d.name() {
                        if name == sel_name {
                            found = Some(d);
                            break;
                        }
                    }
                }
            }
            found.unwrap_or_else(|| {
                host.default_input_device()
                    .expect("no default input device")
            })
        } else {
            host.default_input_device()
                .ok_or_else(|| "no default input device".to_string())?
        };
        let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());
        let config = device
            .default_input_config()
            .map_err(|e| format!("input config: {e}"))?;
        let sample_format = config.sample_format();
        let device_rate = config.sample_rate().0;
        let device_channels = config.channels() as u32;
        eprintln!(
            "[voice] mic: device={device_name} format={sample_format:?} rate={device_rate} ch={device_channels}"
        );

        let muted = Arc::new(Mutex::new(false));
        let muted_for_cb = muted.clone();

        let raw_peak = Arc::new(std::sync::atomic::AtomicI32::new(0));
        let raw_peak_cb = raw_peak.clone();
        let frames_pushed = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let frames_pushed_cb = frames_pushed.clone();
        let speak_peak = Arc::new(std::sync::atomic::AtomicI32::new(0));
        let speak_peak_cb = speak_peak.clone();

        // Carry resampled samples across cpal callbacks so each frame we
        // hand to libwebrtc is always exactly `FRAME_SAMPLES` long.
        let accum: Arc<Mutex<Vec<f32>>> =
            Arc::new(Mutex::new(Vec::with_capacity(FRAME_SAMPLES * 4)));

        // High-quality resampler (rubato FFT). None if device already runs at
        // SAMPLE_RATE (48kHz) — most common case on macOS CoreAudio.
        let resampler = Arc::new(Mutex::new(AudioResampler::new(device_rate, SAMPLE_RATE)));
        if resampler.lock().is_some() {
            eprintln!("[voice] mic: resampling {device_rate}Hz → {SAMPLE_RATE}Hz via rubato");
        }

        let err = |e| eprintln!("mic stream error: {e}");
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let accum = accum.clone();
                let frame_tx = frame_tx.clone();
                let resampler_cb = resampler.clone();
                device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _| {
                        update_peak(&raw_peak_cb, data);
                        update_peak(&speak_peak_cb, data);
                        let pushed = forward_mic(
                            &frame_tx,
                            data,
                            device_rate,
                            device_channels,
                            &muted_for_cb,
                            &accum,
                            &resampler_cb,
                        );
                        frames_pushed_cb
                            .fetch_add(pushed as u64, std::sync::atomic::Ordering::Relaxed);
                    },
                    err,
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let accum = accum.clone();
                let frame_tx = frame_tx.clone();
                let resampler_cb = resampler.clone();
                device.build_input_stream(
                    &config.into(),
                    move |data: &[i16], _| {
                        let f32_buf: Vec<f32> =
                            data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                        update_peak(&raw_peak_cb, &f32_buf);
                        update_peak(&speak_peak_cb, &f32_buf);
                        let pushed = forward_mic(
                            &frame_tx,
                            &f32_buf,
                            device_rate,
                            device_channels,
                            &muted_for_cb,
                            &accum,
                            &resampler_cb,
                        );
                        frames_pushed_cb
                            .fetch_add(pushed as u64, std::sync::atomic::Ordering::Relaxed);
                    },
                    err,
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let accum = accum.clone();
                let frame_tx = frame_tx.clone();
                let resampler_cb = resampler.clone();
                device.build_input_stream(
                    &config.into(),
                    move |data: &[u16], _| {
                        let f32_buf: Vec<f32> = data
                            .iter()
                            .map(|s| (*s as f32 - 32768.0) / 32768.0)
                            .collect();
                        update_peak(&raw_peak_cb, &f32_buf);
                        update_peak(&speak_peak_cb, &f32_buf);
                        let pushed = forward_mic(
                            &frame_tx,
                            &f32_buf,
                            device_rate,
                            device_channels,
                            &muted_for_cb,
                            &accum,
                            &resampler_cb,
                        );
                        frames_pushed_cb
                            .fetch_add(pushed as u64, std::sync::atomic::Ordering::Relaxed);
                    },
                    err,
                    None,
                )
            }
            other => return Err(format!("unsupported sample format: {other:?}")),
        }
        .map_err(|e| format!("build_input_stream: {e}"))?;

        stream.play().map_err(|e| format!("play mic: {e}"))?;

        // Heartbeat — reports loudest raw sample seen since last tick.
        let peak_log = raw_peak.clone();
        let frames_log = frames_pushed.clone();
        tokio::spawn(async move {
            let mut prev_frames = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let p = peak_log.swap(0, std::sync::atomic::Ordering::Relaxed) as f32 / 1_000.0;
                let f = frames_log.load(std::sync::atomic::Ordering::Relaxed);
                let level = if p < 0.001 {
                    "silent"
                } else if p < 0.01 {
                    "very quiet"
                } else {
                    "speaking"
                };
                eprintln!(
                    "[voice] mic heartbeat: raw peak={p:.4} ({level}), frames pushed to webrtc={f} (+{})",
                    f - prev_frames
                );
                prev_frames = f;
            }
        });
        // Speaking indicator: sample every 150ms with a short hangover so the dot
        // doesn't flicker between words/breaths. The threshold is read live from
        // AppState so the audio-settings slider takes effect immediately.
        dioxus::prelude::spawn(async move {
            const HANGOVER_TICKS: u32 = 4; // ~600ms tras el último sonido fuerte
            let mut hangover = 0u32;
            let mut currently_speaking = false;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                let p = speak_peak.swap(0, std::sync::atomic::Ordering::Relaxed);
                let threshold = state.read().mic_sensitivity as i32;
                // Publish the live mic level so the audio-settings VU bar can render
                // it alongside the threshold marker. Clamp to the 0..=1000 range the
                // UI expects (peak is stored as ×1000 fixed-point).
                state.write().mic_level = p.clamp(0, 1000) as u32;
                if p > threshold {
                    hangover = HANGOVER_TICKS;
                } else if hangover > 0 {
                    hangover -= 1;
                }
                let should_speak = hangover > 0;
                if should_speak != currently_speaking {
                    currently_speaking = should_speak;
                    state.write().voice.speaking = should_speak;
                }
            }
        });
        Ok(Self {
            _stream: stream,
            muted,
        })
    }

    fn set_muted(&self, muted: bool) {
        *self.muted.lock() = muted;
    }

    fn stop(self) {
        drop(self._stream);
    }
}

fn forward_mic(
    frame_tx: &tokio::sync::mpsc::UnboundedSender<Vec<i16>>,
    samples: &[f32],
    _device_rate: u32,
    device_channels: u32,
    muted: &Arc<Mutex<bool>>,
    accum: &Arc<Mutex<Vec<f32>>>,
    resampler: &Arc<Mutex<Option<AudioResampler>>>,
) -> usize {
    if *muted.lock() {
        return 0;
    }
    let mono: Vec<f32> = samples
        .chunks(device_channels as usize)
        .map(|c| c.iter().copied().sum::<f32>() / c.len() as f32)
        .collect();
    // High-quality resampling via rubato (FFT + anti-aliasing). Falls back to
    // passthrough if no resampler is needed (device already at SAMPLE_RATE).
    let resampled = {
        let mut rs = resampler.lock();
        match rs.as_mut() {
            Some(r) => r.process(&mono),
            None => mono,
        }
    };

    let mut buf = accum.lock();
    buf.extend(resampled);

    let mut pushed = 0usize;
    while buf.len() >= FRAME_SAMPLES {
        // No dither here: these frames go straight into Opus. Dither is for the
        // final quantization to an output device — feeding noise to a lossy
        // encoder only costs bitrate.
        let chunk: Vec<i16> = buf
            .drain(..FRAME_SAMPLES)
            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        if frame_tx.send(chunk).is_err() {
            break;
        }
        pushed += 1;
    }
    pushed
}

fn update_peak(peak: &Arc<std::sync::atomic::AtomicI32>, samples: &[f32]) {
    let mut local_max: f32 = 0.0;
    for &s in samples {
        let a = s.abs();
        if a > local_max {
            local_max = a;
        }
    }
    // Store as fixed-point (×1000) so we can use AtomicI32.
    let local_i = (local_max * 1_000.0) as i32;
    let mut current = peak.load(std::sync::atomic::Ordering::Relaxed);
    while local_i > current {
        match peak.compare_exchange_weak(
            current,
            local_i,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(c) => current = c,
        }
    }
}

/// High-quality resampler wrapping rubato's FFT-based synchronous resampler.
/// Replaces the old `naive_resample` (linear interpolation without anti-
/// aliasing) which introduced metallic artefacts on frequencies above ~4kHz
/// when the device sample rate differed from LiveKit's 48kHz.
///
/// rubato is pure Rust (no C deps), supports Windows MSVC + macOS (Intel &
/// Apple Silicon) + Linux, and auto-detects SIMD (AVX/SSE/Neon) at runtime.
/// `process_into_buffer` itself does not allocate, but `process` below still
/// returns a fresh `Vec` per call, so this is *not* strictly realtime-safe —
/// the mic path calls it from cpal's callback. The internal buffers are reused
/// to keep that to one allocation; removing it entirely needs a caller-provided
/// output buffer on both call sites.
///
/// The resampler is stateful (it keeps the anti-aliasing filter state between
/// calls), so one instance must live for the whole voice session. Input is
/// buffered until a full `RESAMPLER_CHUNK` is available; the leftover tail
/// stays in `input_accum` for the next call.
struct AudioResampler {
    inner: FftFixedIn<f32>,
    /// Pending input frames not yet enough for a full resampler chunk.
    input_accum: Vec<f32>,
    /// Reusable input chunk handed to rubato, so the per-chunk `drain().
    /// collect()` doesn't allocate on the mic's realtime thread.
    chunk_in: Vec<f32>,
    /// Reusable output scratch. `process_into_buffer` requires this to be
    /// pre-sized to at least `output_frames_next()` — it validates the length
    /// (not the capacity) and refuses to write into a short buffer. Keeping it
    /// alive across calls also avoids allocating on the mic's realtime thread.
    scratch_out: Vec<f32>,
}

impl AudioResampler {
    /// Create a resampler for mono audio. Returns None if `from == to` (no
    /// resampling needed — caller passes samples through unchanged).
    fn new(from_rate: u32, to_rate: u32) -> Option<Self> {
        if from_rate == to_rate {
            return None;
        }
        let inner =
            FftFixedIn::<f32>::new(from_rate as usize, to_rate as usize, RESAMPLER_CHUNK, 2, 1)
                .map_err(|e| eprintln!("[voice] rubato resampler init failed: {e:?}"))
                .ok()?;
        let max_out = inner.output_frames_max();
        Some(Self {
            inner,
            input_accum: Vec::with_capacity(RESAMPLER_CHUNK * 2),
            chunk_in: Vec::with_capacity(RESAMPLER_CHUNK),
            scratch_out: vec![0.0; max_out],
        })
    }

    /// Push raw input samples and return all resampled output available.
    /// Feeds the internal resampler in chunks of `RESAMPLER_CHUNK` frames;
    /// leftover input is kept for the next call.
    fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.input_accum.extend_from_slice(input);

        // Destructure so the borrow checker sees the fields as independent —
        // `inner` is borrowed mutably while the buffers are borrowed too.
        let Self {
            inner,
            input_accum,
            chunk_in,
            scratch_out,
        } = self;

        let mut out = Vec::new();
        while input_accum.len() >= RESAMPLER_CHUNK {
            chunk_in.clear();
            chunk_in.extend(input_accum.drain(..RESAMPLER_CHUNK));
            // The output slice must already be `output_frames_next()` long.
            // Handing rubato an empty Vec makes every call fail validation with
            // `InsufficientOutputBufferSize`, which silently produced zero
            // samples — i.e. total silence on any device not already at 48kHz.
            let need = inner.output_frames_next();
            if scratch_out.len() < need {
                scratch_out.resize(need, 0.0);
            }
            // rubato takes one buffer per channel; we're mono.
            let waves_in = [&chunk_in[..]];
            let mut waves_out = [&mut scratch_out[..]];
            match inner.process_into_buffer(&waves_in, &mut waves_out, None) {
                Ok((_, produced)) => out.extend_from_slice(&scratch_out[..produced]),
                Err(e) => eprintln!("[voice] rubato process error: {e:?}"),
            }
        }
        out
    }
}

/// xorshift32 — a handful of instructions, no thread-local lookup and no
/// locking, so it is safe to call from an audio callback. `rand::random()`
/// reaches for a thread-local `ThreadRng` on every call, which at 48kHz stereo
/// meant ~96k of those per second inside cpal's realtime thread. Dither only
/// needs uniform noise, not cryptographic quality.
#[inline]
fn xorshift32(state: &mut u32) -> f32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    // Top 24 bits -> [0, 1). f32 has 24 bits of mantissa, so this is the most
    // resolution the type can carry anyway.
    (x >> 8) as f32 / (1u32 << 24) as f32
}

/// Seed for the dither RNG. Any non-zero value works; xorshift is a fixed point
/// at zero and would emit a constant.
const DITHER_SEED: u32 = 0x9E37_79B9;

/// TPDF dither + quantization to i16. Reduces quantization distortion on
/// quiet signals (the classic "metallic hiss" at low bit depths). TPDF
/// (triangular probability density function) is the standard choice — it
/// eliminates noise modulation, unlike uniform (RPDF) dither.
///
/// Only worth doing on the *final* conversion to the output device. Dithering
/// audio on its way into a lossy encoder just spends bitrate on noise.
#[inline]
fn dither_to_i16(sample: f32, rng: &mut u32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    let r1 = xorshift32(rng);
    let r2 = xorshift32(rng);
    // TPDF: (r1 + r2 - 1) gives a triangular distribution centered at 0.
    // Amplitude: 1 LSB peak-to-peak (2/i16::MAX).
    let dither = (r1 + r2 - 1.0) * (1.0 / i16::MAX as f32);
    ((clamped + dither) * i16::MAX as f32) as i16
}

// ---------------------------------------------------------------------------
// Playback mixer: NativeAudioStream -> ring buffer -> cpal output
// ---------------------------------------------------------------------------

/// One jitter buffer per subscribed remote track, summed by the output
/// callback.
///
/// These must be per-track. Previously every remote participant shared a single
/// buffer and a single resampler, so with two or more speakers their samples
/// were *appended* to one timeline rather than mixed, and the resampler — which
/// is stateful (FFT overlap plus a partial-chunk accumulator) — had its filter
/// state corrupted by interleaved chunks from different people. That is why the
/// audio degraded when a third participant joined and stayed broken after
/// people left: the corrupted state outlived them.
#[derive(Default)]
struct MixerTracks {
    buffers: std::collections::HashMap<u64, std::collections::VecDeque<f32>>,
    next_id: u64,
}

#[derive(Clone)]
struct PlaybackHandle {
    tracks: Arc<Mutex<MixerTracks>>,
    device_rate: u32,
}

impl PlaybackHandle {
    /// Claim a jitter buffer for one remote track. The id is opaque; the caller
    /// hands it back to `push`/`remove_track`.
    fn add_track(&self) -> u64 {
        let mut t = self.tracks.lock();
        let id = t.next_id;
        t.next_id = t.next_id.wrapping_add(1);
        t.buffers
            .insert(id, std::collections::VecDeque::with_capacity(SAMPLE_RATE as usize / 2));
        id
    }

    fn push(&self, id: u64, samples: &[f32], cap: usize) {
        let mut t = self.tracks.lock();
        if let Some(buf) = t.buffers.get_mut(&id) {
            buf.extend(samples);
            while buf.len() > cap {
                buf.pop_front();
            }
        }
    }

    fn remove_track(&self, id: u64) {
        self.tracks.lock().buffers.remove(&id);
    }
}

/// Pop one sample from a track's jitter buffer, nudging its depth back toward
/// the target by skipping or duplicating a sample occasionally — a cheap
/// time-stretch that doesn't shift pitch.
#[inline]
fn pop_drift_compensated(
    buf: &mut std::collections::VecDeque<f32>,
    counter: u32,
    overrun: usize,
    underrun: usize,
) -> f32 {
    let len = buf.len();
    if len > overrun {
        // Too full — drop every 32nd sample to shrink it without clicks.
        if counter % 32 == 0 {
            buf.pop_front();
        }
        buf.pop_front().unwrap_or(0.0)
    } else if len < underrun {
        // Too empty — repeat every 64th sample to stretch it.
        if counter % 64 == 0 {
            buf.front().copied().unwrap_or(0.0)
        } else {
            buf.pop_front().unwrap_or(0.0)
        }
    } else {
        buf.pop_front().unwrap_or(0.0)
    }
}

struct PlaybackMixer {
    _stream: cpal::Stream,
    handle: PlaybackHandle,
}

impl PlaybackMixer {
    fn start(state: Signal<AppState>) -> Result<Self, String> {
        let host = cpal::default_host();
        // Try to honour user-selected output device name if present.
        let selected = state.read().selected_output_device.clone();
        let device = if let Some(sel_name) = selected {
            let mut found = None;
            if let Ok(devs) = host.output_devices() {
                for d in devs {
                    if let Ok(name) = d.name() {
                        if name == sel_name {
                            found = Some(d);
                            break;
                        }
                    }
                }
            }
            found.unwrap_or_else(|| {
                host.default_output_device()
                    .expect("no default output device")
            })
        } else {
            host.default_output_device()
                .ok_or_else(|| "no default output device".to_string())?
        };
        let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());
        let config = device
            .default_output_config()
            .map_err(|e| format!("output config: {e}"))?;
        let device_channels = config.channels() as usize;
        let sample_format = config.sample_format();
        let device_rate = config.sample_rate().0;
        eprintln!(
            "[voice] playback: device={device_name} format={sample_format:?} rate={device_rate} ch={device_channels}"
        );

        // VecDeque for O(1) pop_front — Vec::remove(0) is O(n) and was
        // starving the audio callback at our buffer sizes.
        let buffer = Arc::new(Mutex::new(
            std::collections::VecDeque::<f32>::with_capacity(SAMPLE_RATE as usize),
        ));
        let buffer_cb = buffer.clone();
        let tracks = Arc::new(Mutex::new(MixerTracks::default()));
        let tracks_cb = tracks.clone();
        let cb_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cb_counter_cb = cb_counter.clone();
        let pulled_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let pulled_cb = pulled_counter.clone();

        // Drift compensation counter — cycles 0..255 to decide when to
        // skip/duplicate a sample for smooth buffer size adjustment.
        let drift_counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let drift_counter_cb = drift_counter.clone();
        let device_rate_cb = device_rate;
        // Dither PRNG state. Moved into the i16 callback so it advances across
        // invocations (a per-callback reseed would emit the same noise pattern
        // every buffer, which is audible as a tone).
        let mut dither_rng = DITHER_SEED;

        let err = |e| eprintln!("output stream error: {e}");
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config.into(),
                move |data: &mut [f32], _| {
                    cb_counter_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let mut tracks = tracks_cb.lock();
                    let mut pulled = 0u64;
                    // Drift compensation thresholds (in samples at device rate).
                    // The overrun mark must stay under the producer-side cap
                    // (PLAYBACK_CAP_DIVISOR, ~200ms) or it can never be reached
                    // and the branch is dead.
                    let overrun_threshold = (device_rate_cb as f64 * 0.15) as usize; // 150ms
                    let underrun_threshold = (device_rate_cb as f64 * 0.05) as usize; // 50ms
                    let mut counter = drift_counter_cb.load(std::sync::atomic::Ordering::Relaxed);
                    for frame in data.chunks_mut(device_channels) {
                        counter = counter.wrapping_add(1);
                        // Re-read the depth each frame: it shrinks as we drain,
                        // so a value hoisted out of the loop goes stale and
                        // keeps correcting long after the drift is gone.
                        let buf_len = buf.len();
                        let sample = if buf_len > overrun_threshold {
                            // Buffer too full — skip every 32nd sample to shrink
                            // it smoothly without audible clicks.
                            if counter % 32 == 0 {
                                buf.pop_front();
                            }
                            buf.pop_front().unwrap_or(0.0)
                        } else if buf_len < underrun_threshold {
                            // Buffer too empty — duplicate every 64th sample to
                            // stretch without pitch change or clicks.
                            if counter % 64 == 0 {
                                buf.front().copied().unwrap_or(0.0)
                            } else {
                                buf.pop_front().unwrap_or(0.0)
                            }
                        } else {
                            buf.pop_front().unwrap_or(0.0)
                        };
                        // Sum every active track. Each keeps its own depth, so
                        // drift is corrected per speaker rather than globally.
                        let mut acc = 0.0f32;
                        for buf in tracks.buffers.values_mut() {
                            acc += pop_drift_compensated(
                                buf,
                                counter,
                                overrun_threshold,
                                underrun_threshold,
                            );
                        }
                        // Limit the mix, not the individual tracks: several
                        // people talking at once can exceed full scale even
                        // when each of them is comfortably within range.
                        let sample = acc.tanh();
                        if sample != 0.0 {
                            pulled += 1;
                        }
                        for s in frame.iter_mut() {
                            *s = sample;
                        }
                    }
                    drift_counter_cb.store(counter, std::sync::atomic::Ordering::Relaxed);
                    pulled_cb.fetch_add(pulled, std::sync::atomic::Ordering::Relaxed);
                },
                err,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config.into(),
                // Dither RNG state, owned by the callback — no shared state, no
                // lock, no thread-local lookup on the realtime thread.
                move |data: &mut [i16], _| {
                    cb_counter_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let mut tracks = tracks_cb.lock();
                    let overrun_threshold = (device_rate_cb as f64 * 0.15) as usize;
                    let underrun_threshold = (device_rate_cb as f64 * 0.05) as usize;
                    let mut counter = drift_counter_cb.load(std::sync::atomic::Ordering::Relaxed);
                    for frame in data.chunks_mut(device_channels) {
                        counter = counter.wrapping_add(1);
                        let buf_len = buf.len();
                        let sample = if buf_len > overrun_threshold {
                            if counter % 32 == 0 {
                                buf.pop_front();
                            }
                            buf.pop_front().unwrap_or(0.0)
                        } else if buf_len < underrun_threshold {
                            if counter % 64 == 0 {
                                buf.front().copied().unwrap_or(0.0)
                            } else {
                                buf.pop_front().unwrap_or(0.0)
                            }
                        } else {
                            buf.pop_front().unwrap_or(0.0)
                        };
                        let mut acc = 0.0f32;
                        for buf in tracks.buffers.values_mut() {
                            acc += pop_drift_compensated(
                                buf,
                                counter,
                                overrun_threshold,
                                underrun_threshold,
                            );
                        }
                        let sample = acc.tanh();
                        // TPDF dither on the f32→i16 conversion.
                        let s16 = dither_to_i16(sample, &mut dither_rng);
                        for s in frame.iter_mut() {
                            *s = s16;
                        }
                    }
                    drift_counter_cb.store(counter, std::sync::atomic::Ordering::Relaxed);
                },
                err,
                None,
            ),
            other => return Err(format!("unsupported output format: {other:?}")),
        }
        .map_err(|e| format!("build_output_stream: {e}"))?;

        // Heartbeat task — proves the audio thread is alive (or not).
        let cb_for_log = cb_counter.clone();
        let pulled_for_log = pulled_counter.clone();
        tokio::spawn(async move {
            let mut prev_cb = 0u64;
            let mut prev_pulled = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let cb = cb_for_log.load(std::sync::atomic::Ordering::Relaxed);
                let pulled = pulled_for_log.load(std::sync::atomic::Ordering::Relaxed);
                eprintln!(
                    "[voice] playback heartbeat: callbacks={} (+{}), non-silent samples written={} (+{})",
                    cb,
                    cb - prev_cb,
                    pulled,
                    pulled - prev_pulled,
                );
                prev_cb = cb;
                prev_pulled = pulled;
            }
        });

        stream.play().map_err(|e| format!("play output: {e}"))?;

        if device_rate != SAMPLE_RATE {
            eprintln!("[voice] playback: resampling {SAMPLE_RATE}Hz → {device_rate}Hz via rubato");
        }
        // The resampler is created per remote track inside consume_remote_track,
        // not here: it carries filter state that only makes sense for one
        // continuous stream.
        let handle = PlaybackHandle { tracks, device_rate };

        Ok(Self {
            _stream: stream,
            handle,
        })
    }

    fn handle(&self) -> PlaybackHandle {
        self.handle.clone()
    }
}

async fn consume_remote_track(mut stream: NativeAudioStream, handle: PlaybackHandle) {
    let mut frames = 0u64;
    let mut sample_count = 0u64;
    let mut peak_recent: i16 = 0;
    // Per-track state. The resampler keeps FFT overlap and a partial-chunk
    // accumulator across calls, so it belongs to exactly one stream — sharing
    // one between participants corrupts it for everyone.
    let mut resampler = AudioResampler::new(SAMPLE_RATE, handle.device_rate);
    let track_id = handle.add_track();
    let cap = (handle.device_rate / PLAYBACK_CAP_DIVISOR) as usize;
    while let Some(frame) = stream.next().await {
        if frames == 0 {
            eprintln!(
                "[voice] remote-track first frame: {} samples @ {} Hz, ch={}",
                frame.data.len(),
                frame.sample_rate,
                frame.num_channels,
            );
        }
        frames += 1;
        sample_count += frame.data.len() as u64;
        let frame_peak = frame
            .data
            .iter()
            .map(|s| s.saturating_abs())
            .max()
            .unwrap_or(0);
        if frame_peak > peak_recent {
            peak_recent = frame_peak;
        }
        {
            // Soft limiter — smooth-clips peaks instead of hard-clipping them.
            // tanh has unit slope at the origin, so conversational levels pass
            // through at their original volume and only the top of the range
            // gets rounded off (1.0 -> 0.76). The previous `(v * 1.5).tanh() *
            // 0.9` was really a ~1.34x booster at speech levels, not a limiter.
            let f32_samples: Vec<f32> = frame
                .data
                .iter()
                .map(|s| (*s as f32 / i16::MAX as f32).tanh())
            // No per-track limiting here — the callback limits the summed mix
            // instead, which is the only place that can see whether several
            // people talking at once are driving the output past full scale.
            let f32_samples: Vec<f32> = frame
                .data
                .iter()
                .map(|s| *s as f32 / i16::MAX as f32)
                .collect();
            // High-quality resampling via rubato (48kHz → device_rate).
            let resampled = match resampler.as_mut() {
                Some(r) => r.process(&f32_samples),
                None => f32_samples,
            };
            // Bound each track's buffer (~200 ms) to limit latency drift. Keep
            // this above the callback's overrun threshold (150ms), otherwise the
            // buffer is hard-trimmed here before drift compensation sees it.
            handle.push(track_id, &resampled, cap);
        }
        if frames % 500 == 0 {
            eprintln!(
                "[voice] remote-track: {frames} frames, {sample_count} samples, peak={peak_recent} ({})",
                if peak_recent < 100 {
                    "near-silent"
                } else if peak_recent < 1000 {
                    "very quiet"
                } else {
                    "audible"
                }
            );
            peak_recent = 0;
        }
    }
    // Drop this track's jitter buffer, otherwise a participant who left keeps
    // contributing silence to the mix (and leaks a buffer per rejoin).
    handle.remove_track(track_id);
    eprintln!("[voice] remote-track stream ended after {frames} frames");
}
