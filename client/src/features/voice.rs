use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use dioxus::core::Task;
use dioxus::prelude::*;
use futures_util::StreamExt;
#[cfg(target_os = "macos")]
use livekit::options::VideoEncoding;
use livekit::options::{AudioEncoding, TrackPublishOptions};
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::prelude::RtcAudioSource;
use livekit::webrtc::stats::RtcStats;
#[cfg(target_os = "macos")]
use livekit::webrtc::video_frame::native::NativeBuffer;
#[cfg(target_os = "macos")]
use livekit::webrtc::video_frame::{VideoFrame, VideoRotation};
#[cfg(target_os = "macos")]
use livekit::webrtc::video_source::native::NativeVideoSource;
#[cfg(target_os = "macos")]
use livekit::webrtc::video_source::{RtcVideoSource, VideoResolution};
use parking_lot::Mutex;
use rubato::{FftFixedIn, Resampler};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::protocol::Id;
use crate::state::{AppState, ConnectionHealth, TrackStats, VoicePhase};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u32 = 1;
const FRAME_MS: u32 = 10;
const FRAME_SAMPLES: usize = (SAMPLE_RATE / 1000 * FRAME_MS) as usize;

const RESAMPLER_CHUNK: usize = 512;

const PLAYBACK_CAP_DIVISOR: u32 = 5;

/// Must stay below the playback cap, or drift correction and the ring buffer
/// fight each other.
const DRIFT_OVERRUN_SECS: f64 = 0.12;
const DRIFT_UNDERRUN_SECS: f64 = 0.03;

const GATE_HANGOVER_FRAMES: u32 = 30;

const GATE_CLOSE_RATIO_PCT: i32 = 50;

const GATE_ENVELOPE_DECAY_PCT: i32 = 75;

const GATE_RAMP_SAMPLES: usize = 120;

const ROOM_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The constructor demands one and nothing reads it: `set_options` stores it
/// and only `options()` reads it back. Issue #170 traces that.
fn inert_apm_options() -> AudioSourceOptions {
    AudioSourceOptions::default()
}

/// Atomics rather than Dioxus signals: the cpal callback is realtime and must
/// never block on a `Signal` read.
#[derive(Clone)]
struct AudioControls {
    threshold: Arc<AtomicI32>,
    mic_gain_pct: Arc<AtomicI32>,
    agc: Arc<AtomicBool>,
    denoise: Arc<AtomicBool>,
    atten_lim_db: Arc<AtomicU32>,
    bitrate_kbps: Arc<AtomicU32>,
    stats_polling: Arc<AtomicBool>,
    deafened: Arc<AtomicBool>,
    gains: Arc<Mutex<HashMap<String, f32>>>,
    /// Absent means **silent**, not unity — a share you have not opted into
    /// should not start playing.
    stream_gains: Arc<Mutex<HashMap<String, f32>>>,
}

