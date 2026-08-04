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
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::prelude::RtcAudioSource;
use parking_lot::Mutex;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::protocol::Id;
use crate::state::{AppState, VoicePhase};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u32 = 1;
const FRAME_MS: u32 = 10;
const FRAME_SAMPLES: usize = (SAMPLE_RATE / 1000 * FRAME_MS) as usize;

pub enum VoiceCmd {
    Connect {
        livekit_url: String,
        token: String,
        channel_id: Id,
    },
    Disconnect,
    SetMute {
        muted: bool,
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

    while let Some(cmd) = rx.recv().await {
        match cmd {
            VoiceCmd::Connect {
                livekit_url,
                token,
                channel_id,
            } => {
                eprintln!("[voice] Connect to {livekit_url} channel={channel_id}");
                if let Some(prev) = session.take() {
                    eprintln!("[voice] shutting down previous session");
                    prev.shutdown().await;
                }
                match ActiveVoice::connect(&livekit_url, &token, channel_id, state).await {
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
            }
            VoiceCmd::SetMute { muted } => {
                eprintln!("[voice] SetMute muted={muted}");
                if let Some(active) = session.as_mut() {
                    active.set_muted(muted).await;
                }
                state.write().voice.muted = muted;
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
    async fn connect(livekit_url: &str, token: &str, _channel_id: Id, state: Signal<AppState>) -> Result<Self, String> {
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
        let (frame_tx, mut frame_rx) =
            tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
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
        let mic = MicCapture::start(frame_tx, state)?;

        // Remote audio mixer.
        let playback = PlaybackMixer::start()?;
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
                        RoomEvent::TrackPublished { participant, publication } => {
                            eprintln!(
                                "[voice] track published by {}: {:?}",
                                participant.identity().0,
                                publication.kind()
                            );
                        }
                        RoomEvent::TrackSubscribed { track, participant, .. } => {
                            eprintln!(
                                "[voice] track SUBSCRIBED from {}: kind={:?}",
                                participant.identity().0,
                                track.kind()
                            );
                        }
                        RoomEvent::TrackUnsubscribed { participant, .. } => {
                            eprintln!("[voice] track unsubscribed from {}", participant.identity().0);
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
    fn start(frame_tx: tokio::sync::mpsc::UnboundedSender<Vec<i16>>, mut state: Signal<AppState>) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no default input device".to_string())?;
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
        let accum: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(FRAME_SAMPLES * 4)));

        let err = |e| eprintln!("mic stream error: {e}");
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let accum = accum.clone();
                let frame_tx = frame_tx.clone();
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
                let level = if p < 0.001 { "silent" } else if p < 0.01 { "very quiet" } else { "speaking" };
                eprintln!(
                    "[voice] mic heartbeat: raw peak={p:.4} ({level}), frames pushed to webrtc={f} (+{})",
                    f - prev_frames
                );
                prev_frames = f;
            }
        });
        // Speaking indicator: sample every 150ms with a short hangover so the dot
        // doesn't flicker between words/breaths.
        dioxus::prelude::spawn(async move {
        const THRESHOLD: i32 = 25; // ajustar sensibilidad acá
        const HANGOVER_TICKS: u32 = 4; // ~600ms tras el último sonido fuerte
        let mut hangover = 0u32;
        let mut currently_speaking = false;
        loop {
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let p = speak_peak.swap(0, std::sync::atomic::Ordering::Relaxed);
        if p > THRESHOLD {
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
    device_rate: u32,
    device_channels: u32,
    muted: &Arc<Mutex<bool>>,
    accum: &Arc<Mutex<Vec<f32>>>,
) -> usize {
    if *muted.lock() {
        return 0;
    }
    let mono: Vec<f32> = samples
        .chunks(device_channels as usize)
        .map(|c| c.iter().copied().sum::<f32>() / c.len() as f32)
        .collect();
    let resampled = naive_resample(&mono, device_rate, SAMPLE_RATE);

    let mut buf = accum.lock();
    buf.extend(resampled);

    let mut pushed = 0usize;
    while buf.len() >= FRAME_SAMPLES {
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

fn naive_resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return input.to_vec();
    }
    let ratio = to as f32 / from as f32;
    let out_len = (input.len() as f32 * ratio) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f32 / ratio;
        let src_idx = src_pos as usize;
        let frac = src_pos - src_idx as f32;
        let a = input.get(src_idx).copied().unwrap_or(0.0);
        let b = input.get(src_idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

// ---------------------------------------------------------------------------
// Playback mixer: NativeAudioStream -> ring buffer -> cpal output
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct PlaybackHandle {
    buffer: Arc<Mutex<std::collections::VecDeque<f32>>>,
}

struct PlaybackMixer {
    _stream: cpal::Stream,
    handle: PlaybackHandle,
}

impl PlaybackMixer {
    fn start() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default output device".to_string())?;
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
        let buffer = Arc::new(Mutex::new(std::collections::VecDeque::<f32>::with_capacity(
            SAMPLE_RATE as usize,
        )));
        let buffer_cb = buffer.clone();
        let cb_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cb_counter_cb = cb_counter.clone();
        let pulled_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let pulled_cb = pulled_counter.clone();

        let err = |e| eprintln!("output stream error: {e}");
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config.into(),
                move |data: &mut [f32], _| {
                    cb_counter_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let mut buf = buffer_cb.lock();
                    let mut pulled = 0u64;
                    for frame in data.chunks_mut(device_channels) {
                        let sample = buf.pop_front().unwrap_or(0.0);
                        if sample != 0.0 {
                            pulled += 1;
                        }
                        for s in frame.iter_mut() {
                            *s = sample;
                        }
                    }
                    pulled_cb.fetch_add(pulled, std::sync::atomic::Ordering::Relaxed);
                },
                err,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config.into(),
                move |data: &mut [i16], _| {
                    cb_counter_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let mut buf = buffer_cb.lock();
                    for frame in data.chunks_mut(device_channels) {
                        let sample = buf.pop_front().unwrap_or(0.0);
                        let s16 = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                        for s in frame.iter_mut() {
                            *s = s16;
                        }
                    }
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

        Ok(Self {
            _stream: stream,
            handle: PlaybackHandle { buffer },
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
            let mut buf = handle.buffer.lock();
            for s in frame.data.iter() {
                buf.push_back(*s as f32 / i16::MAX as f32);
            }
            // Bound the buffer (~500 ms) to limit latency drift.
            let cap = (SAMPLE_RATE / 2) as usize;
            while buf.len() > cap {
                buf.pop_front();
            }
        }
        if frames % 500 == 0 {
            eprintln!(
                "[voice] remote-track: {frames} frames, {sample_count} samples, peak={peak_recent} ({})",
                if peak_recent < 100 { "near-silent" } else if peak_recent < 1000 { "very quiet" } else { "audible" }
            );
            peak_recent = 0;
        }
    }
    eprintln!("[voice] remote-track stream ended after {frames} frames");
}