impl AudioControls {
    fn from_state(s: &AppState) -> Self {
        Self {
            threshold: Arc::new(AtomicI32::new(s.mic_sensitivity as i32)),
            mic_gain_pct: Arc::new(AtomicI32::new(s.mic_volume as i32)),
            agc: Arc::new(AtomicBool::new(s.auto_gain_control)),
            denoise: Arc::new(AtomicBool::new(s.noise_cancellation)),
            atten_lim_db: Arc::new(AtomicU32::new(s.denoise_atten_lim_db)),
            bitrate_kbps: Arc::new(AtomicU32::new(s.voice_bitrate_kbps)),
            stats_polling: Arc::new(AtomicBool::new(false)),
            deafened: Arc::new(AtomicBool::new(false)),
            gains: Arc::new(Mutex::new(HashMap::new())),
            stream_gains: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[derive(Default)]
struct MicMeter {
    peak: AtomicI32,
    peak_pre: AtomicI32,
    open: AtomicBool,
}

#[derive(Default)]
struct GateStats {
    dropped: AtomicU64,
    passed: AtomicU64,
    peak_after: AtomicI32,
    threshold: AtomicI32,
    atten_lim_applied: AtomicU32,
}

pub const DENOISE_ATTEN_LIM_DB_MIN: u32 = 6;
pub const DENOISE_ATTEN_LIM_DB_MAX: u32 = 60;

pub enum VoiceCmd {
    Connect {
        livekit_url: String,
        token: String,
        channel_id: Id,
    },
    /// `done` fires once the rooms are closed and the capture is stopped, so a
    /// caller that is about to drop this service can wait for it.
    Disconnect {
        done: Option<tokio::sync::oneshot::Sender<()>>,
    },
    ListDevices,
    SetDevices {
        input: Option<String>,
        output: Option<String>,
    },
    SetMute {
        muted: bool,
    },
    SetDeafen {
        deafened: bool,
    },
    SetSensitivity {
        threshold: u32,
    },
    SetDenoiseAttenLim {
        db: u32,
    },
    SetNoiseCancellation {
        enabled: bool,
    },
    SetBypassSystemProcessing {
        enabled: bool,
    },
    SetMicVolume {
        percent: u16,
    },
    SetAutoGainControl {
        enabled: bool,
    },
    SetVoiceBitrate {
        kbps: u32,
    },
    SetStatsPolling {
        enabled: bool,
    },
    SetSystemAudio {
        enabled: bool,
        target: Option<crate::sysvideo::Target>,
    },
    SetScreenAudio {
        room: Option<(String, String)>,
    },
    SetScreenVideo {
        room: Option<(String, String)>,
        target: crate::sysvideo::Target,
        settings: crate::sysvideo::Settings,
    },
    SetStreamVolume {
        pubkey: String,
        gain: f32,
    },
    SetUserVolume {
        pubkey: String,
        gain: f32,
    },
}

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
    let mut last_connect: Option<(String, String, Id)> = None;
    let controls = AudioControls::from_state(&state.read());

    while let Some(cmd) = rx.recv().await {
        match cmd {
            VoiceCmd::Connect {
                livekit_url,
                token,
                channel_id,
            } => {
                eprintln!("[voice] Connect to {livekit_url} channel={channel_id}");
                last_connect = Some((livekit_url.clone(), token.clone(), channel_id));
                if let Some(prev) = session.take() {
                    eprintln!("[voice] shutting down previous session");
                    prev.shutdown(state).await;
                }
                match ActiveVoice::connect(
                    &livekit_url,
                    &token,
                    channel_id,
                    state,
                    controls.clone(),
                )
                .await
                {
                    Ok(active) => {
                        eprintln!("[voice] connected ok");
                        {
                            let mut s = state.write();
                            s.voice.phase = VoicePhase::Connected;
                            s.voice_session_epoch += 1;
                        }
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
            VoiceCmd::Disconnect { done } => {
                eprintln!("[voice] Disconnect");
                if let Some(prev) = session.take() {
                    prev.shutdown(state).await;
                }
                {
                    let mut s = state.write();
                    s.voice.phase = VoicePhase::Idle;
                    s.voice.channel_id = None;
                    s.voice.error = None;
                }
                last_connect = None;
                if let Some(done) = done {
                    let _ = done.send(());
                }
            }
            VoiceCmd::ListDevices => {
                eprintln!("[voice] ListDevices request");
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
                {
                    let mut s = state.write();
                    if let Some(i) = input.clone() {
                        s.selected_input_device = Some(i);
                    }
                    if let Some(o) = output.clone() {
                        s.selected_output_device = Some(o);
                    }
                }
                restart_session(
                    &mut session,
                    &last_connect,
                    state,
                    &controls,
                    "device change",
                )
                .await;
            }
            VoiceCmd::SetBypassSystemProcessing { enabled } => {
                eprintln!("[voice] SetBypassSystemProcessing enabled={enabled}");
                {
                    let mut s = state.write();
                    s.bypass_system_audio_processing = enabled;
                    s.mic_bypass_error = None;
                }
                restart_session(
                    &mut session,
                    &last_connect,
                    state,
                    &controls,
                    "microphone-processing change",
                )
                .await;
            }
            VoiceCmd::SetMute { muted } => {
                eprintln!("[voice] SetMute muted={muted}");
                if let Some(active) = session.as_mut() {
                    active.set_muted(muted).await;
                }
                state.write().voice.muted = muted;
            }
            VoiceCmd::SetDeafen { deafened } => {
                eprintln!("[voice] SetDeafen deafened={deafened}");
                controls.deafened.store(deafened, Ordering::Relaxed);
                state.write().voice.deafened = deafened;
            }
            VoiceCmd::SetSensitivity { threshold } => {
                let threshold = threshold.clamp(1, 1000);
                eprintln!("[voice] SetSensitivity threshold={threshold}");
                controls
                    .threshold
                    .store(threshold as i32, Ordering::Relaxed);
                state.write().mic_sensitivity = threshold;
            }
            VoiceCmd::SetDenoiseAttenLim { db } => {
                let db = db.clamp(DENOISE_ATTEN_LIM_DB_MIN, DENOISE_ATTEN_LIM_DB_MAX);
                controls.atten_lim_db.store(db, Ordering::Relaxed);
                state.write().denoise_atten_lim_db = db;
            }
            VoiceCmd::SetNoiseCancellation { enabled } => {
                eprintln!("[voice] SetNoiseCancellation enabled={enabled}");
                controls.denoise.store(enabled, Ordering::Relaxed);
                state.write().noise_cancellation = enabled;
            }
            VoiceCmd::SetMicVolume { percent } => {
                let percent = percent.min(200);
                controls
                    .mic_gain_pct
                    .store(percent as i32, Ordering::Relaxed);
                state.write().mic_volume = percent;
            }
            VoiceCmd::SetAutoGainControl { enabled } => {
                eprintln!("[voice] SetAutoGainControl enabled={enabled}");
                controls.agc.store(enabled, Ordering::Relaxed);
                state.write().auto_gain_control = enabled;
            }
            VoiceCmd::SetVoiceBitrate { kbps } => {
                let kbps = if kbps == 24 { 24 } else { 48 };
                eprintln!("[voice] SetVoiceBitrate kbps={kbps} (applies on next connect)");
                controls.bitrate_kbps.store(kbps, Ordering::Relaxed);
                state.write().voice_bitrate_kbps = kbps;
            }
            VoiceCmd::SetStatsPolling { enabled } => {
                controls.stats_polling.store(enabled, Ordering::Relaxed);
            }
            VoiceCmd::SetSystemAudio { enabled, target } => {
                if let Some(active) = session.as_mut() {
                    if let Err(e) = active.set_system_audio(enabled, target, state).await {
                        eprintln!("[voice] system audio failed: {e}");
                        state.write().error_toast = Some(format!(
                            "Sharing video only — couldn't capture this computer's sound: {e}"
                        ));
                    }
                } else if enabled {
                    eprintln!("[voice] SetSystemAudio ignored — no voice session");
                }
            }
            VoiceCmd::SetScreenAudio { room } => {
                if let Some(active) = session.as_mut() {
                    active.set_screen_audio(room, state).await;
                } else if room.is_some() {
                    eprintln!("[voice] SetScreenAudio ignored — no voice session");
                }
            }
            VoiceCmd::SetScreenVideo {
                room,
                target,
                settings,
            } => {
                if let Some(active) = session.as_mut() {
                    if let Err(e) = active.set_screen_video(room, target, settings, state).await {
                        eprintln!("[voice] screen video failed: {e}");
                        let mut s = state.write();
                        s.screen_sharing = false;
                        s.screen_share_target = None;
                        s.error_toast = Some(format!("Couldn't share your screen: {e}"));
                    }
                } else if room.is_some() {
                    eprintln!("[voice] SetScreenVideo ignored — no voice session");
                }
            }
            VoiceCmd::SetStreamVolume { pubkey, gain } => {
                let gain = gain.clamp(0.0, 2.0);
                crate::dlog!(
                    "voice SetStreamVolume pubkey={} gain={gain:.2}",
                    &pubkey[..pubkey.len().min(8)]
                );
                controls.stream_gains.lock().insert(pubkey, gain);
            }
            VoiceCmd::SetUserVolume { pubkey, gain } => {
                let gain = gain.clamp(0.0, 2.0);
                eprintln!(
                    "[voice] SetUserVolume {} gain={gain:.2}",
                    &pubkey[..pubkey.len().min(8)]
                );
                controls.gains.lock().insert(pubkey, gain);
            }
        }
    }
    eprintln!("[voice] service loop ended (channel closed)");
    Ok(())
}

async fn restart_session(
    session: &mut Option<ActiveVoice>,
    last_connect: &Option<(String, String, Id)>,
    mut state: Signal<AppState>,
    controls: &AudioControls,
    why: &str,
) {
    if session.is_none() {
        return;
    }
    let Some((url, tok, cid)) = last_connect.clone() else {
        return;
    };
    eprintln!("[voice] Reconnecting to apply {why}");
    if let Some(prev) = session.take() {
        prev.shutdown(state).await;
    }
    match ActiveVoice::connect(&url, &tok, cid, state, controls.clone()).await {
        Ok(active) => {
            eprintln!("[voice] reconnected ok");
            {
                let mut s = state.write();
                s.voice.phase = VoicePhase::Connected;
                s.voice_session_epoch += 1;
            }
            *session = Some(active);
        }
        Err(e) => {
            eprintln!("[voice] reconnect FAILED: {e}");
            let mut s = state.write();
            s.voice.phase = VoicePhase::Error;
            s.voice.error = Some(format!("reconnect after {why}: {e}"));
            s.voice.channel_id = None;
        }
    }
}

struct ActiveVoice {
    room: Arc<Room>,
    mic: MicCapture,
    local_audio: LocalAudioTrack,
    _playback: PlaybackMixer,
    event_task: tokio::task::JoinHandle<()>,
    system_audio: Option<SystemAudioTrack>,
    screen_audio: Option<ScreenAudioRoom>,
    screen_video: Option<ScreenVideoRoom>,
    mixer: PlaybackHandle,
    self_pubkey: Option<String>,
    meter_task: Task,
    stats_task: Task,
}

impl ActiveVoice {
    async fn connect(
        livekit_url: &str,
        token: &str,
        _channel_id: Id,
        state: Signal<AppState>,
        controls: AudioControls,
    ) -> Result<Self, String> {
        let mut options = RoomOptions::default();
        options.encryption = crate::e2ee::room_options();
        let (room, mut events) = Room::connect(livekit_url, token, options)
            .await
            .map_err(|e| format!("livekit connect: {e}"))?;
        let room = Arc::new(room);
        crate::e2ee::register_room(&room, crate::e2ee::RoomKind::Voice);

        let source = NativeAudioSource::new(inert_apm_options(), SAMPLE_RATE, CHANNELS, 1000);
        let local_audio =
            LocalAudioTrack::create_audio_track("mic", RtcAudioSource::Native(source.clone()));
        let local_audio_for_mute = local_audio.clone();
        let mic_publication = room
            .local_participant()
            .publish_track(
                LocalTrack::Audio(local_audio),
                TrackPublishOptions {
                    source: TrackSource::Microphone,
                    audio_encoding: Some(AudioEncoding {
                        max_bitrate: controls.bitrate_kbps.load(Ordering::Relaxed) as u64 * 1000,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("publish mic: {e}"))?;
        let encrypted = mic_publication.encryption_type() != livekit::e2ee::EncryptionType::None;
        eprintln!(
            "[voice] mic published: encrypted={encrypted}, red={} (opus in-band FEC unaffected)",
            !encrypted
        );
        crate::e2ee::place_new_voice_publication(&room);

        let (frame_tx, frame_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        let (gated_tx, gated_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let meter = Arc::new(MicMeter::default());
        let start_muted = state.peek().voice.muted;
        let muted = Arc::new(AtomicBool::new(start_muted));
        local_audio_for_mute.rtc_track().set_enabled(!start_muted);
        let gate_stats = Arc::new(GateStats::default());
        {
            let controls = controls.clone();
            let meter = meter.clone();
            let muted = muted.clone();
            let gate_stats = gate_stats.clone();
            std::thread::Builder::new()
                .name("dxf-mic-dsp".into())
                .spawn(move || {
                    denoise_gate_loop(frame_rx, gated_tx, controls, meter, muted, gate_stats)
                })
                .map_err(|e| format!("spawn mic dsp thread: {e}"))?;
        }
        tokio::spawn(publish_loop(gated_rx, source.clone()));
        let mic = MicCapture::start(frame_tx, state, muted, gate_stats)?;
        let meter_task = spawn_meter_task(state, meter);

        let playback = PlaybackMixer::start(state, controls.clone())?;
        let mixer_handle = playback.handle();

        let (native_audio_tx, mut native_audio_rx) =
            tokio::sync::mpsc::unbounded_channel::<StreamAudio>();
        {
            let mut state = state;
            dioxus::prelude::spawn(async move {
                while let Some(ev) = native_audio_rx.recv().await {
                    let mut s = state.write();
                    match ev {
                        StreamAudio::Present(id) => {
                            s.stream_has_audio.insert(id);
                        }
                        StreamAudio::Gone(id) => {
                            s.stream_has_audio.remove(&id);
                        }
                        StreamAudio::RoomGone => {
                            s.stream_has_audio.clear();
                            crate::dlog!(
                                "voice screen_audio RoomGone -> joined=false (playback back to webview)"
                            );
                            s.screen_audio_joined = false;
                        }
                    }
                }
            });
        }

        let (quality_tx, mut quality_rx) = tokio::sync::mpsc::unbounded_channel::<QualityMsg>();
        {
            let mut state = state;
            dioxus::prelude::spawn(async move {
                while let Some(msg) = quality_rx.recv().await {
                    let changed = {
                        let s = state.peek();
                        match &msg {
                            QualityMsg::Set(id, health) => s.voice_quality.get(id) != Some(health),
                            QualityMsg::Drop(id) => s.voice_quality.contains_key(id),
                            QualityMsg::Clear => !s.voice_quality.is_empty(),
                            QualityMsg::Undecryptable => !s.media_undecryptable,
                        }
                    };
                    if !changed {
                        continue;
                    }
                    let mut s = state.write();
                    match msg {
                        QualityMsg::Set(id, health) => {
                            s.voice_quality.insert(id, health);
                        }
                        QualityMsg::Drop(id) => {
                            s.voice_quality.remove(&id);
                        }
                        QualityMsg::Clear => s.voice_quality.clear(),
                        QualityMsg::Undecryptable => s.media_undecryptable = true,
                    }
                }
            });
        }

        let event_task = tokio::spawn({
            let mixer_handle = mixer_handle.clone();
            let stream_gains = controls.stream_gains.clone();
            async move {
                while let Some(ev) = events.recv().await {
                    match &ev {
                        RoomEvent::ParticipantConnected(p) => {
                            crate::dlog!("[voice] participant connected: {}", p.identity().0);
                        }
                        RoomEvent::ParticipantDisconnected(p) => {
                            crate::dlog!("[voice] participant left: {}", p.identity().0);
                            let _ = quality_tx.send(QualityMsg::Drop(p.identity().0.clone()));
                            stream_gains.lock().remove(&p.identity().0);
                        }
                        RoomEvent::ConnectionQualityChanged {
                            quality,
                            participant,
                        } => {
                            let health = match quality {
                                ConnectionQuality::Excellent => ConnectionHealth::Excellent,
                                ConnectionQuality::Good => ConnectionHealth::Good,
                                ConnectionQuality::Poor => ConnectionHealth::Poor,
                                ConnectionQuality::Lost => ConnectionHealth::Lost,
                            };
                            if matches!(health, ConnectionHealth::Poor | ConnectionHealth::Lost) {
                                crate::dlog!(
                                    "[voice] connection {:?} for {}",
                                    health,
                                    participant.identity().0
                                );
                            }
                            let _ = quality_tx
                                .send(QualityMsg::Set(participant.identity().0.clone(), health));
                        }
                        RoomEvent::TrackPublished {
                            participant,
                            publication,
                        } => {
                            crate::dlog!(
                                "[voice] track published by {}: {:?}",
                                participant.identity().0,
                                publication.kind()
                            );
                        }
                        RoomEvent::TrackSubscribed {
                            track, participant, ..
                        } => {
                            crate::dlog!(
                                "[voice] track SUBSCRIBED from {}: kind={:?}",
                                participant.identity().0,
                                track.kind()
                            );
                        }
                        RoomEvent::TrackUnsubscribed {
                            participant,
                            publication,
                            ..
                        } => {
                            crate::dlog!(
                                "[voice] track unsubscribed from {}",
                                participant.identity().0
                            );
                            if publication.source() == TrackSource::ScreenshareAudio {
                                let _ = native_audio_tx
                                    .send(StreamAudio::Gone(participant.identity().0.clone()));
                            }
                        }
                        RoomEvent::Disconnected { reason } => {
                            eprintln!("[voice] room disconnected: {reason:?}");
                            let _ = quality_tx.send(QualityMsg::Clear);
                        }
                        RoomEvent::Reconnecting => {
                            eprintln!("[voice] reconnecting");
                        }
                        RoomEvent::Reconnected => {
                            eprintln!("[voice] reconnected");
                        }
                        RoomEvent::E2eeStateChanged { participant, state } => {
                            use livekit::webrtc::native::frame_cryptor::EncryptionState;
                            match state {
                                EncryptionState::Ok | EncryptionState::New => {}
                                bad => {
                                    let who = participant.identity().0;
                                    eprintln!("[voice] cannot decrypt media from {who}: {bad:?}");
                                    let _ = quality_tx.send(QualityMsg::Undecryptable);
                                }
                            }
                        }
                        _ => {}
                    }
                    if let RoomEvent::TrackSubscribed {
                        track,
                        publication,
                        participant,
                    } = ev
                    {
                        let is_stream = publication.source() == TrackSource::ScreenshareAudio;
                        if let RemoteTrack::Audio(audio) = track {
                            let stream = NativeAudioStream::new(
                                audio.rtc_track(),
                                SAMPLE_RATE as i32,
                                CHANNELS as i32,
                            );
                            let mixer_handle = mixer_handle.clone();
                            let identity = participant.identity().0.clone();
                            if is_stream {
                                let _ =
                                    native_audio_tx.send(StreamAudio::Present(identity.clone()));
                            }
                            tokio::spawn(consume_remote_track(
                                stream,
                                mixer_handle,
                                identity,
                                is_stream,
                            ));
                        }
                    }
                }
                eprintln!("[voice] event stream ended");
                let _ = quality_tx.send(QualityMsg::Clear);
            }
        });

        let self_pubkey = state.peek().self_user.as_ref().map(|u| u.pubkey.clone());
        let stats_task = spawn_stats_task(
            state,
            room.clone(),
            local_audio_for_mute.clone(),
            self_pubkey.clone(),
            controls.stats_polling.clone(),
        );

        Ok(Self {
            room,
            mic,
            local_audio: local_audio_for_mute,
            _playback: playback,
            event_task,
            meter_task,
            stats_task,
            system_audio: None,
            screen_audio: None,
            screen_video: None,
            mixer: mixer_handle,
            self_pubkey,
        })
    }

    async fn set_system_audio(
        &mut self,
        enabled: bool,
        target: Option<crate::sysvideo::Target>,
        state: Signal<AppState>,
    ) -> Result<(), String> {
        if enabled
            && self
                .system_audio
                .as_ref()
                .is_some_and(|existing| existing.target == target)
        {
            return Ok(());
        }
        let had_capture = self.system_audio.is_some();
        if let Some(sa) = self.system_audio.take() {
            let _ = self.room.local_participant().unpublish_track(&sa.sid).await;
        }
        if !enabled {
            if had_capture {
                eprintln!("[voice] system audio stopped");
                crate::dlog!("voice system audio stopped (capture dropped)");
            }
            return Ok(());
        }
        if !crate::sysaudio::supported() {
            return Ok(());
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        let (fatal_tx, mut fatal_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let capture = crate::sysaudio::start(tx, fatal_tx, target)?;
        {
            let mut state = state;
            dioxus::prelude::spawn(async move {
                if let Some(e) = fatal_rx.recv().await {
                    eprintln!("[voice] system audio died mid-share: {e}");
                    state.write().error_toast = Some(format!(
                        "This computer's sound stopped being shared: {e}. The screen is still \
                         being shared — restart the share to send sound again."
                    ));
                }
            });
        }
        let source = NativeAudioSource::new(
            AudioSourceOptions {
                echo_cancellation: false,
                noise_suppression: false,
                auto_gain_control: false,
            },
            SAMPLE_RATE,
            CHANNELS,
            1000,
        );
        let track = LocalAudioTrack::create_audio_track(
            "screen-audio",
            RtcAudioSource::Native(source.clone()),
        );
        let publication = match self
            .room
            .local_participant()
            .publish_track(
                LocalTrack::Audio(track),
                TrackPublishOptions {
                    source: TrackSource::ScreenshareAudio,
                    audio_encoding: Some(
                        livekit::options::audio::MUSIC_HIGH_QUALITY.encoding.clone(),
                    ),
                    dtx: false,
                    ..Default::default()
                },
            )
            .await
        {
            Ok(p) => p,
            Err(e) => return Err(format!("publishing it failed ({e})")),
        };
        let task = tokio::spawn(publish_pcm(rx, source));
        self.system_audio = Some(SystemAudioTrack {
            sid: publication.sid(),
            _capture: capture,
            task,
            target,
        });
        eprintln!("[voice] system audio started");
        crate::dlog!("voice system audio started (target={target:?})");
        Ok(())
    }

    async fn set_screen_video(
        &mut self,
        room: Option<(String, String)>,
        target: crate::sysvideo::Target,
        settings: crate::sysvideo::Settings,
        state: Signal<AppState>,
    ) -> Result<(), String> {
        let Some((url, token)) = room else {
            if let Some(prev) = self.screen_video.take() {
                prev.shutdown().await;
                crate::dlog!("voice screen video stopped (capture dropped)");
            }
            return Ok(());
        };
        if let Some(existing) = &self.screen_video
            && existing.key == (url.clone(), token.clone(), target)
        {
            return Ok(());
        }
        if let Some(prev) = self.screen_video.take() {
            prev.shutdown().await;
        }
        if !crate::sysvideo::supported() {
            return Err("screen capture isn't implemented on this platform".into());
        }
        #[cfg(target_os = "macos")]
        {
            let r = ScreenVideoRoom::connect(&url, &token, target, settings, state).await?;
            self.screen_video = Some(r);
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (url, token, target, settings, state);
        }
        Ok(())
    }

    async fn set_screen_audio(
        &mut self,
        room: Option<(String, String)>,
        mut state: Signal<AppState>,
    ) {
        let Some(key) = room else {
            if let Some(prev) = self.screen_audio.take() {
                prev.shutdown().await;
                let mut s = state.write();
                s.stream_has_audio.clear();
                s.screen_audio_joined = false;
                eprintln!("[voice] screen audio room left");
            }
            return;
        };
        match &self.screen_audio {
            Some(existing) if existing.key == key && existing.alive.load(Ordering::Relaxed) => {
                return;
            }
            Some(_) => {
                if let Some(prev) = self.screen_audio.take() {
                    prev.shutdown().await;
                }
                let mut s = state.write();
                s.stream_has_audio.clear();
                s.screen_audio_joined = false;
                drop(s);
                eprintln!("[voice] screen audio room stale, rejoining");
            }
            None => {}
        }
        let Some(self_pubkey) = self.self_pubkey.clone() else {
            eprintln!("[voice] screen audio skipped — no local identity yet");
            state.write().error_toast =
                Some("Couldn't join the stream's audio: this session has no identity yet.".into());
            return;
        };
        let (url, token) = key.clone();
        match ScreenAudioRoom::connect(&url, &token, self.mixer.clone(), self_pubkey, state, key)
            .await
        {
            Ok(r) => {
                self.screen_audio = Some(r);
                state.write().screen_audio_joined = true;
                eprintln!("[voice] screen audio room joined");
                crate::dlog!("voice screen_audio_joined=true (native owns stream playback)");
            }
            Err(e) => {
                eprintln!("[voice] screen audio room failed: {e}");
                let mut s = state.write();
                s.screen_audio_joined = false;
                s.error_toast = Some(format!(
                    "Stream sound is playing through the app window instead of your chosen \
                     output device: {e}"
                ));
            }
        }
    }

    async fn set_muted(&mut self, muted: bool) {
        self.mic.muted.store(muted, Ordering::Relaxed);
        self.local_audio.rtc_track().set_enabled(!muted);
    }

    async fn shutdown(self, mut state: Signal<AppState>) {
        self.event_task.abort();
        self.meter_task.cancel();
        self.stats_task.cancel();
        self.mic.stop();
        if let Some(sa) = self.screen_audio {
            sa.shutdown().await;
        }
        if let Some(sv) = self.screen_video {
            sv.shutdown().await;
        }
        {
            let mut s = state.write();
            s.stream_has_audio.clear();
            s.screen_audio_joined = false;
            s.voice_quality.clear();
            s.voice_stats.clear();
        }
        match tokio::time::timeout(ROOM_CLOSE_TIMEOUT, self.room.close()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("[voice] room close failed: {e}"),
            Err(_) => eprintln!("[voice] room close timed out, dropping anyway"),
        }
    }
}

struct ScreenVideoRoom {
    room: Arc<Room>,
    #[cfg(target_os = "macos")]
    _capture: crate::sysvideo::Capture,
    key: (String, String, crate::sysvideo::Target),
}

impl ScreenVideoRoom {
    #[cfg(target_os = "macos")]
    async fn connect(
        url: &str,
        token: &str,
        target: crate::sysvideo::Target,
        settings: crate::sysvideo::Settings,
        state: Signal<AppState>,
    ) -> Result<Self, String> {
        let mut options = RoomOptions::default();
        options.auto_subscribe = false;
        options.encryption = crate::e2ee::room_options();
        let (room, mut events) = Room::connect(url, token, options)
            .await
            .map_err(|e| format!("livekit connect: {e}"))?;
        let room = Arc::new(room);
        crate::e2ee::register_room(&room, crate::e2ee::RoomKind::Screen);

        let source = NativeVideoSource::new(
            VideoResolution {
                width: settings.width,
                height: settings.height,
            },
            true,
        );
        let track =
            LocalVideoTrack::create_video_track("screen", RtcVideoSource::Native(source.clone()));

        let publication = room
            .local_participant()
            .publish_track(
                LocalTrack::Video(track),
                TrackPublishOptions {
                    source: TrackSource::Screenshare,
                    video_encoding: Some(VideoEncoding {
                        max_framerate: settings.fps as f64,
                        max_bitrate: settings.max_bitrate,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("publishing the video track failed ({e})"))?;
        eprintln!(
            "[voice] screen video published sid={} identity={} target={:?} {}x{}@{}",
            publication.sid(),
            room.local_participant().identity().0,
            target,
            settings.width,
            settings.height,
            settings.fps,
        );

        let (fatal_tx, mut fatal_rx) = unbounded_channel::<String>();
        {
            let mut state = state;
            dioxus::prelude::spawn(async move {
                if let Some(e) = fatal_rx.recv().await {
                    eprintln!("[voice] screen capture died mid-share: {e}");
                    let mut s = state.write();
                    s.screen_sharing = false;
                    s.screen_share_target = None;
                    s.error_toast =
                        Some(format!("Your screen stopped being shared: {e}. Try again."));
                }
            });
        }

        let capture = crate::sysvideo::start(
            target,
            settings,
            Box::new(move |frame: crate::sysvideo::Frame| {
                let buffer = unsafe {
                    NativeBuffer::from_cv_pixel_buffer(frame.into_consumable_pixel_buffer())
                };
                source.capture_frame(&VideoFrame {
                    rotation: VideoRotation::VideoRotation0,
                    timestamp_us: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_micros() as i64)
                        .unwrap_or(0),
                    frame_metadata: None,
                    buffer,
                });
            }),
            fatal_tx,
        )?;

        tokio::spawn(async move { while events.recv().await.is_some() {} });

        Ok(Self {
            room,
            _capture: capture,
            key: (url.to_string(), token.to_string(), target),
        })
    }

    async fn shutdown(self) {
        #[cfg(target_os = "macos")]
        drop(self._capture);
        match tokio::time::timeout(ROOM_CLOSE_TIMEOUT, self.room.close()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("[voice] screen video room close failed: {e}"),
            Err(_) => eprintln!("[voice] screen video room close timed out, dropping anyway"),
        }
    }
}

struct ScreenAudioRoom {
    room: Arc<Room>,
    event_task: tokio::task::JoinHandle<()>,
    key: (String, String),
    alive: Arc<AtomicBool>,
}

enum QualityMsg {
    Set(String, ConnectionHealth),
    Drop(String),
    Clear,
    Undecryptable,
}

enum StreamAudio {
    Present(String),
    Gone(String),
    RoomGone,
}

impl ScreenAudioRoom {
    async fn connect(
        url: &str,
        token: &str,
        mixer: PlaybackHandle,
        self_pubkey: String,
        state: Signal<AppState>,
        key: (String, String),
    ) -> Result<Self, String> {
        let mut options = RoomOptions::default();
        options.auto_subscribe = false;
        options.encryption = crate::e2ee::room_options();
        let (room, mut events) = Room::connect(url, token, options)
            .await
            .map_err(|e| format!("livekit connect: {e}"))?;
        let room = Arc::new(room);
        crate::e2ee::register_room(&room, crate::e2ee::RoomKind::Screen);

        let (has_tx, mut has_rx) = unbounded_channel::<StreamAudio>();
        {
            let mut state = state;
            dioxus::prelude::spawn(async move {
                while let Some(ev) = has_rx.recv().await {
                    let mut s = state.write();
                    match ev {
                        StreamAudio::Present(id) => {
                            s.stream_has_audio.insert(id);
                        }
                        StreamAudio::Gone(id) => {
                            s.stream_has_audio.remove(&id);
                        }
                        StreamAudio::RoomGone => {
                            s.stream_has_audio.clear();
                            crate::dlog!(
                                "voice screen_audio RoomGone -> joined=false (playback back to webview)"
                            );
                            s.screen_audio_joined = false;
                        }
                    }
                }
            });
        }

        for (_, participant) in room.remote_participants() {
            for (_, publication) in participant.track_publications() {
                if wanted(
                    &publication.source(),
                    &participant.identity().0,
                    &self_pubkey,
                ) {
                    publication.set_subscribed(true);
                }
            }
        }

        let alive = Arc::new(AtomicBool::new(true));
        let event_task = tokio::spawn({
            let self_pubkey = self_pubkey.clone();
            let alive = alive.clone();
            async move {
                while let Some(ev) = events.recv().await {
                    match ev {
                        RoomEvent::TrackPublished {
                            publication,
                            participant,
                        } => {
                            if wanted(
                                &publication.source(),
                                &participant.identity().0,
                                &self_pubkey,
                            ) {
                                publication.set_subscribed(true);
                            }
                        }
                        RoomEvent::TrackSubscribed {
                            track: RemoteTrack::Audio(audio),
                            participant,
                            ..
                        } => {
                            let identity = participant.identity().0.clone();
                            let stream = NativeAudioStream::new(
                                audio.rtc_track(),
                                SAMPLE_RATE as i32,
                                CHANNELS as i32,
                            );
                            let _ = has_tx.send(StreamAudio::Present(identity.clone()));
                            tokio::spawn(consume_remote_track(
                                stream,
                                mixer.clone(),
                                identity,
                                true,
                            ));
                        }
                        RoomEvent::TrackUnsubscribed {
                            participant,
                            publication,
                            ..
                        }
                        | RoomEvent::TrackUnpublished {
                            participant,
                            publication,
                        } => {
                            if publication.source() == TrackSource::ScreenshareAudio {
                                let _ = has_tx
                                    .send(StreamAudio::Gone(participant.identity().0.clone()));
                            }
                        }
                        RoomEvent::Disconnected { reason } => {
                            eprintln!("[voice] screen audio room disconnected: {reason:?}");
                            alive.store(false, Ordering::Relaxed);
                            let _ = has_tx.send(StreamAudio::RoomGone);
                        }
                        _ => {}
                    }
                }
            }
        });

        Ok(Self {
            room,
            event_task,
            key,
            alive,
        })
    }

    async fn shutdown(self) {
        self.event_task.abort();
        match tokio::time::timeout(ROOM_CLOSE_TIMEOUT, self.room.close()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("[voice] screen audio room close failed: {e}"),
            Err(_) => eprintln!("[voice] screen audio room close timed out, dropping anyway"),
        }
    }
}

fn wanted(source: &TrackSource, publisher: &str, self_pubkey: &str) -> bool {
    *source == TrackSource::ScreenshareAudio && publisher != self_pubkey
}

fn denoise_gate_loop(
    mut frame_rx: UnboundedReceiver<Vec<f32>>,
    out_tx: UnboundedSender<Vec<i16>>,
    controls: AudioControls,
    meter: Arc<MicMeter>,
    muted: Arc<AtomicBool>,
    stats: Arc<GateStats>,
) {
    let mut denoiser: Option<crate::denoise::Denoiser> = None;
    let mut agc = crate::agc::Agc::new();
    let mut applied_atten_lim = 0u32;
    let mut gate = GateState::default();
    let mut gated = 0u64;

    while let Some(mut samples) = frame_rx.blocking_recv() {
        let gain_pct = controls.mic_gain_pct.load(Ordering::Relaxed);
        if gain_pct != 100 {
            let g = gain_pct as f32 / 100.0;
            for s in samples.iter_mut() {
                *s = (*s * g).clamp(-1.0, 1.0);
            }
        }

        if muted.load(Ordering::Relaxed) {
            let peak = peak_fixed(&samples);
            meter.bump_peak(peak);
            meter.bump_peak_pre(peak);
            meter.open.store(false, Ordering::Relaxed);
            gate.silence();
            continue;
        }

        let peak_pre = peak_fixed(&samples);
        meter.bump_peak_pre(peak_pre);
        let mut denoised = false;

        if controls.denoise.load(Ordering::Relaxed) {
            if denoiser.is_none() {
                match crate::denoise::Denoiser::new() {
                    Ok(d) => {
                        eprintln!("[voice] DeepFilterNet loaded, noise cancellation active");
                        denoiser = Some(d);
                        let mut dropped = 0;
                        while frame_rx.try_recv().is_ok() {
                            dropped += 1;
                        }
                        if dropped > 0 {
                            eprintln!("[voice] dropped {dropped} hops queued during model load");
                        }
                        gate.silence();
                    }
                    Err(e) => {
                        eprintln!("[voice] noise cancellation unavailable: {e}");
                        controls.denoise.store(false, Ordering::Relaxed);
                    }
                }
            }
            if let Some(d) = denoiser.as_mut() {
                let want = controls.atten_lim_db.load(Ordering::Relaxed);
                if want != applied_atten_lim {
                    d.set_atten_lim(want as f32);
                    applied_atten_lim = want;
                    stats.atten_lim_applied.store(want, Ordering::Relaxed);
                }
                d.process_hop(&mut samples);
                denoised = true;
            }
        } else if denoiser.is_some() {
            eprintln!("[voice] noise cancellation off, releasing model");
            denoiser = None;
            applied_atten_lim = 0;
            stats.atten_lim_applied.store(0, Ordering::Relaxed);
        }

        let boosted = if controls.agc.load(Ordering::Relaxed) {
            agc.process(&mut samples);
            true
        } else {
            agc.reset();
            false
        };

        let peak = if denoised || boosted {
            peak_fixed(&samples)
        } else {
            peak_pre
        };
        meter.bump_peak(peak);
        stats.peak_after.fetch_max(peak, Ordering::Relaxed);

        let threshold = controls.threshold.load(Ordering::Relaxed);
        stats.threshold.store(threshold, Ordering::Relaxed);
        let action = gate.step(peak, threshold);
        meter.open.store(
            matches!(action, GateAction::Pass | GateAction::RampIn),
            Ordering::Relaxed,
        );
        match action {
            GateAction::Pass => {}
            GateAction::RampIn => ramp(&mut samples, true),
            GateAction::RampOut => ramp(&mut samples, false),
            GateAction::Drop => {
                gated += 1;
                stats.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        }
        stats.passed.fetch_add(1, Ordering::Relaxed);

        let data: Vec<i16> = samples
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        if out_tx.send(data).is_err() {
            break;
        }
    }
    eprintln!("[voice] denoise/gate thread ended ({gated} frames gated)");
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GateAction {
    Pass,
    RampIn,
    RampOut,
    Drop,
}

#[derive(Default)]
struct GateState {
    hangover: u32,
    envelope: i32,
    was_open: bool,
}

impl GateState {
    fn silence(&mut self) {
        *self = Self::default();
    }

    fn step(&mut self, peak: i32, open_at: i32) -> GateAction {
        self.envelope = peak.max(self.envelope * GATE_ENVELOPE_DECAY_PCT / 100);
        let hold_at = open_at * GATE_CLOSE_RATIO_PCT / 100;
        let bar = if self.hangover > 0 { hold_at } else { open_at };
        if self.envelope > bar {
            self.hangover = GATE_HANGOVER_FRAMES;
        } else {
            self.hangover = self.hangover.saturating_sub(1);
        }
        let open = self.hangover > 0;
        let action = match (self.was_open, open) {
            (true, true) => GateAction::Pass,
            (false, true) => GateAction::RampIn,
            (true, false) => GateAction::RampOut,
            (false, false) => GateAction::Drop,
        };
        self.was_open = open;
        action
    }
}

fn ramp(samples: &mut [f32], rising: bool) {
    let n = GATE_RAMP_SAMPLES.min(samples.len());
    if n == 0 {
        return;
    }
    for (i, s) in samples.iter_mut().take(n).enumerate() {
        let g = i as f32 / n as f32;
        *s *= if rising { g } else { 1.0 - g };
    }
    if !rising {
        samples[n..].fill(0.0);
    }
}

pub fn peak_to_meter_pct(peak_fixed: u32) -> u32 {
    if peak_fixed == 0 {
        return 0;
    }
    let db = 20.0 * (peak_fixed as f64 / 1000.0).log10();
    (((db - METER_FLOOR_DB) / -METER_FLOOR_DB) * 100.0).clamp(0.0, 100.0) as u32
}

pub fn meter_pct_to_peak(pct: u32) -> u32 {
    let db = (pct.min(100) as f64 / 100.0) * -METER_FLOOR_DB + METER_FLOOR_DB;
    let amp = 10f64.powf(db / 20.0);
    ((amp * 1000.0).round() as i64).clamp(1, 1000) as u32
}

pub fn peak_to_db_label(peak_fixed: u32) -> String {
    if peak_fixed == 0 {
        return "−∞".into();
    }
    format!("{:.0} dB", 20.0 * (peak_fixed as f64 / 1000.0).log10())
}

const METER_FLOOR_DB: f64 = -60.0;

struct SystemAudioTrack {
    sid: livekit::prelude::TrackSid,
    _capture: crate::sysaudio::Capture,
    task: tokio::task::JoinHandle<()>,
    target: Option<crate::sysvideo::Target>,
}

impl Drop for SystemAudioTrack {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn publish_pcm(mut rx: UnboundedReceiver<Vec<f32>>, source: NativeAudioSource) {
    while let Some(samples) = rx.recv().await {
        let data: Vec<i16> = samples
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        let frame = AudioFrame {
            data: data.into(),
            sample_rate: SAMPLE_RATE,
            num_channels: CHANNELS,
            samples_per_channel: FRAME_SAMPLES as u32,
        };
        if let Err(e) = source.capture_frame(&frame).await {
            eprintln!("[voice] system audio capture_frame error: {e:?}");
        }
    }
}

fn peak_fixed(samples: &[f32]) -> i32 {
    (samples.iter().fold(0.0f32, |m, s| m.max(s.abs())) * 1_000.0) as i32
}

async fn publish_loop(mut rx: UnboundedReceiver<Vec<i16>>, source: NativeAudioSource) {
    let mut sent = 0u64;
    while let Some(data) = rx.recv().await {
        let frame = AudioFrame {
            data: data.into(),
            sample_rate: SAMPLE_RATE,
            num_channels: CHANNELS,
            samples_per_channel: FRAME_SAMPLES as u32,
        };
        if let Err(e) = source.capture_frame(&frame).await {
            eprintln!("[voice] capture_frame error: {e:?}");
        }
        sent += 1;
        if sent.is_multiple_of(500) {
            eprintln!("[voice] publish: {sent} frames forwarded to libwebrtc");
        }
    }
    eprintln!("[voice] publish task ended after {sent} frames");
}

impl MicMeter {
    fn bump_peak_pre(&self, value: i32) {
        self.peak_pre.fetch_max(value, Ordering::Relaxed);
    }

    fn bump_peak(&self, value: i32) {
        self.peak.fetch_max(value, Ordering::Relaxed);
    }
}

fn spawn_meter_task(mut state: Signal<AppState>, meter: Arc<MicMeter>) -> Task {
    dioxus::prelude::spawn(async move {
        let mut last_speaking = false;
        let mut last_level = u32::MAX;
        let mut last_level_pre = u32::MAX;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let level = meter.peak.swap(0, Ordering::Relaxed).clamp(0, 1000) as u32;
            let level_pre = meter.peak_pre.swap(0, Ordering::Relaxed).clamp(0, 1000) as u32;
            let speaking = meter.open.load(Ordering::Relaxed);
            if level != last_level {
                last_level = level;
                state.write().mic_level = level;
            }
            if level_pre != last_level_pre {
                last_level_pre = level_pre;
                state.write().mic_level_pre = level_pre;
            }
            if speaking != last_speaking {
                last_speaking = speaking;
                state.write().voice.speaking = speaking;
            }
        }
    })
}

fn spawn_stats_task(
    mut state: Signal<AppState>,
    room: Arc<Room>,
    local_audio: LocalAudioTrack,
    self_pubkey: Option<String>,
    enabled: Arc<AtomicBool>,
) -> Task {
    dioxus::prelude::spawn(async move {
        let mut was_enabled = false;
        let mut prev_out: Option<(u64, u64, Instant)> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;

            if !enabled.load(Ordering::Relaxed) {
                if was_enabled {
                    was_enabled = false;
                    prev_out = None;
                    if !state.peek().voice_stats.is_empty() {
                        state.write().voice_stats.clear();
                    }
                }
                continue;
            }
            was_enabled = true;

            let mut next: HashMap<String, TrackStats> = HashMap::new();
            for (identity, participant) in room.remote_participants() {
                for publication in participant.track_publications().into_values() {
                    if publication.source() != TrackSource::Microphone {
                        continue;
                    }
                    let Some(RemoteTrack::Audio(audio)) = publication.track() else {
                        continue;
                    };
                    let Ok(stats) = audio.get_stats().await else {
                        continue;
                    };
                    if let Some(s) = stats.iter().find_map(|s| match s {
                        RtcStats::InboundRtp(i) => Some(i),
                        _ => None,
                    }) {
                        let st = inbound_stats(s);
                        if let TrackStats::Inbound {
                            loss_pct,
                            jitter_ms,
                            buffer_ms,
                            concealment_events,
                        } = &st
                        {
                            crate::dlog!(
                                "stats in {} loss={:.2}% jitter={:.1}ms buf={:.1}ms conceal={} rx={} lost={}",
                                crate::identity::truncate_pubkey(&identity.0),
                                loss_pct,
                                jitter_ms,
                                buffer_ms,
                                concealment_events,
                                s.received.packets_received,
                                s.received.packets_lost.max(0),
                            );
                        }
                        next.insert(identity.0.clone(), st);
                    }
                }
            }

            if let Some(pk) = self_pubkey.clone()
                && let Ok(stats) = local_audio.get_stats().await
                && let Some(o) = stats.iter().find_map(|s| match s {
                    RtcStats::OutboundRtp(o) => Some(o),
                    _ => None,
                })
            {
                let now = Instant::now();
                let (packets, bytes) = (o.sent.packets_sent, o.sent.bytes_sent);
                let rates = outbound_rates(prev_out, packets, bytes, now);
                prev_out = Some((packets, bytes, now));
                let target_kbps = (o.outbound.target_bitrate / 1000.0).round() as u32;
                let bitrate_kbps = rates.map(|(_, kbit)| kbit);
                let packets_per_sec = rates.map(|(pkt, _)| pkt);
                crate::dlog!(
                    "stats out bitrate={} pkt/s={} target={target_kbps}kbps sent={} bytes={}",
                    bitrate_kbps.map_or("-".into(), |kbit| format!("{kbit:.0}kbps")),
                    packets_per_sec.map_or("-".into(), |pkt| format!("{pkt:.0}")),
                    packets,
                    bytes,
                );
                next.insert(
                    pk,
                    TrackStats::Outbound {
                        bitrate_kbps,
                        packets_per_sec,
                        target_kbps,
                    },
                );
            }

            if state.peek().voice_stats != next {
                state.write().voice_stats = next;
            }
        }
    })
}

fn inbound_stats(s: &livekit::webrtc::stats::InboundRtpStats) -> TrackStats {
    let received = s.received.packets_received;
    let lost = s.received.packets_lost.max(0) as u64;
    let total = received + lost;
    let emitted = s.inbound.jitter_buffer_emitted_count;
    TrackStats::Inbound {
        loss_pct: if total == 0 {
            0.0
        } else {
            lost as f32 / total as f32 * 100.0
        },
        jitter_ms: (s.received.jitter * 1000.0) as f32,
        buffer_ms: if emitted == 0 {
            0.0
        } else {
            (s.inbound.jitter_buffer_target_delay / emitted as f64 * 1000.0) as f32
        },
        concealment_events: s.inbound.concealment_events,
    }
}

fn outbound_rates(
    prev: Option<(u64, u64, Instant)>,
    packets: u64,
    bytes: u64,
    now: Instant,
) -> Option<(u32, u32)> {
    let (prev_packets, prev_bytes, prev_at) = prev?;
    let secs = now.duration_since(prev_at).as_secs_f64();
    if secs <= 0.0 {
        return None;
    }
    let d_packets = packets.checked_sub(prev_packets)?;
    let d_bytes = bytes.checked_sub(prev_bytes)?;
    Some((
        (d_packets as f64 / secs).round() as u32,
        (d_bytes as f64 * 8.0 / 1000.0 / secs).round() as u32,
    ))
}

struct MicCapture {
    _backend: MicBackend,
    muted: Arc<AtomicBool>,
    heartbeat: tokio::task::JoinHandle<()>,
}

enum MicBackend {
    Cpal {
        _stream: cpal::Stream,
    },
    #[cfg(target_os = "windows")]
    Raw {
        _capture: crate::rawmic::Capture,
    },
}

impl MicCapture {
    fn start(
        frame_tx: tokio::sync::mpsc::UnboundedSender<Vec<f32>>,
        mut state: Signal<AppState>,
        muted: Arc<AtomicBool>,
        gate_stats: Arc<GateStats>,
    ) -> Result<Self, String> {
        let raw_peak = Arc::new(AtomicI32::new(0));
        let frames_pushed = Arc::new(AtomicU64::new(0));
        let selected = state.read().selected_input_device.clone();
        state.write().mic_bypass_error = None;

        let backend = match Self::maybe_raw(&frame_tx, state, &selected, &raw_peak, &frames_pushed)
        {
            Some(raw) => raw,
            None => Self::start_cpal(&frame_tx, selected, &raw_peak, &frames_pushed)?,
        };

        let heartbeat = Self::spawn_heartbeat(raw_peak, frames_pushed, gate_stats);
        Ok(Self {
            _backend: backend,
            muted,
            heartbeat,
        })
    }

    #[cfg(target_os = "windows")]
    fn maybe_raw(
        frame_tx: &tokio::sync::mpsc::UnboundedSender<Vec<f32>>,
        mut state: Signal<AppState>,
        selected: &Option<String>,
        raw_peak: &Arc<AtomicI32>,
        frames_pushed: &Arc<AtomicU64>,
    ) -> Option<MicBackend> {
        if !state.read().bypass_system_audio_processing {
            return None;
        }

        let (fatal_tx, mut fatal_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        dioxus::prelude::spawn(async move {
            if let Some(e) = fatal_rx.recv().await {
                eprintln!("[voice] raw mic died mid-call: {e}");
                let mut s = state.write();
                s.mic_bypass_error = Some(e.clone());
                s.error_toast = Some(format!(
                    "Your microphone stopped: {e}. Leave and rejoin the voice channel to \
                     bring it back."
                ));
            }
        });

        let (frame_tx, raw_peak, frames_pushed) =
            (frame_tx.clone(), raw_peak.clone(), frames_pushed.clone());
        let sink: crate::rawmic::SinkBuilder = Box::new(move |rate: u32, channels: u32| {
            let accum: Arc<Mutex<Vec<f32>>> =
                Arc::new(Mutex::new(Vec::with_capacity(FRAME_SAMPLES * 4)));
            let resampler = Arc::new(Mutex::new(AudioResampler::new(rate, SAMPLE_RATE)));
            if resampler.lock().is_some() {
                eprintln!("[voice] mic: resampling {rate}Hz → {SAMPLE_RATE}Hz via rubato");
            }
            let mut mono_buf: Vec<f32> = Vec::with_capacity(1024);
            let mut resampled_buf: Vec<f32> = Vec::with_capacity(1024);
            let sink: crate::rawmic::Sink = Box::new(move |data: &[f32]| {
                update_peak(&raw_peak, data);
                let pushed = forward_mic(
                    &frame_tx,
                    data,
                    channels,
                    &accum,
                    &resampler,
                    &mut mono_buf,
                    &mut resampled_buf,
                );
                frames_pushed.fetch_add(pushed as u64, Ordering::Relaxed);
            });
            sink
        });

        match crate::rawmic::Capture::start(selected.clone(), fatal_tx, sink) {
            Ok(capture) => Some(MicBackend::Raw { _capture: capture }),
            Err(e) => {
                eprintln!("[voice] mic: raw capture unavailable ({e}); using the ordinary path");
                state.write().mic_bypass_error = Some(e);
                None
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn maybe_raw(
        _frame_tx: &tokio::sync::mpsc::UnboundedSender<Vec<f32>>,
        _state: Signal<AppState>,
        _selected: &Option<String>,
        _raw_peak: &Arc<AtomicI32>,
        _frames_pushed: &Arc<AtomicU64>,
    ) -> Option<MicBackend> {
        None
    }

    fn start_cpal(
        frame_tx: &tokio::sync::mpsc::UnboundedSender<Vec<f32>>,
        selected: Option<String>,
        raw_peak: &Arc<AtomicI32>,
        frames_pushed: &Arc<AtomicU64>,
    ) -> Result<MicBackend, String> {
        let frame_tx = frame_tx.clone();
        let host = cpal::default_host();
        let device = if let Some(sel_name) = selected {
            let mut found = None;
            if let Ok(devs) = host.input_devices() {
                for d in devs {
                    if let Ok(name) = d.name()
                        && name == sel_name
                    {
                        found = Some(d);
                        break;
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

        let raw_peak_cb = raw_peak.clone();
        let frames_pushed_cb = frames_pushed.clone();

        let accum: Arc<Mutex<Vec<f32>>> =
            Arc::new(Mutex::new(Vec::with_capacity(FRAME_SAMPLES * 4)));

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
                let mut mono_buf: Vec<f32> = Vec::with_capacity(1024);
                let mut resampled_buf: Vec<f32> = Vec::with_capacity(1024);
                device.build_input_stream(
                    &config.into(),
                    move |data: &[f32], _| {
                        update_peak(&raw_peak_cb, data);
                        let pushed = forward_mic(
                            &frame_tx,
                            data,
                            device_channels,
                            &accum,
                            &resampler_cb,
                            &mut mono_buf,
                            &mut resampled_buf,
                        );
                        frames_pushed_cb.fetch_add(pushed as u64, Ordering::Relaxed);
                    },
                    err,
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let accum = accum.clone();
                let frame_tx = frame_tx.clone();
                let resampler_cb = resampler.clone();
                let mut f32_buf: Vec<f32> = Vec::with_capacity(1024);
                let mut mono_buf: Vec<f32> = Vec::with_capacity(1024);
                let mut resampled_buf: Vec<f32> = Vec::with_capacity(1024);
                device.build_input_stream(
                    &config.into(),
                    move |data: &[i16], _| {
                        f32_buf.clear();
                        f32_buf.extend(data.iter().map(|s| *s as f32 / i16::MAX as f32));
                        update_peak(&raw_peak_cb, &f32_buf);
                        let pushed = forward_mic(
                            &frame_tx,
                            &f32_buf,
                            device_channels,
                            &accum,
                            &resampler_cb,
                            &mut mono_buf,
                            &mut resampled_buf,
                        );
                        frames_pushed_cb.fetch_add(pushed as u64, Ordering::Relaxed);
                    },
                    err,
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let accum = accum.clone();
                let frame_tx = frame_tx.clone();
                let resampler_cb = resampler.clone();
                let mut f32_buf: Vec<f32> = Vec::with_capacity(1024);
                let mut mono_buf: Vec<f32> = Vec::with_capacity(1024);
                let mut resampled_buf: Vec<f32> = Vec::with_capacity(1024);
                device.build_input_stream(
                    &config.into(),
                    move |data: &[u16], _| {
                        f32_buf.clear();
                        f32_buf.extend(data.iter().map(|s| (*s as f32 - 32768.0) / 32768.0));
                        update_peak(&raw_peak_cb, &f32_buf);
                        let pushed = forward_mic(
                            &frame_tx,
                            &f32_buf,
                            device_channels,
                            &accum,
                            &resampler_cb,
                            &mut mono_buf,
                            &mut resampled_buf,
                        );
                        frames_pushed_cb.fetch_add(pushed as u64, Ordering::Relaxed);
                    },
                    err,
                    None,
                )
            }
            other => return Err(format!("unsupported sample format: {other:?}")),
        }
        .map_err(|e| format!("build_input_stream: {e}"))?;

        stream.play().map_err(|e| format!("play mic: {e}"))?;
        Ok(MicBackend::Cpal { _stream: stream })
    }

    fn spawn_heartbeat(
        peak_log: Arc<AtomicI32>,
        frames_log: Arc<AtomicU64>,
        gate_stats: Arc<GateStats>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut prev_frames = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let p = peak_log.swap(0, Ordering::Relaxed) as f32 / 1_000.0;
                let f = frames_log.load(Ordering::Relaxed);
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
                let after = gate_stats.peak_after.swap(0, Ordering::Relaxed) as f32 / 1_000.0;
                let passed = gate_stats.passed.swap(0, Ordering::Relaxed);
                let dropped = gate_stats.dropped.swap(0, Ordering::Relaxed);
                crate::dlog!(
                    "mic 2s raw={p:.4} after={after:.4} thr={:.4} passed={passed} dropped={dropped}{}",
                    gate_stats.threshold.load(Ordering::Relaxed) as f32 / 1_000.0,
                    match gate_stats.atten_lim_applied.load(Ordering::Relaxed) {
                        0 => String::new(),
                        db => format!(" atten={db}dB"),
                    },
                );
                prev_frames = f;
            }
        })
    }

    fn stop(self) {}
}

impl Drop for MicCapture {
    fn drop(&mut self) {
        self.heartbeat.abort();
    }
}

fn forward_mic(
    frame_tx: &tokio::sync::mpsc::UnboundedSender<Vec<f32>>,
    samples: &[f32],
    device_channels: u32,
    accum: &Arc<Mutex<Vec<f32>>>,
    resampler: &Arc<Mutex<Option<AudioResampler>>>,
    mono_buf: &mut Vec<f32>,
    resampled_buf: &mut Vec<f32>,
) -> usize {
    mono_buf.clear();
    mono_buf.extend(
        samples
            .chunks(device_channels as usize)
            .map(|c| c.iter().copied().sum::<f32>() / c.len() as f32),
    );
    {
        let mut rs = resampler.lock();
        match rs.as_mut() {
            Some(r) => r.process_into(mono_buf, resampled_buf),
            None => {
                resampled_buf.clear();
                resampled_buf.extend_from_slice(mono_buf);
            }
        }
    }

    let mut buf = accum.lock();
    buf.extend_from_slice(resampled_buf);

    let mut pushed = 0usize;
    while buf.len() >= FRAME_SAMPLES {
        let chunk: Vec<f32> = buf.drain(..FRAME_SAMPLES).collect();
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

struct AudioResampler {
    inner: FftFixedIn<f32>,
    input_accum: Vec<f32>,
    chunk_in: Vec<f32>,
    scratch_out: Vec<f32>,
}

impl AudioResampler {
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

    fn process_into(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.clear();
        self.input_accum.extend_from_slice(input);

        let Self {
            inner,
            input_accum,
            chunk_in,
            scratch_out,
        } = self;

        while input_accum.len() >= RESAMPLER_CHUNK {
            chunk_in.clear();
            chunk_in.extend(input_accum.drain(..RESAMPLER_CHUNK));
            let need = inner.output_frames_next();
            if scratch_out.len() < need {
                scratch_out.resize(need, 0.0);
            }
            let waves_in = [&chunk_in[..]];
            let mut waves_out = [&mut scratch_out[..]];
            match inner.process_into_buffer(&waves_in, &mut waves_out, None) {
                Ok((_, produced)) => out.extend_from_slice(&scratch_out[..produced]),
                Err(e) => eprintln!("[voice] rubato process error: {e:?}"),
            }
        }
    }
}

#[inline]
fn xorshift32(state: &mut u32) -> f32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    (x >> 8) as f32 / (1u32 << 24) as f32
}

const DITHER_SEED: u32 = 0x9E37_79B9;

#[inline]
fn dither_to_i16(sample: f32, rng: &mut u32) -> i16 {
    let clamped = sample.clamp(-1.0, 1.0);
    let r1 = xorshift32(rng);
    let r2 = xorshift32(rng);
    let dither = (r1 + r2 - 1.0) * (1.0 / i16::MAX as f32);
    ((clamped + dither) * i16::MAX as f32) as i16
}

#[derive(Default)]
struct MixerTracks {
    buffers: std::collections::HashMap<u64, TrackBuf>,
    next_id: u64,
}

struct TrackBuf {
    samples: std::collections::VecDeque<f32>,
    identity: String,
    gain: f32,
    is_stream: bool,
}

#[derive(Clone)]
struct PlaybackHandle {
    tracks: Arc<Mutex<MixerTracks>>,
    device_rate: u32,
    gains: Arc<Mutex<HashMap<String, f32>>>,
    stream_gains: Arc<Mutex<HashMap<String, f32>>>,
}

impl PlaybackHandle {
    fn add_track(&self, identity: String, is_stream: bool) -> u64 {
        let gain = if is_stream {
            self.stream_gains
                .lock()
                .get(&identity)
                .copied()
                .unwrap_or(0.0)
        } else {
            self.gains.lock().get(&identity).copied().unwrap_or(1.0)
        };
        crate::dlog!(
            "mixer add_track identity={identity} is_stream={is_stream} initial_gain={gain:.2}"
        );
        let mut t = self.tracks.lock();
        let id = t.next_id;
        t.next_id = t.next_id.wrapping_add(1);
        t.buffers.insert(
            id,
            TrackBuf {
                samples: std::collections::VecDeque::with_capacity(SAMPLE_RATE as usize / 2),
                identity,
                gain,
                is_stream,
            },
        );
        id
    }

    fn push(&self, id: u64, samples: &[f32], cap: usize) {
        let mut t = self.tracks.lock();
        if let Some(track) = t.buffers.get_mut(&id) {
            track.samples.extend(samples);
            while track.samples.len() > cap {
                track.samples.pop_front();
            }
        }
    }

    fn remove_track(&self, id: u64) {
        let removed = self.tracks.lock().buffers.remove(&id);
        if let Some(t) = removed {
            crate::dlog!(
                "mixer remove_track identity={} is_stream={}",
                t.identity,
                t.is_stream
            );
        }
    }
}

fn refresh_gains(
    tracks: &mut MixerTracks,
    gains: &Arc<Mutex<HashMap<String, f32>>>,
    stream_gains: &Arc<Mutex<HashMap<String, f32>>>,
    deafened: &Arc<AtomicBool>,
) {
    if tracks.buffers.is_empty() {
        return;
    }
    if deafened.load(Ordering::Relaxed) {
        for track in tracks.buffers.values_mut() {
            track.gain = 0.0;
        }
        return;
    }
    let g = gains.lock();
    let sg = stream_gains.lock();
    for track in tracks.buffers.values_mut() {
        track.gain = if track.is_stream {
            sg.get(&track.identity).copied().unwrap_or(0.0)
        } else {
            g.get(&track.identity).copied().unwrap_or(1.0)
        };
    }
}

/// A lone speaker must come out exactly as sent: the old per-track curve cost
/// every listener up to 1.4 dB, and the f32 path could hand the device 2.0.
#[inline]
fn mix(acc: f32) -> f32 {
    acc.clamp(-1.0, 1.0)
}

#[inline]
fn pop_drift_compensated(
    buf: &mut std::collections::VecDeque<f32>,
    counter: u32,
    overrun: usize,
    underrun: usize,
) -> f32 {
    let len = buf.len();
    if len > overrun {
        if counter.is_multiple_of(32) {
            buf.pop_front();
        }
        buf.pop_front().unwrap_or(0.0)
    } else if len < underrun {
        if counter.is_multiple_of(64) {
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
    fn start(state: Signal<AppState>, controls: AudioControls) -> Result<Self, String> {
        let host = cpal::default_host();
        let selected = state.read().selected_output_device.clone();
        let device = if let Some(sel_name) = selected {
            let mut found = None;
            if let Ok(devs) = host.output_devices() {
                for d in devs {
                    if let Ok(name) = d.name()
                        && name == sel_name
                    {
                        found = Some(d);
                        break;
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

        let tracks = Arc::new(Mutex::new(MixerTracks::default()));
        let tracks_cb = tracks.clone();
        let cb_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let cb_counter_cb = cb_counter.clone();
        let pulled_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let pulled_cb = pulled_counter.clone();

        let drift_counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let drift_counter_cb = drift_counter.clone();
        let device_rate_cb = device_rate;
        let gains_f32 = controls.gains.clone();
        let gains_i16 = controls.gains.clone();
        let stream_gains_f32 = controls.stream_gains.clone();
        let stream_gains_i16 = controls.stream_gains.clone();
        let deafened_f32 = controls.deafened.clone();
        let deafened_i16 = controls.deafened.clone();
        let mut dither_rng = DITHER_SEED;

        let err = |e| eprintln!("output stream error: {e}");
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config.into(),
                move |data: &mut [f32], _| {
                    cb_counter_cb.fetch_add(1, Ordering::Relaxed);
                    let mut tracks = tracks_cb.lock();
                    refresh_gains(&mut tracks, &gains_f32, &stream_gains_f32, &deafened_f32);
                    let mut pulled = 0u64;
                    let overrun_threshold = (device_rate_cb as f64 * DRIFT_OVERRUN_SECS) as usize;
                    let underrun_threshold = (device_rate_cb as f64 * DRIFT_UNDERRUN_SECS) as usize;
                    let mut counter = drift_counter_cb.load(Ordering::Relaxed);
                    for frame in data.chunks_mut(device_channels) {
                        counter = counter.wrapping_add(1);
                        let mut acc = 0.0f32;
                        for track in tracks.buffers.values_mut() {
                            let s = track.gain
                                * pop_drift_compensated(
                                    &mut track.samples,
                                    counter,
                                    overrun_threshold,
                                    underrun_threshold,
                                );
                            acc += s;
                        }
                        let sample = mix(acc);
                        if sample != 0.0 {
                            pulled += 1;
                        }
                        for s in frame.iter_mut() {
                            *s = sample;
                        }
                    }
                    drift_counter_cb.store(counter, Ordering::Relaxed);
                    pulled_cb.fetch_add(pulled, Ordering::Relaxed);
                },
                err,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config.into(),
                move |data: &mut [i16], _| {
                    cb_counter_cb.fetch_add(1, Ordering::Relaxed);
                    let mut tracks = tracks_cb.lock();
                    refresh_gains(&mut tracks, &gains_i16, &stream_gains_i16, &deafened_i16);
                    let overrun_threshold = (device_rate_cb as f64 * DRIFT_OVERRUN_SECS) as usize;
                    let underrun_threshold = (device_rate_cb as f64 * DRIFT_UNDERRUN_SECS) as usize;
                    let mut counter = drift_counter_cb.load(Ordering::Relaxed);
                    for frame in data.chunks_mut(device_channels) {
                        counter = counter.wrapping_add(1);
                        let mut acc = 0.0f32;
                        for track in tracks.buffers.values_mut() {
                            let s = track.gain
                                * pop_drift_compensated(
                                    &mut track.samples,
                                    counter,
                                    overrun_threshold,
                                    underrun_threshold,
                                );
                            acc += s;
                        }
                        let sample = mix(acc);
                        let s16 = dither_to_i16(sample, &mut dither_rng);
                        for s in frame.iter_mut() {
                            *s = s16;
                        }
                    }
                    drift_counter_cb.store(counter, Ordering::Relaxed);
                },
                err,
                None,
            ),
            other => return Err(format!("unsupported output format: {other:?}")),
        }
        .map_err(|e| format!("build_output_stream: {e}"))?;

        let cb_for_log = Arc::downgrade(&cb_counter);
        let pulled_for_log = Arc::downgrade(&pulled_counter);
        tokio::spawn(async move {
            let mut prev_cb = 0u64;
            let mut prev_pulled = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let (Some(cb_counter), Some(pulled_counter)) =
                    (cb_for_log.upgrade(), pulled_for_log.upgrade())
                else {
                    break;
                };
                let cb = cb_counter.load(std::sync::atomic::Ordering::Relaxed);
                let pulled = pulled_counter.load(std::sync::atomic::Ordering::Relaxed);
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
        let handle = PlaybackHandle {
            tracks,
            device_rate,
            gains: controls.gains.clone(),
            stream_gains: controls.stream_gains.clone(),
        };

        Ok(Self {
            _stream: stream,
            handle,
        })
    }

    fn handle(&self) -> PlaybackHandle {
        self.handle.clone()
    }
}

async fn consume_remote_track(
    mut stream: NativeAudioStream,
    handle: PlaybackHandle,
    identity: String,
    is_stream: bool,
) {
    let mut frames = 0u64;
    let mut sample_count = 0u64;
    let mut peak_recent: i16 = 0;
    let mut resampler = AudioResampler::new(SAMPLE_RATE, handle.device_rate);
    let mut f32_buf: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
    let mut resampled_buf: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
    let who = match identity.split_once('#') {
        Some((pk, suffix)) => format!("{}#{suffix}", crate::identity::truncate_pubkey(pk)),
        None => crate::identity::truncate_pubkey(&identity),
    };
    let track_id = handle.add_track(identity, is_stream);
    let cap = (handle.device_rate / PLAYBACK_CAP_DIVISOR) as usize;
    while let Some(frame) = stream.next().await {
        if frames == 0 {
            eprintln!(
                "[voice] remote-track {who} first frame: {} samples @ {} Hz, ch={}",
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
            f32_buf.clear();
            f32_buf.extend(frame.data.iter().map(|s| *s as f32 / i16::MAX as f32));
            match resampler.as_mut() {
                Some(r) => {
                    r.process_into(&f32_buf, &mut resampled_buf);
                    handle.push(track_id, &resampled_buf, cap);
                }
                None => handle.push(track_id, &f32_buf, cap),
            }
        }
        if frames.is_multiple_of(500) {
            eprintln!(
                "[voice] remote-track {who}: {frames} frames, {sample_count} samples, peak={peak_recent} ({})",
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
    handle.remove_track(track_id);
    eprintln!("[voice] remote-track {who} stream ended after {frames} frames");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One person talking has to arrive at the level they were sent at. The
    /// per-track curve this replaced took up to 1.4 dB off that case for a sum
    /// that only ever happens when several people talk at once.
    #[test]
    fn a_lone_speaker_is_not_quietened() {
        for s in [0.0, 0.1, 0.5, 0.9, 1.0, -0.7] {
            assert_eq!(mix(s), s);
        }
    }

    /// The f32 output stream writes what it is handed, so a sum that leaves
    /// full scale reaches the device as clipping it never asked for.
    #[test]
    fn a_sum_never_leaves_full_scale() {
        assert_eq!(mix(1.8), 1.0);
        assert_eq!(mix(-2.4), -1.0);
    }

    /// The two halves have to agree: the AGC runs before the gate, so a mic too
    /// quiet to clear the default bar on its own clears it once normalised.
    /// That is the reported difference against other clients, end to end.
    #[test]
    fn a_quiet_mic_reaches_the_gate_loud_enough_to_open_it() {
        let quiet = 0.02_f32;
        let default_bar = 50; // `settings::default_mic_sensitivity`
        assert!(
            peak_fixed(&[quiet]) < default_bar,
            "precondition: raw, this mic is under the bar and gets dropped"
        );

        let mut agc = crate::agc::Agc::new();
        let mut gate = GateState::default();
        let mut action = GateAction::Drop;
        for _ in 0..600 {
            let mut hop: Vec<f32> = (0..crate::denoise::HOP)
                .map(|i| if i % 2 == 0 { quiet } else { -quiet })
                .collect();
            agc.process(&mut hop);
            action = gate.step(peak_fixed(&hop), default_bar);
        }
        assert!(
            matches!(action, GateAction::Pass),
            "still {action:?} after the AGC settled"
        );
    }

    #[test]
    fn the_gate_holds_below_the_level_it_opened_at() {
        let open_at = 21; // the reporter's own sensitivity setting
        let hold_at = open_at * GATE_CLOSE_RATIO_PCT / 100;
        assert!(
            hold_at < open_at,
            "hysteresis has to hold below the opening"
        );

        let tail = 14;
        assert!(tail < open_at, "precondition: would not open the gate");
        assert!(
            tail > hold_at,
            "a tail between the two bars is exactly what the hysteresis is for"
        );
    }

    #[test]
    fn the_gate_envelope_never_reads_below_the_hop() {
        let mut env = 0i32;
        for peak in [0, 5, 900, 3, 0, 0, 40] {
            env = peak.max(env * GATE_ENVELOPE_DECAY_PCT / 100);
            assert!(env >= peak, "envelope {env} read under its own hop {peak}");
        }
        assert!(env < 900, "envelope latched at the loudest hop it ever saw");
    }

    #[test]
    fn muting_makes_the_gate_forget_the_level_it_was_hearing() {
        let open_at = 21;
        let mut gate = GateState::default();
        for _ in 0..5 {
            gate.step(900, open_at);
        }
        assert!(gate.was_open, "precondition: loud speech opened the gate");

        gate.silence();

        assert_eq!(
            gate.step(5, open_at),
            GateAction::Drop,
            "a quiet hop after the mute was judged against the level from before it"
        );
    }

    #[test]
    fn a_gate_that_kept_its_envelope_would_hold_open_for_a_third_of_a_second() {
        let open_at = 21;
        let mut gate = GateState::default();
        for _ in 0..5 {
            gate.step(900, open_at);
        }
        gate.hangover = 0;

        let held = (0..100)
            .take_while(|_| gate.step(5, open_at) != GateAction::Drop)
            .count();
        assert!(
            held > 30,
            "the trace in the review depends on this being long, not a hop or two; got {held}"
        );
    }

    #[test]
    fn one_ramp_in_at_the_start_and_one_ramp_out_at_the_end() {
        let open_at = 21;
        let mut gate = GateState::default();
        assert_eq!(gate.step(900, open_at), GateAction::RampIn);
        assert_eq!(gate.step(900, open_at), GateAction::Pass);

        let tail: Vec<_> = (0..GATE_HANGOVER_FRAMES + 60)
            .map(|_| gate.step(0, open_at))
            .collect();
        assert_eq!(
            tail.iter().filter(|a| **a == GateAction::RampOut).count(),
            1,
            "the fade out has to happen once, not on every silent hop"
        );
        assert_eq!(*tail.last().unwrap(), GateAction::Drop);
    }

    #[test]
    fn a_closing_ramp_ends_on_silence() {
        let mut hop = vec![1.0f32; FRAME_SAMPLES];
        ramp(&mut hop, false);
        assert_eq!(hop[0], 1.0, "the fade starts at full level");
        assert!(hop.iter().all(|s| *s <= 1.0 && *s >= 0.0));
        assert!(
            hop[GATE_RAMP_SAMPLES..].iter().all(|s| *s == 0.0),
            "everything past the ramp has to be silence"
        );

        let mut hop = vec![1.0f32; FRAME_SAMPLES];
        ramp(&mut hop, true);
        assert_eq!(hop[0], 0.0, "the fade in starts at silence");
        assert!(
            hop[GATE_RAMP_SAMPLES..].iter().all(|s| *s == 1.0),
            "past the ramp the signal is untouched"
        );

        let mut short = vec![1.0f32; GATE_RAMP_SAMPLES / 2];
        ramp(&mut short, false);
        assert_eq!(short[0], 1.0);
    }

    #[test]
    fn outbound_rates_are_per_second_and_need_two_readings() {
        let t0 = Instant::now();
        assert_eq!(outbound_rates(None, 50, 6_000, t0), None);

        let t1 = t0 + std::time::Duration::from_secs(1);
        assert_eq!(
            outbound_rates(Some((0, 0, t0)), 50, 6_000, t1),
            Some((50, 48))
        );

        let t2 = t0 + std::time::Duration::from_secs(2);
        assert_eq!(
            outbound_rates(Some((0, 0, t0)), 50, 6_000, t2),
            Some((25, 24))
        );
    }

    #[test]
    fn outbound_rates_refuse_a_counter_that_went_backwards() {
        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_secs(1);
        assert_eq!(outbound_rates(Some((900, 0, t0)), 10, 6_000, t1), None);
        assert_eq!(outbound_rates(Some((0, 90_000, t0)), 50, 600, t1), None);
        assert_eq!(outbound_rates(Some((0, 0, t0)), 50, 6_000, t0), None);
    }

    #[test]
    fn speech_sits_in_the_middle_of_the_meter() {
        let pct = peak_to_meter_pct(32);
        assert!((45..=55).contains(&pct), "quiet speech read as {pct}%");
        assert!(peak_to_meter_pct(700) > 90);
    }

    #[test]
    fn meter_endpoints_behave() {
        assert_eq!(peak_to_meter_pct(0), 0);
        assert_eq!(peak_to_meter_pct(1000), 100);
        assert_eq!(peak_to_meter_pct(1), 0);
    }

    #[test]
    fn meter_pct_round_trips_through_the_peak_scale() {
        for pct in [10, 25, 50, 75, 100] {
            let peak = meter_pct_to_peak(pct);
            let back = peak_to_meter_pct(peak);
            assert!(back.abs_diff(pct) <= 1, "{pct}% -> peak {peak} -> {back}%");
        }
    }

    #[test]
    fn threshold_never_collapses_to_zero() {
        assert!(meter_pct_to_peak(0) >= 1);
        assert!(meter_pct_to_peak(1000) <= 1000);
    }

    /// Not a guard — a measurement, like the raw-mode one in `rawmic`. The AGC
    /// runs before the gate, so the bar judges audio whose level has already
    /// been normalised. Re-run it with a real recording to settle 63/98:
    /// synthetic speech has a crest factor near 2 and real speech 3 to 5, so
    /// the numbers below understate what a microphone actually delivers.
    #[test]
    #[ignore = "a measurement to re-run, not a guard; see issue #171"]
    fn where_the_gate_opens_once_the_agc_has_normalised_the_level() {
        fn noise(n: usize, rms_want: f32, seed: &mut u32) -> Vec<f32> {
            let mut v: Vec<f32> = (0..n)
                .map(|_| {
                    *seed ^= *seed << 13;
                    *seed ^= *seed >> 17;
                    *seed ^= *seed << 5;
                    (*seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
                })
                .collect();
            let r = (v.iter().map(|s| s * s).sum::<f32>() / n as f32).sqrt();
            let k = rms_want / r.max(1e-9);
            for s in v.iter_mut() {
                *s *= k;
            }
            v
        }
        fn voiced(n: usize, rms_want: f32, seed: &mut u32, ph: &mut f32) -> Vec<f32> {
            let mut v = noise(n, rms_want * 0.4, seed);
            for s in v.iter_mut() {
                *ph += 2.0 * std::f32::consts::PI * 140.0 / 48_000.0;
                *s += rms_want * 1.2 * ph.sin();
            }
            v
        }

        let threshold = crate::settings::ClientSettings::default().mic_sensitivity as i32;
        println!("bar {threshold} ({})", peak_to_db_label(threshold as u32));

        println!(
            "
what the bar demands of the microphone, before the AGC:"
        );
        let (mut seed, mut ph) = (0x9E37_79B9u32, 0.0f32);
        for rms in [0.01f32, 0.02, 0.03, 0.05, 0.13] {
            let peak = peak_fixed(&voiced(FRAME_SAMPLES, rms, &mut seed, &mut ph));
            println!(
                "  speech {:+.0} dBFS -> peak {peak:>4}  {}",
                20.0 * rms.log10(),
                if peak > threshold { "passes" } else { "CUT" }
            );
        }

        println!(
            "
two seconds of speech, then only room tone:"
        );
        for room in [0.002f32, 0.003, 0.005, 0.01] {
            let (mut seed, mut ph) = (0x9E37_79B9u32, 0.0f32);
            let mut agc = crate::agc::Agc::new();
            let mut gate = GateState::default();
            for _ in 0..200 {
                let mut h = voiced(FRAME_SAMPLES, 0.02, &mut seed, &mut ph);
                agc.process(&mut h);
                gate.step(peak_fixed(&h), threshold);
            }
            let mut open = 0;
            for _ in 0..500 {
                let mut h = noise(FRAME_SAMPLES, room, &mut seed);
                agc.process(&mut h);
                if !matches!(gate.step(peak_fixed(&h), threshold), GateAction::Drop) {
                    open += 1;
                }
            }
            println!(
                "  room {:+.0} dBFS -> {open}/500 hops of the pause transmitted",
                20.0 * room.log10()
            );
        }
    }
}
