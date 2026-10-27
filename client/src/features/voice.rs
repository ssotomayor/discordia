use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use dioxus::core::Task;
use dioxus::prelude::*;
use futures_util::StreamExt;
use livekit::options::{AudioEncoding, TrackPublishOptions};
#[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
use livekit::options::{DegradationPreference, VideoCodec, VideoEncoderBackend, VideoEncoding};
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::peer_connection_factory::{
    ContinualGatheringPolicy, IceServer as RtcIceServer, IceTransportsType, RtcConfiguration,
};
use livekit::webrtc::prelude::RtcAudioSource;
use livekit::webrtc::stats::RtcStats;
#[cfg(target_os = "macos")]
use livekit::webrtc::video_frame::native::NativeBuffer;
#[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
use livekit::webrtc::video_frame::{VideoFrame, VideoRotation};
#[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
use livekit::webrtc::video_source::native::NativeVideoSource;
#[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
use livekit::webrtc::video_source::{RtcVideoSource, VideoResolution};
use parking_lot::Mutex;
use rubato::audioadapter_buffers::direct::SequentialSlice;
use rubato::{
    Async, Fft, FixedAsync, FixedSync, Resampler, SincInterpolationParameters,
    SincInterpolationType, WindowFunction,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::protocol::{ClientMessage, IceServer, Id};
use crate::state::{AppState, ConnectionHealth, TrackStats, VoicePhase};

#[cfg(any(target_os = "android", target_os = "windows"))]
#[path = "voice_camera.rs"]
mod camera;
#[cfg(any(target_os = "android", target_os = "windows"))]
use camera::CameraPublisher;

pub(crate) const SAMPLE_RATE: u32 = 48_000;
#[path = "voice_activity.rs"]
mod activity;
#[path = "voice_calibration.rs"]
mod calibration;
#[path = "voice_direct.rs"]
pub mod direct;
#[path = "voice_drift.rs"]
mod drift;
#[path = "voice_dsp.rs"]
mod dsp;
#[path = "voice_mic_test.rs"]
pub mod mic_test;
#[path = "voice_playback_recovery.rs"]
mod playback_recovery;
pub(crate) fn microphone_context(state: &AppState) -> Result<crate::mic_profile::Context, String> {
    calibration::context(state)
}
#[path = "voice_network.rs"]
mod network;
#[path = "voice_subscriptions.rs"]
mod subscriptions;
const CHANNELS: u32 = 1;
const FRAME_MS: u32 = 10;
const FRAME_SAMPLES: usize = (SAMPLE_RATE / 1000 * FRAME_MS) as usize;
const _: () = assert!(FRAME_SAMPLES == crate::audio_queue::SAMPLES);

/// Buffered (non-zero) means a 10 ms timer in webrtc-sys that sends silence
/// whenever the queue runs under 10 ms, so a drifting capture clock cuts holes.
const SOURCE_QUEUE_MS: u32 = 0;

pub(crate) const RESAMPLER_CHUNK: usize = 512;

const PLAYBACK_CAP_DIVISOR: u32 = 5;

const GATE_HANGOVER_FRAMES: u32 = 30;

const GATE_CLOSE_RATIO_PCT: i32 = 50;

const GATE_ENVELOPE_DECAY_PCT: i32 = 75;

const GATE_RAMP_SAMPLES: usize = 120;

/// Krisp VAD's activation threshold in Discord (`vadKrispActivationThreshold`).
const SPEECH_PROBABILITY: f32 = 0.5;

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
    processing: Arc<Mutex<crate::mic_profile::Processing>>,
    threshold: Arc<AtomicI32>,
    mic_gain_pct: Arc<AtomicI32>,
    agc: Arc<AtomicBool>,
    denoise: Arc<AtomicBool>,
    atten_lim_db: Arc<AtomicU32>,
    denoise_retry: Arc<AtomicU32>,
    bitrate_kbps: Arc<AtomicU32>,
    stats_polling: Arc<AtomicBool>,
    deafened: Arc<AtomicBool>,
    gains: Arc<Mutex<HashMap<String, f32>>>,
    /// Absent means **silent**, not unity — a share you have not opted into
    /// should not start playing.
    stream_gains: Arc<Mutex<HashMap<String, f32>>>,
    watched_streams: tokio::sync::watch::Sender<HashSet<String>>,
    soundboard_pct: Arc<AtomicU32>,
}

#[derive(Clone)]
pub(super) struct StreamPlaybackControls(Arc<Mutex<HashMap<String, f32>>>);

impl StreamPlaybackControls {
    pub(super) fn apply(&self, gains: &[(String, f32)]) {
        *self.0.lock() = gains.iter().cloned().collect();
    }
}

impl AudioControls {
    fn mic_config(&self) -> crate::mic_profile::Config {
        let processing = self.processing.lock();
        crate::mic_profile::Config {
            processing: *processing,
            threshold: self.threshold.load(Ordering::Relaxed).clamp(1, 1000) as u32,
            volume: self.mic_gain_pct.load(Ordering::Relaxed).clamp(0, 200) as u16,
            agc: self.agc.load(Ordering::Relaxed),
            denoise: self.denoise.load(Ordering::Relaxed),
            suppression: self.atten_lim_db.load(Ordering::Relaxed),
        }
    }

    fn apply_mic(&self, config: crate::mic_profile::Config) {
        let config = config.normalized();
        let mut processing = self.processing.lock();
        *processing = config.processing;
        self.threshold
            .store(config.threshold as i32, Ordering::Relaxed);
        self.mic_gain_pct
            .store(config.volume as i32, Ordering::Relaxed);
        self.agc.store(config.agc, Ordering::Relaxed);
        self.denoise.store(config.denoise, Ordering::Relaxed);
        self.atten_lim_db
            .store(config.suppression, Ordering::Relaxed);
    }

    fn sync_stream_gains(&self, state: &AppState) {
        *self.stream_gains.lock() = crate::stream_audio::playback_gains(state, &[])
            .into_iter()
            .collect();
        self.watched_streams.send_if_modified(|watched| {
            if *watched == state.screen_viewing {
                return false;
            }
            watched.clone_from(&state.screen_viewing);
            true
        });
    }

    fn from_state(s: &AppState) -> Self {
        Self {
            processing: Arc::new(Mutex::new(s.mic_processing)),
            threshold: Arc::new(AtomicI32::new(s.mic_sensitivity as i32)),
            mic_gain_pct: Arc::new(AtomicI32::new(s.mic_volume as i32)),
            agc: Arc::new(AtomicBool::new(s.auto_gain_control)),
            denoise: Arc::new(AtomicBool::new(s.noise_cancellation)),
            atten_lim_db: Arc::new(AtomicU32::new(s.denoise_atten_lim_db)),
            denoise_retry: Arc::new(AtomicU32::new(s.denoise_retry)),
            bitrate_kbps: Arc::new(AtomicU32::new(s.voice_bitrate_kbps)),
            stats_polling: Arc::new(AtomicBool::new(false)),
            deafened: Arc::new(AtomicBool::new(s.voice.deafened)),
            // Restored volumes reach the mixer here: nothing sends them again.
            gains: Arc::new(Mutex::new(
                s.user_volumes
                    .keys()
                    .chain(s.user_muted.iter())
                    .map(|pk| (pk.clone(), s.voice_gain_of(pk)))
                    .collect(),
            )),
            stream_gains: Arc::new(Mutex::new(HashMap::new())),
            watched_streams: tokio::sync::watch::channel(s.screen_viewing.clone()).0,
            soundboard_pct: Arc::new(AtomicU32::new(s.soundboard_volume.min(100))),
        }
    }
}

#[derive(Default)]
struct MicMeter {
    peak: AtomicI32,
    peak_pre: AtomicI32,
    gate_peak: AtomicI32,
    open: AtomicBool,
    /// 0 normal, 2 paused.
    denoise_fallback: AtomicU8,
}

#[derive(Default)]
struct GateStats {
    ready: AtomicBool,
    ready_notify: tokio::sync::Notify,
    dropped: AtomicU64,
    passed: AtomicU64,
    peak_after: AtomicI32,
    threshold: AtomicI32,
    atten_lim_applied: AtomicU32,
    /// 0 off, 2 RNNoise.
    engine: AtomicU8,
    speaking: AtomicU64,
}

impl GateStats {
    async fn wait_ready(&self) -> Result<(), String> {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while !self.ready.load(Ordering::Acquire) {
                self.ready_notify.notified().await;
            }
        })
        .await
        .map_err(|_| "Timed out preparing microphone processing".to_string())
    }
}

pub const DENOISE_ATTEN_LIM_DB_MIN: u32 = 6;
pub const DENOISE_ATTEN_LIM_DB_MAX: u32 = 60;

pub enum VoiceCmd {
    ApplyMicrophone {
        config: crate::mic_profile::Config,
    },
    RetryNoiseCancellation,
    Connect {
        livekit_url: String,
        alternate_urls: Vec<String>,
        route_revision: u32,
        report_tx: UnboundedSender<ClientMessage>,
        token: String,
        channel_id: Id,
        ice_servers: Vec<IceServer>,
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
    SetBypassSystemProcessing {
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
    #[cfg(any(target_os = "android", target_os = "windows"))]
    SetCamera {
        room: Option<(String, String)>,
        device: Option<String>,
        settings: Signal<crate::settings::ClientSettings>,
        gateway: crate::state::GatewayTx,
    },
    SetStreamVolume {
        pubkey: String,
        gain: f32,
    },
    SyncStreamWatches,
    SetUserVolume {
        pubkey: String,
        gain: f32,
    },
    SetSoundboardVolume {
        percent: u32,
    },
    PlaySound {
        pcm: Arc<[f32]>,
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

#[cfg(test)]
#[path = "voice_audio_tests.rs"]
mod audio_audit_tests;

#[cfg(test)]
#[path = "voice_speech_tests.rs"]
mod speech_audit_tests;

#[path = "voice_endpoints.rs"]
mod voice_endpoints;

pub fn spawn_voice_service(state: Signal<AppState>) -> UnboundedSender<VoiceCmd> {
    let (tx, rx) = unbounded_channel::<VoiceCmd>();
    let controls = AudioControls::from_state(&state.read());
    provide_context(StreamPlaybackControls(controls.stream_gains.clone()));
    crate::service_task::spawn(async move {
        if let Err(e) = service_loop(rx, state, controls).await {
            tracing::error!(error = %e, "voice service loop ended");
        }
    });
    tx
}

async fn service_loop(
    mut rx: UnboundedReceiver<VoiceCmd>,
    mut state: Signal<AppState>,
    controls: AudioControls,
) -> Result<(), String> {
    let mut session: Option<ActiveVoice> = None;
    let mut last_connect: Option<(String, String, Id, Vec<IceServer>)> = None;

    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;
    loop {
        tokio::select! {
            maybe = rx.recv() => {
        let Some(cmd) = maybe else { break };
        match cmd {
            VoiceCmd::Connect {
                livekit_url,
                alternate_urls,
                route_revision,
                report_tx,
                token,
                channel_id,
                ice_servers,
            } => {
                eprintln!("[voice] Connect to {livekit_url} channel={channel_id}");
                last_connect = None;
                if let Some(prev) = session.take() {
                    eprintln!("[voice] shutting down previous session");
                    prev.shutdown(state).await;
                }
                #[cfg(target_os = "android")]
                {
                    if state.peek().server_voice_channel() != Some(channel_id)
                        || state.peek().voice_route_revision != route_revision {
                        continue;
                    }
                    if let Err(error) = crate::android::prepare_voice().await {
                        if state.peek().server_voice_channel() == Some(channel_id)
                            && state.peek().voice_route_revision == route_revision {
                            if let Err(send_error) = report_tx.send(ClientMessage::LeaveVoice) {
                                tracing::debug!(%send_error, "voice gateway closed");
                            }
                            let mut s = state.write();
                            s.error_toast = Some(error.clone());
                            s.voice.phase = VoicePhase::Error;
                            s.voice.error = Some(error);
                            s.voice.channel_id = None;
                        }
                        continue;
                    }
                }
                let result = voice_endpoints::try_endpoints(livekit_url, alternate_urls, |url| {
                    let token = token.clone();
                    let controls = controls.clone();
                    let ice = ice_servers.clone();
                    async move {
                        if state.peek().server_voice_channel() != Some(channel_id)
                            || state.peek().voice_route_revision != route_revision {
                            return Err("voice join cancelled".into());
                        }
                        ActiveVoice::connect(&url, &token, channel_id, ice, state, controls).await
                    }
                }).await;
                if state.peek().server_voice_channel() != Some(channel_id)
                    || state.peek().voice_route_revision != route_revision {
                    if let Ok((active, _)) = result { active.shutdown(state).await; }
                    continue;
                }
                let _ = report_tx.send(ClientMessage::VoiceConnectionReport {
                    channel_id, route_revision, connected: result.is_ok(),
                });
                match result {
                    Ok((active, url)) => {
                        last_connect = Some((url.clone(), token.clone(), channel_id, ice_servers));
                        state.write().set_voice_endpoint(url);
                        eprintln!("[voice] connected ok — phase Connected");
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
                // A newer call is already under way once the server has us in one or a
                // token is minting; wiping now would take its key, channel and screen token.
                let newer_call = {
                    let s = state.peek();
                    s.voice.phase == VoicePhase::Connecting || s.server_voice_channel().is_some()
                };
                if !newer_call {
                    state.write().end_voice_locally();
                }
                tokio::spawn(async {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    crate::audio_diag::log("5 s after leaving");
                });
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
                        if let Ok(name) = crate::audio_device::name(&d) {
                            let is_input = crate::audio_device::selectable(&d, true);
                            let is_output = crate::audio_device::selectable(&d, false);
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
                // The UI already owns the latest selection, including System default.
                // A queued command must not restore an older device over it.
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
            VoiceCmd::ApplyMicrophone { config } => {
                config.apply_state(&mut state.write());
                controls.apply_mic(config);
            }
            VoiceCmd::RetryNoiseCancellation => {
                controls
                    .denoise_retry
                    .store(state.peek().denoise_retry, Ordering::Relaxed);
            }
            VoiceCmd::SetVoiceBitrate { kbps } => {
                let kbps = crate::settings::normalize_voice_bitrate(kbps);
                eprintln!("[voice] SetVoiceBitrate ceiling={kbps}");
                controls.bitrate_kbps.store(kbps, Ordering::Relaxed);
                state.write().voice_bitrate_kbps = kbps;
            }
            VoiceCmd::SetStatsPolling { enabled } => {
                controls.stats_polling.store(enabled, Ordering::Relaxed);
            }
            VoiceCmd::SetSystemAudio { enabled, target } => {
                if let Some(active) = session.as_mut() {
                    #[cfg(target_os = "android")]
                    let enabled = enabled && state.peek().screen_sharing && active.screen_video.is_some();
                    if let Err(e) = active.set_system_audio(enabled, target, state).await {
                        eprintln!("[voice] system audio failed: {e}");
                        #[cfg(target_os = "android")]
                        { state.write().screen_native_audio = false; }
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
                    active.set_screen_audio(room, state, &controls).await;
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
            #[cfg(any(target_os = "android", target_os = "windows"))]
            VoiceCmd::SetCamera {
                room,
                device,
                settings,
                gateway,
            } => {
                let epoch = state.read().voice_session_epoch;
                let was_on = state.read().camera_on;
                if let Some(active) = session.as_mut()
                    && let Err(e) = active
                        .set_camera(room, device, state, settings, gateway.clone())
                        .await
                {
                    let mut s = state.write();
                    if s.voice_session_epoch == epoch && (s.camera_on || s.camera_starting) {
                        s.camera_on = false;
                        s.camera_starting = false;
                        s.error_toast = Some(format!("Couldn't start your camera: {e}"));
                        if was_on {
                            gateway.send(crate::protocol::ClientMessage::SetCamera { on: false });
                        }
                    }
                }
            }
            VoiceCmd::SetStreamVolume { pubkey, gain } => {
                controls.sync_stream_gains(&state.read());
                crate::dlog!(
                    "voice SetStreamVolume pubkey={} requested={gain:.2} applied={:.2}",
                    &pubkey[..pubkey.len().min(8)],
                    controls.stream_gains.lock().get(&pubkey).copied().unwrap_or(0.0)
                );
            }
            VoiceCmd::SyncStreamWatches => controls.sync_stream_gains(&state.read()),
            VoiceCmd::SetUserVolume { pubkey, gain } => {
                let gain = gain.clamp(0.0, 2.0);
                eprintln!(
                    "[voice] SetUserVolume {} gain={gain:.2}",
                    &pubkey[..pubkey.len().min(8)]
                );
                controls.gains.lock().insert(pubkey, gain);
            }
            VoiceCmd::SetSoundboardVolume { percent } => {
                controls
                    .soundboard_pct
                    .store(percent.min(100), Ordering::Relaxed);
            }
            VoiceCmd::PlaySound { pcm } => {
                if let Some(active) = session.as_mut() {
                    if let Err(e) = active.play_sound(pcm).await {
                        eprintln!("[voice] soundboard failed: {e}");
                        state.write().error_toast = Some(format!("Couldn't play that sound: {e}"));
                    }
                } else {
                    eprintln!("[voice] PlaySound ignored — no voice session");
                }
            }
        }
            }
            _ = tick.tick() => {
                controls.sync_stream_gains(&state.read());
                if let Some(active) = session.as_mut() {
                    active.maintain(state, &controls).await;
                }
            }
        }
    }
    eprintln!("[voice] service loop ended (channel closed)");
    Ok(())
}

async fn restart_session(
    session: &mut Option<ActiveVoice>,
    last_connect: &Option<(String, String, Id, Vec<IceServer>)>,
    mut state: Signal<AppState>,
    controls: &AudioControls,
    why: &str,
) {
    if session.is_none() {
        return;
    }
    let Some((url, tok, cid, ice)) = last_connect.clone() else {
        return;
    };
    eprintln!("[voice] Reconnecting to apply {why}");
    if let Some(prev) = session.take() {
        prev.shutdown(state).await;
    }
    match ActiveVoice::connect(&url, &tok, cid, ice, state, controls.clone()).await {
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
    #[cfg(target_os = "android")]
    _background_call: crate::android::BackgroundCall,
    room: Arc<Room>,
    mic: Option<MicCapture>,
    muted: Arc<AtomicBool>,
    frame_tx: crate::audio_queue::AudioSender<f32>,
    gate_stats: Arc<GateStats>,
    local_audio: LocalAudioTrack,
    playback: PlaybackMixer,
    faults: UnboundedSender<AudioFault>,
    faults_rx: UnboundedReceiver<AudioFault>,
    mic_retry_at: Option<Instant>,
    output_retry_at: Option<Instant>,
    device_scan_at: Option<Instant>,
    mic_error: Option<String>,
    output_error: Option<String>,
    event_task: tokio::task::JoinHandle<()>,
    system_audio: Option<SystemAudioTrack>,
    soundboard: Option<SoundboardTrack>,
    screen_audio: Option<ScreenAudioRoom>,
    screen_video: Option<NativeVideoRoom>,
    mixer: PlaybackHandle,
    self_pubkey: Option<String>,
    meter_task: Task,
    stats_task: Task,
    /// Every room of this call dials the same SFU, so every one gets these.
    ice_servers: Vec<IceServer>,
}

/// What a room's ICE agent is told. A host behind NAT is reached through the
/// relay its rendezvous runs; with nothing listed, libwebrtc gathers host
/// candidates only, as before.
pub fn rtc_config(servers: &[IceServer]) -> RtcConfiguration {
    let mut config = RtcConfiguration::default();
    config.ice_servers = servers
        .iter()
        .map(|s| RtcIceServer {
            urls: s.urls.clone(),
            username: s.username.clone(),
            password: s.credential.clone(),
        })
        .collect();
    config.continual_gathering_policy = ContinualGatheringPolicy::GatherOnce;
    config.ice_transport_type = IceTransportsType::All;
    config
}

fn new_muted_microphone_track(source: NativeAudioSource) -> LocalAudioTrack {
    let track = LocalAudioTrack::create_audio_track("mic", RtcAudioSource::Native(source));
    track.mute();
    track
}

fn apply_microphone_mute(track: &LocalAudioTrack, muted: &AtomicBool, value: bool) {
    muted.store(value, Ordering::Relaxed);
    if value {
        track.mute();
    } else {
        track.unmute();
    }
}

impl ActiveVoice {
    async fn connect(
        livekit_url: &str,
        token: &str,
        channel_id: Id,
        ice_servers: Vec<IceServer>,
        state: Signal<AppState>,
        controls: AudioControls,
    ) -> Result<Self, String> {
        #[cfg(target_os = "android")]
        {
            crate::android::check_voice_ready()?;
            crate::android::check_microphone_permission()?;
        }
        #[cfg(target_os = "android")]
        let background_call = crate::android::BackgroundCall::start().await?;
        // Publishing before the channel key arrives can strand peers' decryptors after the first key swap.
        crate::e2ee::ensure_channel_key(channel_id, state).await?;
        let mut options = RoomOptions::default();
        options.auto_subscribe = false;
        options.encryption = crate::e2ee::room_options();
        options.join_retries = 0;
        options.connect_timeout = std::time::Duration::from_secs(4);
        options.signal_connect_timeout = Some(std::time::Duration::from_secs(4));
        options.rtc_config = rtc_config(&ice_servers);
        tracing::info!(
            url = %livekit_url,
            %channel_id,
            ice_urls = ?crate::protocol::ice_urls(&ice_servers),
            "voice route: dialing the voice room"
        );
        let (room, mut events) = Room::connect(livekit_url, token, options)
            .await
            .map_err(|e| format!("livekit connect: {e}"))?;
        let room = Arc::new(room);
        crate::e2ee::register_room(&room, crate::e2ee::RoomKind::Voice);

        let source =
            NativeAudioSource::new(inert_apm_options(), SAMPLE_RATE, CHANNELS, SOURCE_QUEUE_MS);
        let local_audio = new_muted_microphone_track(source.clone());
        let local_audio_for_mute = local_audio.clone();
        let red = !(cfg!(target_os = "android") && crate::e2ee::enabled());
        let mic_publication = match room
            .local_participant()
            .publish_track(
                LocalTrack::Audio(local_audio),
                TrackPublishOptions {
                    source: TrackSource::Microphone,
                    red,
                    // The gate sends zeros, not nothing (trap 38); DTX makes them 2.5 packets/s.
                    dtx: true,
                    audio_encoding: Some(AudioEncoding {
                        max_bitrate: controls.bitrate_kbps.load(Ordering::Relaxed) as u64 * 1000,
                    }),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(publication) => publication,
            Err(error) => {
                let _ = room.close().await;
                return Err(format!("publish mic: {error}"));
            }
        };
        let encrypted = mic_publication.encryption_type() != livekit::e2ee::EncryptionType::None;
        eprintln!(
            "[voice] mic published: encrypted={encrypted}, red={red} (opus in-band FEC unaffected)"
        );
        crate::e2ee::place_new_voice_publication(&room);

        let (frame_tx, frame_rx) = crate::audio_queue::channel::<f32>();
        let (gated_tx, gated_rx) = crate::audio_queue::channel::<i16>();
        let (reference_tx, reference_rx) = crate::audio_queue::reference_channel::<i16>();
        let (faults, faults_rx) = unbounded_channel::<AudioFault>();
        let meter = Arc::new(MicMeter::default());
        let muted = Arc::new(AtomicBool::new(true));
        let gate_stats = Arc::new(GateStats::default());
        // Past the mic publish the room is live and nothing downstream owns it
        // yet, so every way out of here has to close it.
        {
            let controls = controls.clone();
            let meter = meter.clone();
            let muted = muted.clone();
            let gate_stats = gate_stats.clone();
            if let Err(error) = std::thread::Builder::new()
                .name("dxf-mic-dsp".into())
                .spawn(move || {
                    denoise_gate_loop(
                        frame_rx,
                        gated_tx,
                        controls,
                        meter,
                        muted,
                        gate_stats,
                        reference_rx,
                    )
                })
            {
                let _ = room.close().await;
                return Err(format!("spawn mic dsp thread: {error}"));
            }
        }
        if let Err(error) = gate_stats.wait_ready().await {
            let _ = room.close().await;
            return Err(error);
        }
        // DSP initialization yields; a mute changed during that wait must win before capture starts.
        let (start_muted, start_deafened) = {
            let s = state.peek();
            (s.voice.muted || s.voice.deafened, s.voice.deafened)
        };
        controls.deafened.store(start_deafened, Ordering::Relaxed);
        apply_microphone_mute(&local_audio_for_mute, &muted, start_muted);
        tokio::spawn(publish_loop(gated_rx, source.clone()));
        // A missing microphone no longer refuses the join: the track stays
        // published and silent until one appears.
        let (mic, mic_error) = match MicCapture::start(
            frame_tx.clone(),
            state,
            muted.clone(),
            gate_stats.clone(),
            faults.clone(),
        ) {
            Ok(mic) => (Some(mic), None),
            Err(e) => {
                eprintln!("[voice] microphone unavailable: {e}");
                (
                    None,
                    Some(format!("Microphone unavailable — retrying. {e}")),
                )
            }
        };
        crate::audio_diag::log("mic open");
        let meter_task = spawn_meter_task(state, meter);

        controls.sync_stream_gains(&state.read());
        let playback = PlaybackMixer::start(state, controls.clone(), reference_tx, faults.clone());
        let output_error = (!playback.is_open()).then(|| "No output device — retrying".to_string());
        let mixer_handle = playback.handle();

        let (native_audio_tx, mut native_audio_rx) =
            tokio::sync::mpsc::unbounded_channel::<StreamAudio>();
        {
            let mut state = state;
            crate::service_task::spawn(async move {
                while let Some(ev) = native_audio_rx.recv().await {
                    let mut s = state.write();
                    match ev {
                        StreamAudio::Present(id) => {
                            s.stream_has_audio
                                .set(crate::stream_audio::Source::Voice, &id, true);
                        }
                        StreamAudio::Gone(id) => {
                            s.stream_has_audio
                                .set(crate::stream_audio::Source::Voice, &id, false);
                        }
                        StreamAudio::RoomGone => {
                            s.stream_has_audio
                                .clear_source(crate::stream_audio::Source::Voice);
                            crate::dlog!("voice stream audio cleared");
                        }
                    }
                }
            });
        }

        // The banner's dot follows the room itself, so a silent reconnect shows
        // yellow and the return to green does not wait on anything else.
        let (phase_tx, mut phase_rx) = tokio::sync::mpsc::unbounded_channel::<VoicePhase>();
        {
            let mut state = state;
            crate::service_task::spawn(async move {
                while let Some(phase) = phase_rx.recv().await {
                    let current = state.peek().voice.clone();
                    let ours = current.channel_id == Some(channel_id)
                        && matches!(
                            current.phase,
                            VoicePhase::Connecting | VoicePhase::Connected
                        );
                    if ours && current.phase != phase {
                        eprintln!("[voice] room says {phase:?}");
                        let mut s = state.write();
                        s.voice.phase = phase;
                        if phase != VoicePhase::Connected {
                            s.voice_send_route = None;
                            s.voice_receive_route = None;
                        }
                    }
                }
            });
        }

        let (quality_tx, mut quality_rx) = tokio::sync::mpsc::unbounded_channel::<QualityMsg>();
        {
            let mut state = state;
            crate::service_task::spawn(async move {
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

        let self_pubkey = state.peek().self_user.as_ref().map(|u| u.pubkey.clone());
        controls.sync_stream_gains(&state.read());
        subscriptions::refresh(
            &room,
            self_pubkey.as_deref().unwrap_or_default(),
            false,
            &controls.watched_streams.borrow(),
        );
        let event_task = tokio::spawn({
            let mixer_handle = mixer_handle.clone();
            let room = room.clone();
            let local = self_pubkey.clone().unwrap_or_default();
            let mut watched = controls.watched_streams.subscribe();
            async move {
                let mut consumers = tokio::task::JoinSet::new();
                let mut consumer_handles = HashMap::new();
                while let Some(ev) =
                    subscriptions::next_event(&room, &local, false, &mut watched, &mut events).await
                {
                    while consumers.try_join_next().is_some() {}
                    match &ev {
                        RoomEvent::ParticipantConnected(p) => {
                            crate::dlog!("[voice] participant connected: {}", p.identity().0);
                        }
                        RoomEvent::ParticipantDisconnected(p) => {
                            crate::dlog!("[voice] participant left: {}", p.identity().0);
                            let _ = quality_tx.send(QualityMsg::Drop(p.identity().0.clone()));
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
                        }
                        | RoomEvent::TrackUnpublished {
                            participant,
                            publication,
                        } => {
                            if let Some(task) = consumer_handles.remove(&publication.sid()) {
                                tokio::task::AbortHandle::abort(&task);
                            }
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
                            consumers.abort_all();
                            consumer_handles.clear();
                            eprintln!("[voice] room disconnected: {reason:?}");
                            let _ = quality_tx.send(QualityMsg::Clear);
                        }
                        RoomEvent::Reconnecting => {
                            eprintln!("[voice] reconnecting");
                            let _ = phase_tx.send(VoicePhase::Connecting);
                        }
                        RoomEvent::Reconnected => {
                            eprintln!("[voice] reconnected");
                            let _ = phase_tx.send(VoicePhase::Connected);
                        }
                        RoomEvent::ConnectionStateChanged(ConnectionState::Connected) => {
                            let _ = phase_tx.send(VoicePhase::Connected);
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
                        let kind = TrackKind::of(publication.source(), &publication.name());
                        if !subscriptions::wanted(
                            publication.source(),
                            &publication.name(),
                            &participant.identity().0,
                            &local,
                            false,
                            &watched.borrow(),
                        ) {
                            publication.set_subscribed(false);
                            continue;
                        }
                        if let RemoteTrack::Audio(audio) = track {
                            let stream = NativeAudioStream::new(
                                audio.rtc_track(),
                                SAMPLE_RATE as i32,
                                CHANNELS as i32,
                            );
                            let mixer_handle = mixer_handle.clone();
                            let identity = participant.identity().0.clone();
                            if kind == TrackKind::Stream {
                                let _ =
                                    native_audio_tx.send(StreamAudio::Present(identity.clone()));
                            }
                            let task = consumers.spawn(consume_remote_track(
                                stream,
                                mixer_handle,
                                identity,
                                kind,
                            ));
                            if let Some(previous) = consumer_handles.insert(publication.sid(), task)
                            {
                                previous.abort();
                            }
                        }
                    }
                }
                eprintln!("[voice] event stream ended");
                let _ = quality_tx.send(QualityMsg::Clear);
            }
        });

        let stats_task = network::spawn_stats_task(
            state,
            room.clone(),
            local_audio_for_mute.clone(),
            self_pubkey.clone(),
            controls.clone(),
        );

        let active = Self {
            #[cfg(target_os = "android")]
            _background_call: background_call,
            room,
            mic,
            muted,
            frame_tx,
            gate_stats,
            local_audio: local_audio_for_mute,
            playback,
            faults,
            faults_rx,
            mic_retry_at: None,
            output_retry_at: None,
            device_scan_at: None,
            mic_error,
            output_error,
            event_task,
            meter_task,
            stats_task,
            system_audio: None,
            soundboard: None,
            screen_audio: None,
            screen_video: None,
            ice_servers,
            mixer: mixer_handle,
            self_pubkey,
        };
        active.refresh_device_error(state);
        Ok(active)
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
            #[cfg(not(target_os = "android"))]
            return Ok(());
            #[cfg(target_os = "android")]
            if let Some(existing) = self.system_audio.as_ref()
                && existing._capture.is_active().await?
            {
                return Ok(());
            }
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
        let (tx, rx) = crate::audio_queue::channel::<f32>();
        let (fatal_tx, mut fatal_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        #[cfg(target_os = "windows")]
        let capture =
            tokio::task::spawn_blocking(move || crate::sysaudio::start(tx, fatal_tx, target))
                .await
                .map_err(|e| format!("screen-audio capture worker failed: {e}"))??;
        #[cfg(target_os = "android")]
        let capture = crate::sysaudio::start_android(tx, fatal_tx, target).await?;
        #[cfg(not(any(target_os = "android", target_os = "windows")))]
        let capture = crate::sysaudio::start(tx, fatal_tx, target)?;
        {
            let mut state = state;
            crate::service_task::spawn(async move {
                if let Some(e) = fatal_rx.recv().await {
                    eprintln!("[voice] system audio died mid-share: {e}");
                    #[cfg(target_os = "android")]
                    {
                        state.write().screen_native_audio = false;
                    }
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
            SOURCE_QUEUE_MS,
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

    async fn play_sound(&mut self, pcm: Arc<[f32]>) -> Result<(), String> {
        if self.soundboard.is_none() {
            self.soundboard = Some(self.publish_soundboard().await?);
        }
        if let Some(board) = &self.soundboard {
            let _ = board.tx.send(pcm);
        }
        Ok(())
    }

    /// Published on the first play and kept for the session: publishing costs
    /// a signalling round trip, which would land on every click.
    async fn publish_soundboard(&self) -> Result<SoundboardTrack, String> {
        let source = NativeAudioSource::new(
            AudioSourceOptions {
                echo_cancellation: false,
                noise_suppression: false,
                auto_gain_control: false,
            },
            SAMPLE_RATE,
            CHANNELS,
            SOURCE_QUEUE_MS,
        );
        let track = LocalAudioTrack::create_audio_track(
            SOUNDBOARD_TRACK,
            RtcAudioSource::Native(source.clone()),
        );
        let publication = self
            .room
            .local_participant()
            .publish_track(
                LocalTrack::Audio(track),
                TrackPublishOptions {
                    source: TrackSource::Unknown,
                    audio_encoding: Some(
                        livekit::options::audio::MUSIC_HIGH_QUALITY.encoding.clone(),
                    ),
                    dtx: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("publishing it failed ({e})"))?;
        crate::e2ee::place_new_voice_publication(&self.room);
        let (tx, rx) = unbounded_channel::<Arc<[f32]>>();
        let task = tokio::spawn(soundboard_loop(
            rx,
            source,
            self.mixer.clone(),
            self.self_pubkey.clone().unwrap_or_default(),
        ));
        crate::dlog!("voice soundboard track published");
        Ok(SoundboardTrack {
            _sid: publication.sid(),
            tx,
            task,
        })
    }

    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    async fn video_room(&mut self, key: (String, String)) -> Result<&mut NativeVideoRoom, String> {
        if self
            .screen_video
            .as_ref()
            .is_some_and(|room| room.key != key)
            && let Some(room) = self.screen_video.take()
        {
            room.shutdown().await;
        }
        if self.screen_video.is_none() {
            self.screen_video = Some(NativeVideoRoom::connect(key, &self.ice_servers).await?);
        }
        self.screen_video
            .as_mut()
            .ok_or_else(|| "native video room was closed".into())
    }

    async fn close_unused_video_room(&mut self) {
        if self
            .screen_video
            .as_ref()
            .is_some_and(NativeVideoRoom::is_empty)
            && let Some(room) = self.screen_video.take()
        {
            room.shutdown().await;
        }
    }

    async fn set_screen_video(
        &mut self,
        room: Option<(String, String)>,
        target: crate::sysvideo::Target,
        settings: crate::sysvideo::Settings,
        state: Signal<AppState>,
    ) -> Result<(), String> {
        let Some(key) = room else {
            if let Some(room) = self.screen_video.as_mut()
                && let Some(screen) = room.screen.take()
            {
                screen.shutdown().await;
            }
            self.close_unused_video_room().await;
            return Ok(());
        };
        if !crate::sysvideo::supported() {
            return Err("screen capture isn't implemented on this platform".into());
        }
        #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
        {
            if let Some(channel) = state.peek().server_voice_channel() {
                crate::e2ee::ensure_channel_key(channel, state).await?;
            }
            let room = self.video_room(key).await?;
            if room
                .screen
                .as_ref()
                .is_some_and(|screen| screen.key == (target, settings))
            {
                return Ok(());
            }
            if let Some(screen) = room.screen.take() {
                screen.shutdown().await;
            }
            match ScreenVideoRoom::connect(room.room.clone(), target, settings, state).await {
                Ok(screen) => room.screen = Some(screen),
                Err(e) => {
                    self.close_unused_video_room().await;
                    return Err(e);
                }
            }
        }
        #[cfg(not(any(target_os = "android", target_os = "macos", target_os = "windows")))]
        let _ = (key, target, settings, state);
        Ok(())
    }

    #[cfg(any(target_os = "android", target_os = "windows"))]
    async fn set_camera(
        &mut self,
        room: Option<(String, String)>,
        device: Option<String>,
        state: Signal<AppState>,
        settings: Signal<crate::settings::ClientSettings>,
        gateway: crate::state::GatewayTx,
    ) -> Result<(), String> {
        let Some(key) = room else {
            if let Some(room) = self.screen_video.as_mut()
                && let Some(camera) = room.camera.take()
            {
                camera.shutdown().await;
            }
            self.close_unused_video_room().await;
            return Ok(());
        };
        if let Some(channel) = state.peek().server_voice_channel() {
            crate::e2ee::ensure_channel_key(channel, state).await?;
        }
        let room = self.video_room(key).await?;
        if room.camera.as_ref().is_some_and(|camera| {
            camera.key == device && camera.quality == settings.read().camera_quality.normalized()
        }) {
            return Ok(());
        }
        if let Some(camera) = room.camera.take() {
            camera.shutdown().await;
        }
        room.camera_starting = true;
        let result =
            CameraPublisher::connect(room.room.clone(), device, state, settings, gateway).await;
        room.camera_starting = false;
        let mut failure = None;
        match result {
            Ok(camera) => room.camera = Some(camera),
            Err(e) => failure = Some(e),
        }
        if let Some(e) = failure {
            self.close_unused_video_room().await;
            return Err(e);
        }
        Ok(())
    }

    async fn set_screen_audio(
        &mut self,
        room: Option<(String, String)>,
        mut state: Signal<AppState>,
        controls: &AudioControls,
    ) {
        let Some(key) = room else {
            if let Some(prev) = self.screen_audio.take() {
                prev.shutdown().await;
                let mut s = state.write();
                s.stream_has_audio
                    .clear_source(crate::stream_audio::Source::Screen);
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
                s.stream_has_audio
                    .clear_source(crate::stream_audio::Source::Screen);
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
        match ScreenAudioRoom::connect(
            &url,
            &token,
            self.mixer.clone(),
            self_pubkey,
            state,
            &self.ice_servers,
            controls.watched_streams.subscribe(),
        )
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
        apply_microphone_mute(&self.local_audio, &self.muted, muted);
        if let Some(mic) = &self.mic {
            mic.muted.store(muted, Ordering::Relaxed);
        }
    }

    /// Keeps the call alive across a device that came and went: refresh the
    /// list, reopen what is missing, and back off so a device that is still
    /// gone is not spun on.
    async fn maintain(&mut self, mut state: Signal<AppState>, controls: &AudioControls) {
        while let Ok(fault) = self.faults_rx.try_recv() {
            match fault {
                AudioFault::Input => {
                    if let Some(mic) = self.mic.take() {
                        mic.stop();
                    }
                    self.mic_retry_at = Some(Instant::now());
                }
                AudioFault::Output => {
                    self.playback.drop_stream();
                    self.output_retry_at = Some(Instant::now());
                }
            }
        }

        // The device lists are a full cpal enumeration. The fault channel
        // already reports a device that errors, so watching the list can run on
        // a slower cadence than the reopen/backoff below.
        let now = Instant::now();
        if self.device_scan_at.is_none_or(|at| now >= at) {
            self.device_scan_at = Some(now + DEVICE_SCAN);
            let host = cpal::default_host();
            let mut inputs = Vec::new();
            let mut outputs = Vec::new();
            if let Ok(devices) = host.devices() {
                for device in devices {
                    if let Ok(name) = crate::audio_device::name(&device) {
                        if crate::audio_device::selectable(&device, true) {
                            inputs.push(name.clone());
                        }
                        if crate::audio_device::selectable(&device, false) {
                            outputs.push(name);
                        }
                    }
                }
            }
            {
                let mut s = state.write();
                if s.available_input_devices != inputs {
                    s.available_input_devices = inputs.clone();
                }
                if s.available_output_devices != outputs {
                    s.available_output_devices = outputs.clone();
                }
            }
            // Watch the device actually in use, not the selection: `pick_device`
            // silently falls back to the default, and re-checking a still-selected
            // but unplugged name would reopen the fallback every tick.
            let mic_device = self
                .mic
                .as_ref()
                .and_then(|mic| mic.device_name())
                .map(str::to_string);
            // AAudio's virtual default route is absent from physical device enumeration.
            #[cfg(target_os = "android")]
            let (default_input, default_output) = (
                host.default_input_device()
                    .and_then(|device| crate::audio_device::name(&device).ok()),
                host.default_output_device()
                    .and_then(|device| crate::audio_device::name(&device).ok()),
            );
            #[cfg(not(target_os = "android"))]
            let (default_input, default_output): (Option<String>, Option<String>) = (None, None);
            if let Some(name) = mic_device.as_deref()
                && crate::audio_device::missing(name, &inputs, default_input.as_deref())
            {
                if let Some(mic) = self.mic.take() {
                    mic.stop();
                }
                self.mic_retry_at = Some(now);
            }
            let output_device = self.playback.device_name().map(str::to_string);
            if let Some(name) = output_device.as_deref()
                && crate::audio_device::missing(name, &outputs, default_output.as_deref())
            {
                self.playback.drop_stream();
                self.output_retry_at = Some(now);
            }
        }
        if self.mic.is_none() && self.mic_retry_at.is_none_or(|at| now >= at) {
            let faults = self.faults.clone();
            match MicCapture::start(
                self.frame_tx.clone(),
                state,
                self.muted.clone(),
                self.gate_stats.clone(),
                faults,
            ) {
                Ok(mic) => {
                    eprintln!("[voice] microphone reopened");
                    self.mic = Some(mic);
                    self.mic_retry_at = None;
                    self.mic_error = None;
                }
                Err(e) => {
                    self.mic_retry_at = Some(now + DEVICE_RETRY);
                    self.mic_error = Some(format!("Microphone unavailable — retrying. {e}"));
                }
            }
        }
        if !self.playback.is_open() && self.output_retry_at.is_none_or(|at| now >= at) {
            let faults = self.faults.clone();
            match self.playback.try_open(&mut state, controls, &faults) {
                Ok(()) => {
                    eprintln!("[voice] output reopened");
                    self.output_retry_at = None;
                    self.output_error = None;
                }
                Err(e) => {
                    self.output_retry_at = Some(now + DEVICE_RETRY);
                    self.output_error = Some(format!("No output device — retrying. {e}"));
                }
            }
        }
        publish_audio_devices(state, self.mic.as_ref(), &self.playback);
        self.refresh_device_error(state);
    }

    /// One banner line for whatever is down. Both down is its own message: the
    /// output block used to overwrite the microphone's every tick, so a user
    /// whose headset took both lost sight of the microphone failure.
    fn refresh_device_error(&self, mut state: Signal<AppState>) {
        let combined = match (self.mic_error.as_deref(), self.output_error.as_deref()) {
            (Some(_), Some(_)) => {
                Some("Microphone and output device unavailable — retrying".to_string())
            }
            (Some(mic), None) => Some(mic.to_string()),
            (None, Some(output)) => Some(output.to_string()),
            (None, None) => None,
        };
        let mut s = state.write();
        if s.voice.error != combined {
            s.voice.error = combined;
        }
    }

    async fn shutdown(self, mut state: Signal<AppState>) {
        self.event_task.abort();
        self.meter_task.cancel();
        self.stats_task.cancel();
        if let Some(mic) = self.mic {
            mic.stop();
        }
        crate::audio_diag::log("mic dropped");
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
            s.voice_ping_ms = None;
            s.voice_uplink_congested = false;
            s.voice_send_route = None;
            s.voice_receive_route = None;
        }
        match tokio::time::timeout(ROOM_CLOSE_TIMEOUT, self.room.close()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("[voice] room close failed: {e}"),
            Err(_) => eprintln!("[voice] room close timed out, dropping anyway"),
        }
    }
}

struct NativeVideoRoom {
    room: Arc<Room>,
    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    key: (String, String),
    screen: Option<ScreenVideoRoom>,
    #[cfg(any(target_os = "android", target_os = "windows"))]
    camera: Option<CameraPublisher>,
    /// A camera that is still opening owns the room; closing it under the
    /// publication would fail its negotiation.
    #[cfg(any(target_os = "android", target_os = "windows"))]
    camera_starting: bool,
}

impl NativeVideoRoom {
    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    async fn connect(key: (String, String), ice_servers: &[IceServer]) -> Result<Self, String> {
        let mut options = RoomOptions::default();
        options.auto_subscribe = false;
        options.encryption = crate::e2ee::room_options();
        options.rtc_config = rtc_config(ice_servers);
        let (room, mut events) = Room::connect(&key.0, &key.1, options)
            .await
            .map_err(|e| format!("livekit connect: {e}"))?;
        let room = Arc::new(room);
        crate::e2ee::register_room(&room, crate::e2ee::RoomKind::Screen);
        tokio::spawn(async move { while events.recv().await.is_some() {} });
        Ok(Self {
            room,
            key,
            screen: None,
            #[cfg(any(target_os = "android", target_os = "windows"))]
            camera: None,
            #[cfg(any(target_os = "android", target_os = "windows"))]
            camera_starting: false,
        })
    }
    fn is_empty(&self) -> bool {
        #[cfg(any(target_os = "android", target_os = "windows"))]
        if self.camera.is_some() || self.camera_starting {
            return false;
        }
        self.screen.is_none()
    }
    async fn shutdown(self) {
        if let Some(screen) = self.screen {
            screen.shutdown().await;
        }
        #[cfg(any(target_os = "android", target_os = "windows"))]
        if let Some(camera) = self.camera {
            camera.shutdown().await;
        }
        match tokio::time::timeout(ROOM_CLOSE_TIMEOUT, self.room.close()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("[video] room close failed: {e}"),
            Err(_) => eprintln!("[video] room close timed out"),
        }
    }
}

struct ScreenVideoRoom {
    room: Arc<Room>,
    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    sid: TrackSid,
    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    stats_task: Task,
    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    network_task: Task,
    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    fatal_task: Task,
    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    state: Signal<AppState>,
    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    _capture: crate::sysvideo::Capture,
    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    key: (crate::sysvideo::Target, crate::sysvideo::Settings),
}

#[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
fn screen_video_options(settings: crate::sysvideo::Settings) -> TrackPublishOptions {
    TrackPublishOptions {
        source: TrackSource::Screenshare,
        video_codec: match settings.codec {
            crate::sysvideo::Codec::Auto | crate::sysvideo::Codec::H264 => VideoCodec::H264,
            crate::sysvideo::Codec::Vp8 => VideoCodec::VP8,
        },
        video_encoder: match settings.encoder {
            crate::sysvideo::Encoder::Auto | crate::sysvideo::Encoder::Gpu => {
                VideoEncoderBackend::Hardware
            }
            crate::sysvideo::Encoder::Cpu => VideoEncoderBackend::Software,
        },
        degradation_preference: Some(match settings.priority {
            crate::sysvideo::Priority::Motion => DegradationPreference::MaintainFramerate,
            crate::sysvideo::Priority::Detail => DegradationPreference::MaintainResolution,
            crate::sysvideo::Priority::Balanced => DegradationPreference::Balanced,
        }),
        // Explicit presets avoid LiveKit's 3 FPS screen-share fallback.
        simulcast: settings.adaptive_quality && settings.height > 360,
        simulcast_layers: (settings.adaptive_quality && settings.height > 360).then(|| {
            let height = if settings.height <= 720 { 360 } else { 720 };
            vec![livekit::options::VideoPreset::new(
                settings.width * height / settings.height,
                height,
                if height == 360 { 500_000 } else { 1_500_000 },
                settings.fps.min(30) as f64,
            )]
        }),
        video_encoding: Some(VideoEncoding {
            max_framerate: settings.fps as f64,
            max_bitrate: settings.max_bitrate,
        }),
        video_start_bitrate: Some(
            match (settings.height, settings.fps) {
                (720, 60) => 3_000_000,
                (720, _) => 2_000_000,
                (1080, 60) => 6_000_000,
                (1080, _) => 4_000_000,
                (1440, 30) => 8_000_000,
                (1440, 60) => 10_000_000,
                (1440, _) => 5_000_000,
                (2160, 60) => 14_000_000,
                (2160, _) => 12_000_000,
                _ => (settings.max_bitrate / 2).clamp(1_000_000, 4_000_000),
            }
            .min(settings.max_bitrate)
            .saturating_add(if settings.adaptive_quality && settings.height > 360 {
                if settings.height <= 720 {
                    500_000
                } else {
                    1_500_000
                }
            } else {
                0
            }),
        ),
        ..Default::default()
    }
}

impl ScreenVideoRoom {
    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    async fn connect(
        room: Arc<Room>,
        target: crate::sysvideo::Target,
        settings: crate::sysvideo::Settings,
        state: Signal<AppState>,
    ) -> Result<Self, String> {
        let mut publish_settings = settings;
        publish_settings.codec = crate::sysvideo::codec_capabilities()
            .await?
            .resolve(settings.codec, settings.encoder)?;
        if settings.adaptive_quality || cfg!(target_os = "android") {
            let sources = tokio::task::spawn_blocking(crate::sysvideo::sources)
                .await
                .map_err(|e| format!("screen source lookup failed: {e}"))??;
            if let Some(actual) = sources.iter().find(|s| s.target == target) {
                (publish_settings.width, publish_settings.height) =
                    crate::sysvideo::fit_resolution(actual.width, actual.height, settings);
            }
        }
        let source = NativeVideoSource::new(
            VideoResolution {
                width: publish_settings.width,
                height: publish_settings.height,
            },
            true,
        );
        let track =
            LocalVideoTrack::create_video_track("screen", RtcVideoSource::Native(source.clone()));

        let publication = room
            .local_participant()
            .publish_track(
                LocalTrack::Video(track.clone()),
                screen_video_options(publish_settings),
            )
            .await;
        let publication = match publication {
            Ok(publication) => publication,
            Err(error) if publish_settings.adaptive_quality => {
                tracing::warn!(%error, "simulcast publication failed; retrying a single quality");
                publish_settings.adaptive_quality = false;
                let publication = room
                    .local_participant()
                    .publish_track(
                        LocalTrack::Video(track.clone()),
                        screen_video_options(publish_settings),
                    )
                    .await
                    .map_err(|e| format!("publishing the video track failed ({e})"))?;
                let mut state = state;
                state.write().error_toast = Some(
                    "Adaptive quality is unavailable. Sharing a single quality instead.".into(),
                );
                publication
            }
            Err(error) => {
                return Err(format!("publishing the video track failed ({error})"));
            }
        };
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
        let encoder_fatal = fatal_tx.clone();
        let capture_epoch = state.read().voice_session_epoch;
        let fatal_task = {
            let mut state = state;
            crate::service_task::spawn(async move {
                if let Some(e) = fatal_rx.recv().await {
                    eprintln!("[voice] screen capture died mid-share: {e}");
                    let mut s = state.write();
                    if s.voice_session_epoch != capture_epoch
                        || s.screen_share_target != Some(target)
                    {
                        return;
                    }
                    s.screen_sharing = false;
                    s.screen_share_target = None;
                    s.error_toast =
                        Some(format!("Your screen stopped being shared: {e}. Try again."));
                }
            })
        };

        let adapter_dropped = Arc::new(AtomicU64::new(0));
        let sink_adapter_dropped = adapter_dropped.clone();
        let sink: crate::sysvideo::FrameSink = Box::new(move |frame: crate::sysvideo::Frame| {
            #[cfg(any(target_os = "android", target_os = "windows"))]
            let timestamp_us = frame.timestamp_us;
            #[cfg(target_os = "macos")]
            let timestamp_us = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros() as i64)
                .unwrap_or(0);
            #[cfg(target_os = "android")]
            let rotation: VideoRotation = frame.rotation;
            #[cfg(not(target_os = "android"))]
            let rotation = VideoRotation::VideoRotation0;
            #[cfg(target_os = "macos")]
            // SAFETY: LiveKit takes ownership of the retained CVPixelBuffer reference.
            let buffer =
                unsafe { NativeBuffer::from_cv_pixel_buffer(frame.into_consumable_pixel_buffer()) };
            #[cfg(any(target_os = "android", target_os = "windows"))]
            let buffer = frame.buffer;
            if !source.capture_frame(&VideoFrame {
                rotation,
                timestamp_us,
                frame_metadata: None,
                buffer,
            }) {
                sink_adapter_dropped.fetch_add(1, Ordering::Relaxed);
            }
        });
        #[cfg(target_os = "android")]
        let capture = crate::sysvideo::start(target, settings, sink, fatal_tx).await;
        #[cfg(target_os = "macos")]
        let capture = crate::sysvideo::start(target, settings, sink, fatal_tx);
        #[cfg(target_os = "windows")]
        let capture = tokio::task::spawn_blocking(move || {
            crate::sysvideo::start(target, settings, sink, fatal_tx)
        })
        .await
        .unwrap_or_else(|e| Err(format!("capture worker failed: {e}")));
        let capture = match capture {
            Ok(capture) => capture,
            Err(error) => {
                fatal_task.cancel();
                if let Err(e) = room
                    .local_participant()
                    .unpublish_track(&publication.sid())
                    .await
                {
                    eprintln!("[screen] failed capture publication cleanup: {e}");
                }
                return Err(error);
            }
        };

        #[cfg(target_os = "android")]
        let capture_control = capture.control();

        let mut stats_state = state;
        let capture_metrics = capture.metrics();
        let stats_sid = publication.sid();
        let status_room = room.clone();
        let network_task = network::spawn_video_budget(state, track.clone(), 300_000);
        let stats_task = crate::service_task::spawn(async move {
            #[cfg(target_os = "android")]
            let mut mobile_budget =
                crate::sysvideo::mobile_budget::MobileBudget::warm_start(settings, Instant::now());
            #[cfg(target_os = "android")]
            let mut pressure_at = Instant::now();
            let mut previous = None;
            let mut capture_previous = capture_metrics.snapshot();
            let mut encode_previous = None;
            let mut sampled = Instant::now();
            let mut status_sent = Instant::now() - std::time::Duration::from_secs(3);
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let now = Instant::now();
                let captured = capture_metrics.snapshot();
                let capture_fps = captured.frames.saturating_sub(capture_previous.frames) as f64
                    / now.duration_since(sampled).as_secs_f64();
                let capture_processing_ms = captured.processing_ms_since(capture_previous);
                let capture_callbacks_fps = captured
                    .callbacks
                    .saturating_sub(capture_previous.callbacks)
                    as f64
                    / now.duration_since(sampled).as_secs_f64();
                let capture_paced_out = captured
                    .paced_out
                    .saturating_sub(capture_previous.paced_out);
                let capture_readback_ms = captured.readback_ms_since(capture_previous);
                let capture_conversion_ms = captured.conversion_ms_since(capture_previous);
                let capture_copy_ms = captured.copy_ms_since(capture_previous);
                capture_previous = captured;
                sampled = now;
                let Ok(report) = track.get_stats().await else {
                    continue;
                };
                let Some(outbound) = report
                    .iter()
                    .filter_map(|entry| match entry {
                        RtcStats::OutboundRtp(o) if o.stream.kind == "video" => Some(o),
                        _ => None,
                    })
                    .max_by_key(|o| {
                        (
                            o.outbound.rid == "f",
                            o.outbound.frame_width as u64 * o.outbound.frame_height as u64,
                        )
                    })
                else {
                    continue;
                };
                if settings.encoder == crate::sysvideo::Encoder::Gpu && {
                    let implementation = outbound
                        .outbound
                        .encoder_implementation
                        .to_ascii_lowercase();
                    implementation.contains("openh264") || implementation.contains("libvpx")
                } {
                    if encoder_fatal
                        .send(format!(
                            "{} encoding failed. Choose Automatic or CPU.",
                            crate::sysvideo::hardware_encoder_label()
                        ))
                        .is_err()
                    {
                        eprintln!("[screen] encoder failure after session teardown");
                    }
                    break;
                }
                let (packets, bytes) =
                    report
                        .iter()
                        .fold((0_u64, 0_u64), |(packets, bytes), entry| {
                            if let RtcStats::OutboundRtp(o) = entry
                                && o.stream.kind == "video"
                            {
                                (packets + o.sent.packets_sent, bytes + o.sent.bytes_sent)
                            } else {
                                (packets, bytes)
                            }
                        });
                let rates = outbound_rates(previous, packets, bytes, now);
                previous = Some((packets, bytes, now));
                let encode_ms = screen_encode_ms(
                    encode_previous,
                    outbound.outbound.frames_encoded,
                    outbound.outbound.total_encode_time,
                );
                encode_previous = Some((
                    outbound.outbound.frames_encoded,
                    outbound.outbound.total_encode_time,
                ));
                #[cfg(target_os = "android")]
                if now.duration_since(pressure_at) >= std::time::Duration::from_secs(3) {
                    pressure_at = now;
                    if let Ok(pressure) = crate::android::screen_capture_pressure().await {
                        let input = crate::sysvideo::mobile_budget::Pressure {
                            active_frames: capture_fps >= mobile_budget.current().fps as f64 * 0.5,
                            sustaining_fps: capture_fps >= mobile_budget.current().fps as f64 * 0.9
                                && outbound.outbound.frames_per_second
                                    >= mobile_budget.current().fps as f64 * 0.9,
                            thermal: pressure & 7,
                            low_memory: pressure & 8 != 0,
                            cpu_limited: outbound.outbound.quality_limitation_reason
                                == livekit::webrtc::stats::QualityLimitationReason::Cpu
                                && capture_fps >= mobile_budget.current().fps as f64 * 0.8,
                            processing_ms: capture_processing_ms,
                            encode_ms,
                        };
                        if let Some((level, next)) = mobile_budget.sample(now, input) {
                            match capture_control.apply(next).await {
                                Ok(()) => {
                                    mobile_budget.applied(level, now);
                                    tracing::info!(
                                        level,
                                        width = next.width,
                                        height = next.height,
                                        fps = next.fps,
                                        pressure,
                                        "Android screen capture budget changed"
                                    );
                                }
                                Err(error) => {
                                    tracing::debug!(%error, "Android screen capture budget unavailable")
                                }
                            }
                        }
                    }
                }
                #[cfg(target_os = "android")]
                let effective_settings = mobile_budget.current();
                #[cfg(not(target_os = "android"))]
                let effective_settings = settings;
                if now.duration_since(status_sent).as_secs() >= 3 {
                    let (width, height) = crate::sysvideo::fit_resolution(
                        captured.width,
                        captured.height,
                        effective_settings,
                    );
                    let payload = serde_json::json!({
                        "sid": stats_sid.to_string(),
                        "width": width, "height": height,
                        "primaryWidth": outbound.outbound.frame_width,
                        "primaryHeight": outbound.outbound.frame_height,
                        "primaryFps": outbound.outbound.frames_per_second,
                        "captureFps": capture_fps,
                        "adaptive": publish_settings.adaptive_quality,
                        "bandwidth": outbound.outbound.quality_limitation_reason
                            == livekit::webrtc::stats::QualityLimitationReason::Bandwidth,
                        "cpu": outbound.outbound.quality_limitation_reason
                            == livekit::webrtc::stats::QualityLimitationReason::Cpu,
                        "captureLimited": super::screen_stats::screen_device_limited(
                            effective_settings.fps as f64, capture_fps, outbound.outbound.frames_per_second,
                            capture_processing_ms, encode_ms),
                    })
                    .to_string()
                    .into_bytes();
                    if let Err(error) = status_room
                        .local_participant()
                        .publish_data(livekit::DataPacket {
                            payload,
                            topic: Some("discordia.screen-quality.v1".into()),
                            ..Default::default()
                        })
                        .await
                    {
                        tracing::debug!(%error, "screen quality status unavailable");
                    }
                    status_sent = now;
                }
                let mut s = stats_state.write();
                if s.voice_session_epoch != capture_epoch
                    || s.screen_share_target != Some(target)
                    || !s.screen_sharing
                {
                    break;
                }
                let mut stats = native_screen_stats(
                    outbound,
                    &report,
                    capture_fps,
                    rates.map(|(_, rate)| rate),
                );
                stats.capture_width = (captured.width > 0).then_some(captured.width);
                stats.packets = Some(packets);
                stats.capture_height = (captured.height > 0).then_some(captured.height);
                stats.capture_processing_ms = capture_processing_ms;
                stats.capture_conversion_ms = capture_conversion_ms;
                stats.capture_copy_ms = capture_copy_ms;
                stats.capture_callbacks_fps =
                    (captured.callbacks > 0).then_some(capture_callbacks_fps);
                stats.capture_paced_out = (captured.callbacks > 0).then_some(captured.paced_out);
                stats.adapter_dropped_frames = Some(adapter_dropped.load(Ordering::Relaxed));
                stats.encode_ms = encode_ms;
                stats.device_limited = super::screen_stats::screen_device_limited(
                    effective_settings.fps as f64,
                    capture_fps,
                    outbound.outbound.frames_per_second,
                    capture_processing_ms,
                    encode_ms,
                );
                tracing::debug!(
                    max_bitrate_bps = settings.max_bitrate,
                    capture_callbacks_fps,
                    capture_paced_out,
                    capture_largest_gap_ms = captured.largest_gap_us as f64 / 1000.0,
                    capture_readback_ms,
                    capture_conversion_ms,
                    capture_copy_ms,
                    track_sid = %stats_sid,
                    key_frames_encoded = outbound.outbound.key_frames_encoded,
                    nack_count = outbound.outbound.nack_count,
                    pli_count = outbound.outbound.pli_count,
                    retransmitted_packets = outbound.outbound.retransmitted_packets_sent,
                    ?stats,
                    "native screen share stats"
                );
                s.screen_share_stats = Some(stats);
            }
        });

        Ok(Self {
            room,
            stats_task,
            network_task,
            fatal_task,
            state,
            _capture: capture,
            sid: publication.sid(),
            key: (target, settings),
        })
    }

    async fn shutdown(self) {
        #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
        {
            self.stats_task.cancel();
            self.network_task.cancel();
            self.fatal_task.cancel();
            let mut state = self.state;
            state.write().screen_share_stats = None;
            #[cfg(target_os = "windows")]
            if let Err(e) = tokio::task::spawn_blocking(move || drop(self._capture)).await {
                eprintln!("[screen] capture stop failed: {e}");
            }
            #[cfg(any(target_os = "android", target_os = "macos"))]
            drop(self._capture);
            if let Err(e) = self
                .room
                .local_participant()
                .unpublish_track(&self.sid)
                .await
            {
                eprintln!("[screen] unpublish failed: {e}");
            }
        }
        #[cfg(not(any(target_os = "android", target_os = "macos", target_os = "windows")))]
        let _ = self.room;
    }
}

#[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
fn native_screen_stats(
    o: &livekit::webrtc::stats::OutboundRtpStats,
    report: &[RtcStats],
    capture_fps: f64,
    bitrate_kbps: Option<u32>,
) -> crate::state::ScreenShareStats {
    use livekit::webrtc::stats::QualityLimitationReason;
    let codec = report.iter().find_map(|entry| match entry {
        RtcStats::Codec(c) if c.rtc.id == o.stream.codec_id => Some(c.codec.mime_type.clone()),
        _ => None,
    });
    let encoder = (!o.outbound.encoder_implementation.is_empty())
        .then(|| o.outbound.encoder_implementation.clone());
    crate::state::ScreenShareStats {
        outbound: true,
        capture_width: None,
        capture_height: None,
        capture_fps: Some(capture_fps),
        capture_processing_ms: None,
        capture_conversion_ms: None,
        capture_copy_ms: None,
        capture_callbacks_fps: None,
        capture_paced_out: None,
        adapter_dropped_frames: None,
        encode_ms: None,
        encoded_width: (o.outbound.frame_width > 0).then_some(o.outbound.frame_width),
        encoded_height: (o.outbound.frame_height > 0).then_some(o.outbound.frame_height),
        encoded_fps: Some(o.outbound.frames_per_second),
        bitrate_kbps,
        target_bitrate_kbps: (o.outbound.target_bitrate > 0.0)
            .then_some((o.outbound.target_bitrate / 1000.0).round() as u32),
        codec,
        power_efficient: encoder.as_ref().map(|_| o.outbound.power_efficient_encoder),
        codec_implementation: encoder,
        quality_limitation_reason: Some(
            match o.outbound.quality_limitation_reason {
                QualityLimitationReason::None => "none",
                QualityLimitationReason::Cpu => "cpu",
                QualityLimitationReason::Bandwidth => "bandwidth",
                QualityLimitationReason::Other => "other",
            }
            .into(),
        ),
        device_limited: false,
        frames: Some(o.outbound.frames_encoded as u64),
        packets: Some(o.sent.packets_sent),
        packets_lost: None,
        jitter_ms: None,
        reception: None,
        error: None,
    }
}

#[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
fn screen_encode_ms(previous: Option<(u32, f64)>, frames: u32, seconds: f64) -> Option<f64> {
    let (before_frames, before_seconds) = previous?;
    let count = frames.checked_sub(before_frames)?;
    let elapsed = seconds - before_seconds;
    (count > 0 && elapsed.is_finite() && elapsed >= 0.0).then(|| elapsed * 1000.0 / count as f64)
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
        ice_servers: &[IceServer],
        mut watched: tokio::sync::watch::Receiver<HashSet<String>>,
    ) -> Result<Self, String> {
        #[cfg(target_os = "android")]
        crate::android::ensure_media_ready().await?;
        let mut options = RoomOptions::default();
        options.auto_subscribe = false;
        options.encryption = crate::e2ee::room_options();
        options.rtc_config = rtc_config(ice_servers);
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
                            s.stream_has_audio
                                .set(crate::stream_audio::Source::Screen, &id, true);
                        }
                        StreamAudio::Gone(id) => {
                            s.stream_has_audio
                                .set(crate::stream_audio::Source::Screen, &id, false);
                        }
                        StreamAudio::RoomGone => {
                            s.stream_has_audio
                                .clear_source(crate::stream_audio::Source::Screen);
                            crate::dlog!(
                                "voice screen_audio RoomGone -> joined=false (playback back to webview)"
                            );
                            s.screen_audio_joined = false;
                        }
                    }
                }
            });
        }

        subscriptions::refresh(&room, &self_pubkey, true, &watched.borrow());

        let alive = Arc::new(AtomicBool::new(true));
        let event_task = tokio::spawn({
            let self_pubkey = self_pubkey.clone();
            let alive = alive.clone();
            let room = room.clone();
            async move {
                let mut consumers = tokio::task::JoinSet::new();
                let mut consumer_handles = HashMap::new();
                while let Some(ev) =
                    subscriptions::next_event(&room, &self_pubkey, true, &mut watched, &mut events)
                        .await
                {
                    while consumers.try_join_next().is_some() {}
                    match ev {
                        RoomEvent::TrackSubscribed {
                            track: RemoteTrack::Audio(audio),
                            participant,
                            publication,
                        } => {
                            if !subscriptions::wanted(
                                publication.source(),
                                &publication.name(),
                                &participant.identity().0,
                                &self_pubkey,
                                true,
                                &watched.borrow(),
                            ) {
                                publication.set_subscribed(false);
                                continue;
                            }
                            let identity = participant.identity().0.clone();
                            let stream = NativeAudioStream::new(
                                audio.rtc_track(),
                                SAMPLE_RATE as i32,
                                CHANNELS as i32,
                            );
                            let _ = has_tx.send(StreamAudio::Present(identity.clone()));
                            let task = consumers.spawn(consume_remote_track(
                                stream,
                                mixer.clone(),
                                identity,
                                TrackKind::Stream,
                            ));
                            if let Some(previous) = consumer_handles.insert(publication.sid(), task)
                            {
                                previous.abort();
                            }
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
                            if let Some(task) = consumer_handles.remove(&publication.sid()) {
                                tokio::task::AbortHandle::abort(&task);
                            }
                            if publication.source() == TrackSource::ScreenshareAudio {
                                let _ = has_tx
                                    .send(StreamAudio::Gone(participant.identity().0.clone()));
                            }
                        }
                        RoomEvent::Disconnected { reason } => {
                            consumers.abort_all();
                            consumer_handles.clear();
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
            key: (url.to_string(), token.to_string()),
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

fn denoise_gate_loop(
    mut frame_rx: crate::audio_queue::AudioReceiver<f32>,
    out_tx: crate::audio_queue::AudioSender<i16>,
    controls: AudioControls,
    meter: Arc<MicMeter>,
    muted: Arc<AtomicBool>,
    stats: Arc<GateStats>,
    reference_rx: crate::audio_queue::AudioReceiver<i16>,
) {
    let mut processor = dsp::Processor::new(reference_rx);
    processor.prepare(controls.mic_config());
    stats.ready.store(true, Ordering::Release);
    stats.ready_notify.notify_one();
    let mut retry = controls.denoise_retry.load(Ordering::Relaxed);
    let mut trace = crate::voice_record::MicrophoneTrace::new("mic-processing", "mic-processed");
    let mut dsp_csv = crate::voice_record::Csv::new("mic-dsp", MIC_DSP_HEADER);
    let mut lead_in = activity::LeadIn::new();
    let mut transmission = Vec::with_capacity(4);
    let mut capture_id = None;
    while let Some(packet) = frame_rx.blocking_recv() {
        let next_capture = packet.position.map(|position| position.capture_id);
        if next_capture != capture_id {
            processor.silence();
            lead_in.clear();
            capture_id = next_capture;
        }
        if !packet.is_fresh() {
            processor.silence();
            lead_in.clear();
            trace.record_float(
                packet.position,
                packet.captured,
                "stale_input",
                &[0.0; FRAME_SAMPLES],
            );
            dsp_csv.row(stale_dsp_row);
            continue;
        }
        let config = controls.mic_config();
        let asked = controls.denoise_retry.load(Ordering::Relaxed);
        if asked != retry {
            retry = asked;
            processor.retry_denoise();
        }
        let raw_dbfs = crate::voice_record::rms_dbfs(&packet.samples);
        let is_muted = muted.load(Ordering::Relaxed);
        let processed = processor.process(packet.samples, config, is_muted);
        lead_in.push(
            crate::audio_queue::Frame {
                samples: processed.samples,
                captured: packet.captured,
                position: packet.position,
            },
            processed.action,
            config,
            is_muted,
            &mut transmission,
        );
        meter.denoise_fallback.store(
            match processor.denoise_fallback() {
                None => 0,
                Some(crate::denoise::Fallback::Off) => 2,
            },
            Ordering::Relaxed,
        );
        meter.bump_peak_pre(processed.pre);
        meter
            .gate_peak
            .fetch_max(processed.gate_peak, Ordering::Relaxed);
        meter.bump_peak(processed.peak);
        stats
            .peak_after
            .fetch_max(processed.peak, Ordering::Relaxed);
        stats
            .threshold
            .store(config.threshold as i32, Ordering::Relaxed);
        stats.atten_lim_applied.store(
            if processed.engine.is_some() {
                config.suppression
            } else {
                0
            },
            Ordering::Relaxed,
        );
        stats.engine.store(
            match processed.engine {
                None => 0,
                Some(crate::denoise::Engine::RNNoise) => 2,
            },
            Ordering::Relaxed,
        );
        if processed.speech > SPEECH_PROBABILITY {
            stats.speaking.fetch_add(1, Ordering::Relaxed);
        }
        let open = matches!(processed.action, GateAction::Pass | GateAction::RampIn);
        meter.open.store(open, Ordering::Relaxed);
        let sent = processed.action != GateAction::Drop;
        dsp_csv.row(|| {
            format!(
                "{raw_dbfs:.1},{:.1},{:.1},{:.2},{:.1},{:?},{},{},{},{},{},{},{},{:.2},{},{},{},{},{},{}",
                processed.echoed_dbfs,
                processed.denoised_dbfs,
                processed.agc_gain_db,
                if sent {
                    crate::voice_record::rms_dbfs(&processed.samples)
                } else {
                    -120.0
                },
                processed.action,
                config.threshold,
                config.agc,
                config.denoise,
                config.suppression,
                config.volume,
                packet.captured.elapsed().as_millis(),
                processed
                    .engine
                    .map_or("off", crate::denoise::Engine::label),
                processed.speech,
                "",
                config.processing.echo,
                config.processing.high_pass,
                config.processing.gate,
                config.processing.headphones,
                config.processing.voice_detection,
            )
        });
        for outgoing in transmission.drain(..) {
            if outgoing.silent {
                stats.dropped.fetch_add(1, Ordering::Relaxed);
            } else {
                stats.passed.fetch_add(1, Ordering::Relaxed);
            }
            let data = outgoing
                .frame
                .samples
                .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
            let packet = crate::audio_queue::Frame {
                samples: data,
                captured: outgoing.frame.captured,
                position: outgoing.frame.position,
            };
            let (position, captured) = (packet.position, packet.captured);
            let outcome = if packet.is_fresh() {
                match crate::audio_queue::offer(&out_tx, packet) {
                    Ok(true) if outgoing.silent => "gate_drop",
                    Ok(true) if outgoing.lead_in => "lead_in_queued",
                    Ok(true) => "queued",
                    Ok(false) => "publisher_full",
                    Err(()) => "publisher_closed",
                }
            } else {
                "stale_processed"
            };
            trace.record_float(position, captured, outcome, &outgoing.frame.samples);
            if outcome == "publisher_closed" {
                return;
            }
        }
    }
}

const MIC_DSP_HEADER: &str = "raw_dbfs,after_echo_dbfs,after_denoise_dbfs,agc_gain_db,processed_dbfs,gate,threshold,agc,denoise,suppression,volume,queue_age_ms,engine,speech,lsnr_db,echo,high_pass,gate_enabled,headphones,voice_detection";

fn stale_dsp_row() -> String {
    let mut fields = vec![""; MIC_DSP_HEADER.split(',').count()];
    fields[..6].copy_from_slice(&["-120", "-120", "-120", "0", "-120", "Stale"]);
    fields.join(",")
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

/// The name is the tag: livekit 0.7's `TrackSource` has no soundboard value,
/// and a listener has to tell this apart from a microphone to give it its own volume.
const SOUNDBOARD_TRACK: &str = "soundboard";
/// A spammed button stacks sounds; past this many the oldest is dropped.
const MAX_OVERLAPPING_SOUNDS: usize = 4;
/// Past this the loop was suspended, not late; replaying the gap would flood the SFU.
const SOUNDBOARD_MAX_CATCH_UP: std::time::Duration = std::time::Duration::from_millis(200);

struct SoundboardTrack {
    _sid: livekit::prelude::TrackSid,
    tx: UnboundedSender<Arc<[f32]>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for SoundboardTrack {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Silence goes out between sounds so the track's RTP clock keeps running
/// (trap 38); DTX keeps that cheap. The player hears the same mix locally:
/// nobody is sent their own track.
async fn soundboard_loop(
    mut rx: UnboundedReceiver<Arc<[f32]>>,
    source: NativeAudioSource,
    mixer: PlaybackHandle,
    self_identity: String,
) {
    let local = mixer.add_local_soundboard(self_identity);
    let mut rate = mixer.rate();
    let mut cap = (rate / PLAYBACK_CAP_DIVISOR) as usize;
    let mut resampler = AudioResampler::new(SAMPLE_RATE, rate);
    let mut resampled: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
    let mut playing: Vec<(Arc<[f32]>, usize)> = Vec::new();
    let mut was_playing = false;
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(FRAME_MS as u64));
    // Frames are the track's RTP clock: a late tick must be made up, or the
    // track falls behind real time and listeners' NetEq stretches it (trap 38).
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    loop {
        loop {
            match rx.try_recv() {
                Ok(pcm) => playing.push((pcm, 0)),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    mixer.remove_track(local);
                    return;
                }
            }
        }
        if playing.len() > MAX_OVERLAPPING_SOUNDS {
            playing.drain(..playing.len() - MAX_OVERLAPPING_SOUNDS);
        }
        let due = tick.tick().await;
        if due.elapsed() > SOUNDBOARD_MAX_CATCH_UP {
            tick.reset();
        }
        let playing_this_frame = !playing.is_empty();
        let mut frame = [0.0f32; FRAME_SAMPLES];
        for (pcm, pos) in playing.iter_mut() {
            let end = (*pos + FRAME_SAMPLES).min(pcm.len());
            for (out, s) in frame.iter_mut().zip(&pcm[*pos..end]) {
                *out += s;
            }
            *pos = end;
        }
        playing.retain(|(pcm, pos)| *pos < pcm.len());
        let data = frame.map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
        if let Err(e) = source.capture_frame(&pcm_frame(&data)).await {
            eprintln!("[voice] soundboard capture_frame error: {e:?}");
        }
        if !playing_this_frame && !was_playing {
            continue;
        }
        was_playing = playing_this_frame;
        let current = mixer.rate();
        if current != rate {
            rate = current;
            resampler = AudioResampler::new(SAMPLE_RATE, rate);
            cap = (rate / PLAYBACK_CAP_DIVISOR) as usize;
        }
        match resampler.as_mut() {
            Some(r) => {
                r.process_into(&frame, &mut resampled);
                mixer.push(local, &resampled, rate, cap);
            }
            None => {
                mixer.push(local, &frame, rate, cap);
            }
        }
    }
}

/// The direct source takes exactly one DSP block per call; anything else is refused.
fn pcm_frame(samples: &[i16; FRAME_SAMPLES]) -> AudioFrame<'_> {
    AudioFrame {
        data: std::borrow::Cow::Borrowed(samples),
        sample_rate: SAMPLE_RATE,
        num_channels: CHANNELS,
        samples_per_channel: FRAME_SAMPLES as u32,
    }
}

async fn publish_pcm(mut rx: crate::audio_queue::AudioReceiver<f32>, source: NativeAudioSource) {
    while let Some(packet) = rx.recv().await {
        if !packet.is_fresh() {
            continue;
        }
        let data = packet
            .samples
            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
        if let Err(e) = source.capture_frame(&pcm_frame(&data)).await {
            eprintln!("[voice] system audio capture_frame error: {e:?}");
        }
    }
}

fn peak_fixed(samples: &[f32]) -> i32 {
    (samples.iter().fold(0.0f32, |m, s| m.max(s.abs())) * 1_000.0) as i32
}

async fn publish_loop(mut rx: crate::audio_queue::AudioReceiver<i16>, source: NativeAudioSource) {
    let mut sent = 0u64;
    let mut trace = crate::voice_record::MicrophoneTrace::new("mic-publication", "mic-accepted");
    while let Some(packet) = rx.recv().await {
        if !packet.is_fresh() {
            trace.record(packet.position, packet.captured, "stale_publish", None);
            continue;
        }
        if let Err(e) = source.capture_frame(&pcm_frame(&packet.samples)).await {
            eprintln!("[voice] capture_frame error: {e:?}");
            trace.record(packet.position, packet.captured, "source_error", None);
            continue;
        }
        trace.record(
            packet.position,
            packet.captured,
            "accepted",
            Some(&packet.samples),
        );
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

    fn denoise_fallback(&self) -> Option<crate::denoise::Fallback> {
        match self.denoise_fallback.load(Ordering::Relaxed) {
            2 => Some(crate::denoise::Fallback::Off),
            _ => None,
        }
    }
}

fn spawn_meter_task(mut state: Signal<AppState>, meter: Arc<MicMeter>) -> Task {
    dioxus::prelude::spawn(async move {
        let mut last_speaking = false;
        let mut last_level = u32::MAX;
        let mut last_level_pre = u32::MAX;
        let mut last_gate_level = u32::MAX;
        let mut last_fallback = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let level = meter.peak.swap(0, Ordering::Relaxed).clamp(0, 1000) as u32;
            let level_pre = meter.peak_pre.swap(0, Ordering::Relaxed).clamp(0, 1000) as u32;
            let speaking = meter.open.load(Ordering::Relaxed);
            let gate_level = meter.gate_peak.swap(0, Ordering::Relaxed).clamp(0, 1000) as u32;
            if gate_level != last_gate_level {
                last_gate_level = gate_level;
                state.write().mic_gate_level = gate_level;
            }
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
            let fallback = meter.denoise_fallback();
            if fallback != last_fallback {
                last_fallback = fallback;
                state.write().denoise_fallback = fallback;
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
        recent_loss_pct: None,
        discarded_packets: None,
        concealed_pct: None,
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

// Prefer the default handle to preserve route following and the historical CoreAudio workaround.
pub(crate) fn pick_device(
    selected: Option<&str>,
    default: Option<cpal::Device>,
    all: Option<impl Iterator<Item = cpal::Device>>,
) -> Option<cpal::Device> {
    let Some(wanted) = selected else {
        return default;
    };
    let default_name = default
        .as_ref()
        .and_then(|d| crate::audio_device::name(d).ok());
    if default_name.as_deref() == Some(wanted) {
        return default;
    }
    all.and_then(|devs| {
        devs.into_iter().find(|d| {
            crate::audio_device::name(d)
                .map(|n| n == wanted)
                .unwrap_or(false)
        })
    })
    .or(default)
}

/// A device that went away mid-call. The voice session reopens it with backoff.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AudioFault {
    Input,
    Output,
}

/// Reopening faster than this just spins on a device that is still gone; a
/// re-plug is caught by the next tick anyway.
const DEVICE_RETRY: std::time::Duration = std::time::Duration::from_secs(2);

/// The device lists are a full cpal enumeration; the fault channel already
/// reports a device that errors, so watching the list can be slower than the tick.
const DEVICE_SCAN: std::time::Duration = std::time::Duration::from_secs(5);

struct MicCapture {
    _backend: MicBackend,
    muted: Arc<AtomicBool>,
    heartbeat: tokio::task::JoinHandle<()>,
    /// The device actually opened, which may be the default rather than
    /// `selected_input_device`; `maintain` watches this name for disappearance.
    device_name: Option<String>,
}

enum MicBackend {
    Cpal {
        stream: cpal::Stream,
    },
    #[cfg(target_os = "windows")]
    Raw {
        _capture: crate::rawmic::Capture,
    },
}

fn publish_audio_devices(
    mut state: Signal<AppState>,
    mic: Option<&MicCapture>,
    playback: &PlaybackMixer,
) {
    let input = mic.and_then(|mic| match &mic._backend {
        #[cfg(target_os = "windows")]
        MicBackend::Raw { _capture } => Some(_capture.device_name.clone()),
        _ => mic.device_name.clone(),
    });
    let output = playback.device_name().map(str::to_owned);
    if state.peek().active_input_device != input || state.peek().active_output_device != output {
        let mut current = state.write();
        current.active_input_device = input;
        current.active_output_device = output;
    }
}

impl MicCapture {
    fn start(
        frame_tx: crate::audio_queue::AudioSender<f32>,
        mut state: Signal<AppState>,
        muted: Arc<AtomicBool>,
        gate_stats: Arc<GateStats>,
        faults: UnboundedSender<AudioFault>,
    ) -> Result<Self, String> {
        #[cfg(target_os = "android")]
        crate::android::check_microphone_permission()?;
        let raw_peak = Arc::new(AtomicI32::new(0));
        let frames_pushed = Arc::new(AtomicU64::new(0));
        let selected = state.read().selected_input_device.clone();
        state.write().mic_bypass_error = None;
        state.write().active_input_device = None;

        let (backend, device_name) = match Self::maybe_raw(
            &frame_tx,
            state,
            &selected,
            &raw_peak,
            &frames_pushed,
            &faults,
        ) {
            // The raw path reports its own device loss; only cpal resolves a
            // name `maintain` can watch.
            Some(raw) => (raw, None),
            None => {
                let (backend, name) =
                    Self::start_cpal(&frame_tx, selected, &raw_peak, &frames_pushed, &faults)?;
                (backend, Some(name))
            }
        };

        if let Some(name) = &device_name {
            state.write().active_input_device = Some(name.clone());
        }
        let heartbeat = Self::spawn_heartbeat(raw_peak, frames_pushed, gate_stats);
        Ok(Self {
            _backend: backend,
            muted,
            heartbeat,
            device_name,
        })
    }

    fn device_name(&self) -> Option<&str> {
        self.device_name.as_deref()
    }

    #[cfg(target_os = "windows")]
    fn maybe_raw(
        frame_tx: &crate::audio_queue::AudioSender<f32>,
        mut state: Signal<AppState>,
        selected: &Option<String>,
        raw_peak: &Arc<AtomicI32>,
        frames_pushed: &Arc<AtomicU64>,
        faults: &UnboundedSender<AudioFault>,
    ) -> Option<MicBackend> {
        if !state.read().bypass_system_audio_processing {
            return None;
        }

        let (fatal_tx, mut fatal_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::rawmic::CaptureFault>();
        // A raw device can only be rebuilt from the session loop, so the death
        // is reported upwards instead of retried here.
        let faults = faults.clone();
        dioxus::prelude::spawn(async move {
            if let Some(e) = fatal_rx.recv().await {
                match e {
                    crate::rawmic::CaptureFault::Device(error) => {
                        eprintln!("[voice] raw mic died mid-call: {error}");
                        state.write().mic_bypass_error = Some(error);
                    }
                    crate::rawmic::CaptureFault::DefaultChanged => {
                        tracing::info!("Default microphone changed; reopening capture");
                    }
                }
                let _ = faults.send(AudioFault::Input);
            }
        });

        let (frame_tx, raw_peak, frames_pushed) =
            (frame_tx.clone(), raw_peak.clone(), frames_pushed.clone());
        let sink: crate::rawmic::SinkBuilder = Box::new(move |rate: u32, channels: u32| {
            let accum = Arc::new(Mutex::new(MicBuffer::new()));
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
            Ok(capture) => {
                state.write().active_input_device = Some(capture.device_name.clone());
                Some(MicBackend::Raw { _capture: capture })
            }
            Err(e) => {
                eprintln!("[voice] mic: raw capture unavailable ({e}); using the ordinary path");
                state.write().mic_bypass_error = Some(e);
                None
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn maybe_raw(
        _frame_tx: &crate::audio_queue::AudioSender<f32>,
        _state: Signal<AppState>,
        _selected: &Option<String>,
        _raw_peak: &Arc<AtomicI32>,
        _frames_pushed: &Arc<AtomicU64>,
        _faults: &UnboundedSender<AudioFault>,
    ) -> Option<MicBackend> {
        None
    }

    fn start_cpal(
        frame_tx: &crate::audio_queue::AudioSender<f32>,
        selected: Option<String>,
        raw_peak: &Arc<AtomicI32>,
        frames_pushed: &Arc<AtomicU64>,
        faults: &UnboundedSender<AudioFault>,
    ) -> Result<(MicBackend, String), String> {
        let frame_tx = frame_tx.clone();
        let host = cpal::default_host();
        let device = pick_device(
            selected.as_deref(),
            host.default_input_device(),
            host.input_devices().ok(),
        )
        .ok_or_else(|| "no default input device".to_string())?;
        let device_name = crate::audio_device::name(&device).unwrap_or_else(|_| "<unknown>".into());
        let config = crate::audio_device::config(
            &device,
            true,
            &[
                cpal::SampleFormat::F32,
                cpal::SampleFormat::I16,
                cpal::SampleFormat::U16,
            ],
        )
        .map_err(|e| format!("input config: {e}"))?;
        let sample_format = config.sample_format();
        let device_rate = config.sample_rate();
        let device_channels = config.channels() as u32;
        eprintln!(
            "[voice] mic: device={device_name} format={sample_format:?} rate={device_rate} ch={device_channels}"
        );

        let raw_peak_cb = raw_peak.clone();
        let frames_pushed_cb = frames_pushed.clone();

        let accum = Arc::new(Mutex::new(MicBuffer::new()));

        let resampler = Arc::new(Mutex::new(AudioResampler::new(device_rate, SAMPLE_RATE)));
        if resampler.lock().is_some() {
            eprintln!("[voice] mic: resampling {device_rate}Hz → {SAMPLE_RATE}Hz via rubato");
        }

        let report_fault = |faults: &UnboundedSender<AudioFault>| {
            let faults = faults.clone();
            move |e: cpal::Error| {
                eprintln!("mic stream error: {e}");
                if crate::audio_device::needs_reopen(e.kind()) {
                    let _ = faults.send(AudioFault::Input);
                }
            }
        };
        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let accum = accum.clone();
                let frame_tx = frame_tx.clone();
                let resampler_cb = resampler.clone();
                let mut mono_buf: Vec<f32> = Vec::with_capacity(1024);
                let mut resampled_buf: Vec<f32> = Vec::with_capacity(1024);
                device.build_input_stream(
                    config.into(),
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
                    report_fault(faults),
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
                    config.into(),
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
                    report_fault(faults),
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
                    config.into(),
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
                    report_fault(faults),
                    None,
                )
            }
            other => return Err(format!("unsupported sample format: {other:?}")),
        }
        .map_err(|e| format!("build_input_stream: {e}"))?;

        stream.play().map_err(|e| format!("play mic: {e}"))?;
        Ok((MicBackend::Cpal { stream }, device_name))
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
                    "[voice] mic heartbeat: raw peak={p:.4} ({level}), frames queued for processing={f} (+{})",
                    f - prev_frames
                );
                let after = gate_stats.peak_after.swap(0, Ordering::Relaxed) as f32 / 1_000.0;
                let passed = gate_stats.passed.swap(0, Ordering::Relaxed);
                let dropped = gate_stats.dropped.swap(0, Ordering::Relaxed);
                let speaking = gate_stats.speaking.swap(0, Ordering::Relaxed);
                crate::dlog!(
                    "mic 2s raw={p:.4} after={after:.4} thr={:.4} passed={passed} dropped={dropped} speech={}%{}",
                    gate_stats.threshold.load(Ordering::Relaxed) as f32 / 1_000.0,
                    speaking * 100 / (passed + dropped).max(1),
                    match (
                        gate_stats.engine.load(Ordering::Relaxed),
                        gate_stats.atten_lim_applied.load(Ordering::Relaxed),
                    ) {
                        (2, db) => format!(" nc=RNNoise atten={db}dB"),
                        _ => String::new(),
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
        #[allow(irrefutable_let_patterns)]
        if let MicBackend::Cpal { stream } = &self._backend {
            match stream.pause() {
                Ok(()) => eprintln!("[voice] mic stream stopped"),
                Err(e) => eprintln!("[voice] mic stream would not stop: {e}"),
            }
        }
    }
}

struct MicBuffer {
    samples: Vec<f32>,
    id: u64,
    next_sample: u64,
    recorder: crate::voice_record::CaptureRecorder,
}

impl MicBuffer {
    fn new() -> Self {
        static NEXT_CAPTURE: AtomicU64 = AtomicU64::new(1);
        Self {
            samples: Vec::with_capacity(FRAME_SAMPLES * 4),
            id: NEXT_CAPTURE.fetch_add(1, Ordering::Relaxed),
            next_sample: 0,
            recorder: crate::voice_record::CaptureRecorder::new(),
        }
    }
}

fn forward_mic(
    frame_tx: &crate::audio_queue::AudioSender<f32>,
    samples: &[f32],
    device_channels: u32,
    accum: &Arc<Mutex<MicBuffer>>,
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

    let mut capture = accum.lock();
    capture.samples.extend_from_slice(resampled_buf);

    let mut pushed = 0usize;
    while capture.samples.len() >= FRAME_SAMPLES {
        let mut samples = [0.0; FRAME_SAMPLES];
        samples.copy_from_slice(&capture.samples[..FRAME_SAMPLES]);
        capture.samples.drain(..FRAME_SAMPLES);
        let mut packet = crate::audio_queue::Frame::new(samples);
        packet.position = Some(crate::audio_queue::CapturePosition {
            capture_id: capture.id,
            first_sample: capture.next_sample,
        });
        capture.next_sample += FRAME_SAMPLES as u64;
        let (position, captured) = (packet.position, packet.captured);
        let outcome = match crate::audio_queue::offer(frame_tx, packet) {
            Ok(true) => {
                pushed += 1;
                "queued"
            }
            Ok(false) => "capture_full",
            Err(()) => "capture_closed",
        };
        capture
            .recorder
            .record(samples, position, captured, outcome);
        if outcome == "capture_closed" {
            break;
        }
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

pub(crate) struct AudioResampler {
    inner: Box<dyn Resampler<f32>>,
    input_accum: Vec<f32>,
    chunk_in: Vec<f32>,
    scratch_out: Vec<f32>,
}

/// Room for `drift::Control`'s correction either way.
const ADAPTIVE_RATIO_RANGE: f64 = 1.02;

#[cfg(test)]
#[path = "resampler_tests.rs"]
mod resampler_tests;

impl AudioResampler {
    pub(crate) fn new(from_rate: u32, to_rate: u32) -> Option<Self> {
        if from_rate == to_rate {
            return None;
        }
        let inner = Fft::<f32>::new_custom(
            from_rate as usize,
            to_rate as usize,
            RESAMPLER_CHUNK,
            2,
            1,
            WindowFunction::BlackmanHarris2,
            FixedSync::Input,
        )
        .map_err(|e| eprintln!("[voice] rubato resampler init failed: {e:?}"))
        .ok()?;
        Some(Self::wrap(Box::new(inner)))
    }

    /// Built even when the rates match: its ratio is what corrects the drift
    /// between the sender's playout clock and this device.
    pub(crate) fn adaptive(from_rate: u32, to_rate: u32) -> Option<Self> {
        let inner = Async::<f32>::new_sinc(
            f64::from(to_rate) / f64::from(from_rate),
            ADAPTIVE_RATIO_RANGE,
            &SincInterpolationParameters {
                sinc_len: 64,
                f_cutoff: None,
                oversampling_factor: 128,
                interpolation: SincInterpolationType::Linear,
                window: WindowFunction::BlackmanHarris2,
            },
            FRAME_SAMPLES,
            1,
            FixedAsync::Input,
        )
        .map_err(|e| eprintln!("[voice] rubato adaptive resampler init failed: {e:?}"))
        .ok()?;
        Some(Self::wrap(Box::new(inner)))
    }

    fn wrap(inner: Box<dyn Resampler<f32>>) -> Self {
        let max_in = inner.input_frames_max();
        let max_out = inner.output_frames_max();
        Self {
            inner,
            input_accum: Vec::with_capacity(max_in * 2),
            chunk_in: Vec::with_capacity(max_in),
            scratch_out: vec![0.0; max_out],
        }
    }

    /// Ramped over the next chunk; a fixed resampler ignores it.
    pub(crate) fn set_relative_ratio(&mut self, ratio: f64) {
        if let Some(adjustable) = self.inner.as_adjustable() {
            let _ = adjustable.set_resample_ratio_relative(ratio, true);
        }
    }

    pub(crate) fn process_into(&mut self, input: &[f32], out: &mut Vec<f32>) {
        out.clear();
        self.input_accum.extend_from_slice(input);

        let Self {
            inner,
            input_accum,
            chunk_in,
            scratch_out,
        } = self;

        while input_accum.len() >= inner.input_frames_next() {
            let needed = inner.input_frames_next();
            chunk_in.clear();
            chunk_in.extend_from_slice(&input_accum[..needed]);
            let need = inner.output_frames_next();
            if scratch_out.len() < need {
                scratch_out.resize(need, 0.0);
            }
            let waves_in = SequentialSlice::new(&chunk_in[..], 1, needed)
                .expect("mono input holds the required frames");
            let mut waves_out = SequentialSlice::new_mut(&mut scratch_out[..], 1, need)
                .expect("mono output holds the required frames");
            match inner.process_into_buffer(&waves_in, &mut waves_out, None) {
                Ok((consumed, produced)) => {
                    if consumed == 0 {
                        break;
                    }
                    input_accum.drain(..consumed);
                    out.extend_from_slice(&scratch_out[..produced]);
                }
                Err(e) => {
                    input_accum.drain(..needed);
                    eprintln!("[voice] rubato process error: {e:?}");
                }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrackKind {
    Voice,
    Stream,
    Soundboard,
}

impl TrackKind {
    fn of(source: TrackSource, name: &str) -> Self {
        if name == SOUNDBOARD_TRACK {
            TrackKind::Soundboard
        } else if source == TrackSource::ScreenshareAudio {
            TrackKind::Stream
        } else {
            TrackKind::Voice
        }
    }
}

struct TrackBuf {
    samples: std::collections::VecDeque<f32>,
    identity: String,
    gain: f32,
    kind: TrackKind,
    starved: u64,
    drift_correction: bool,
    recovery: playback_recovery::Recovery,
    source_rate: u32,
}

#[derive(Clone)]
struct PlaybackHandle {
    tracks: Arc<Mutex<MixerTracks>>,
    /// Shared and mutable: a live output reopen can change the device's native
    /// rate while track consumers are already running.
    device_rate: Arc<AtomicU32>,
    gains: Arc<Mutex<HashMap<String, f32>>>,
    stream_gains: Arc<Mutex<HashMap<String, f32>>>,
    soundboard_pct: Arc<AtomicU32>,
}

/// Someone you muted for yourself stays muted when they press a sound.
fn track_gain(
    kind: TrackKind,
    identity: &str,
    gains: &HashMap<String, f32>,
    stream_gains: &HashMap<String, f32>,
    soundboard_pct: u32,
) -> f32 {
    let voice = gains.get(identity).copied().unwrap_or(1.0);
    match kind {
        TrackKind::Voice => voice,
        TrackKind::Stream => stream_gains
            .get(crate::stream_audio::identity(identity))
            .copied()
            .unwrap_or(0.0),
        TrackKind::Soundboard if voice == 0.0 => 0.0,
        TrackKind::Soundboard => soundboard_pct.min(100) as f32 / 100.0,
    }
}

impl PlaybackHandle {
    fn rate(&self) -> u32 {
        self.device_rate.load(Ordering::Relaxed)
    }

    fn set_rate(&self, rate: u32) {
        let mut t = self.tracks.lock();
        if self.rate() == rate {
            return;
        }
        for track in t.buffers.values_mut() {
            track.samples.clear();
            track.source_rate = rate;
            track.recovery.set_rate(rate);
        }
        self.device_rate.store(rate, Ordering::Relaxed);
    }

    fn add_track(&self, identity: String, kind: TrackKind) -> u64 {
        self.insert_track(identity, kind, true)
    }

    /// Pushed only while a sound plays, so an empty queue is a pause, not a stall.
    fn add_local_soundboard(&self, identity: String) -> u64 {
        self.insert_track(identity, TrackKind::Soundboard, false)
    }

    fn add_buffered_track(&self, identity: String) -> u64 {
        self.insert_track(identity, TrackKind::Voice, false)
    }

    fn insert_track(&self, identity: String, kind: TrackKind, drift_correction: bool) -> u64 {
        let gain = track_gain(
            kind,
            &identity,
            &self.gains.lock(),
            &self.stream_gains.lock(),
            self.soundboard_pct.load(Ordering::Relaxed),
        );
        crate::dlog!("mixer add_track identity={identity} kind={kind:?} initial_gain={gain:.2}");
        let mut t = self.tracks.lock();
        let id = t.next_id;
        t.next_id = t.next_id.wrapping_add(1);
        t.buffers.insert(
            id,
            TrackBuf {
                samples: std::collections::VecDeque::with_capacity(SAMPLE_RATE as usize / 2),
                identity,
                gain,
                kind,
                starved: 0,
                drift_correction,
                recovery: playback_recovery::Recovery::new(self.rate()),
                source_rate: self.rate(),
            },
        );
        id
    }

    fn queued_samples(&self, id: u64) -> Option<usize> {
        self.tracks
            .lock()
            .buffers
            .get(&id)
            .map(|track| track.samples.len())
    }

    /// Queue depth and counters are sampled under the same lock as the appended audio.
    fn push(
        &self,
        id: u64,
        samples: &[f32],
        rate: u32,
        cap: usize,
    ) -> Option<(usize, u64, f32, u64)> {
        let mut t = self.tracks.lock();
        let current = self.rate();
        if rate != current {
            return None;
        }
        let track = t.buffers.get_mut(&id)?;
        if track.source_rate != current {
            track.samples.clear();
            track.source_rate = current;
        }
        track.recovery.set_rate(current);
        track.samples.extend(samples);
        drift::bound(&mut track.samples, cap, current);
        Some((
            track.samples.len(),
            track.starved,
            track.gain,
            track.recovery.held_samples,
        ))
    }

    fn remove_track(&self, id: u64) {
        let removed = self.tracks.lock().buffers.remove(&id);
        if let Some(t) = removed {
            crate::dlog!(
                "mixer remove_track identity={} kind={:?}",
                t.identity,
                t.kind
            );
        }
    }
}

fn refresh_gains(
    tracks: &mut MixerTracks,
    gains: &Arc<Mutex<HashMap<String, f32>>>,
    stream_gains: &Arc<Mutex<HashMap<String, f32>>>,
    soundboard_pct: &Arc<AtomicU32>,
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
    let board = soundboard_pct.load(Ordering::Relaxed);
    for track in tracks.buffers.values_mut() {
        track.gain = track_gain(track.kind, &track.identity, &g, &sg, board);
    }
}

fn next_playback_sample(tracks: &mut MixerTracks) -> f32 {
    let mut total = 0.0;
    for track in tracks.buffers.values_mut() {
        if track.samples.is_empty() {
            track.starved += 1;
        }
        if track.drift_correction && !track.recovery.ready(track.samples.len()) {
            continue;
        }
        let sample = track.samples.pop_front().unwrap_or(0.0);
        let resume = if track.drift_correction {
            track.recovery.resume_gain()
        } else {
            1.0
        };
        total += track.gain * resume * sample;
    }
    total
}

struct PlaybackMixer {
    stream: Option<cpal::Stream>,
    handle: PlaybackHandle,
    reference_tx: crate::audio_queue::AudioSender<i16>,
    callbacks: Arc<AtomicU64>,
    pulled: Arc<AtomicU64>,
    /// The device actually opened; `maintain` watches this name, not the
    /// selection, since `pick_device` falls back to the default.
    device_name: Option<String>,
}

impl Drop for PlaybackMixer {
    fn drop(&mut self) {
        self.drop_stream();
    }
}

impl PlaybackMixer {
    /// Never fails: a machine with no output device still joins, and the stream
    /// is opened the moment one appears.
    fn start(
        mut state: Signal<AppState>,
        controls: AudioControls,
        reference_tx: crate::audio_queue::AudioSender<i16>,
        faults: UnboundedSender<AudioFault>,
    ) -> Self {
        let handle = PlaybackHandle {
            tracks: Arc::new(Mutex::new(MixerTracks::default())),
            device_rate: Arc::new(AtomicU32::new(SAMPLE_RATE)),
            gains: controls.gains.clone(),
            stream_gains: controls.stream_gains.clone(),
            soundboard_pct: controls.soundboard_pct.clone(),
        };
        let mut mixer = Self {
            stream: None,
            handle,
            reference_tx,
            callbacks: Arc::new(AtomicU64::new(0)),
            pulled: Arc::new(AtomicU64::new(0)),
            device_name: None,
        };
        mixer.spawn_heartbeat();
        if let Err(e) = mixer.try_open(&mut state, &controls, &faults) {
            eprintln!("[voice] playback unavailable: {e}");
        }
        mixer
    }

    fn spawn_heartbeat(&self) {
        let cb_for_log = Arc::downgrade(&self.callbacks);
        let pulled_for_log = Arc::downgrade(&self.pulled);
        tokio::spawn(async move {
            let mut prev_cb = 0u64;
            let mut prev_pulled = 0u64;
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let (Some(callbacks), Some(pulled)) =
                    (cb_for_log.upgrade(), pulled_for_log.upgrade())
                else {
                    break;
                };
                let cb = callbacks.load(std::sync::atomic::Ordering::Relaxed);
                let pulled_written = pulled.load(std::sync::atomic::Ordering::Relaxed);
                eprintln!(
                    "[voice] playback heartbeat: callbacks={} (+{}), non-silent samples written={} (+{})",
                    cb,
                    cb - prev_cb,
                    pulled_written,
                    pulled_written - prev_pulled,
                );
                prev_cb = cb;
                prev_pulled = pulled_written;
            }
        });
    }

    fn try_open(
        &mut self,
        state: &mut Signal<AppState>,
        controls: &AudioControls,
        faults: &UnboundedSender<AudioFault>,
    ) -> Result<(), String> {
        let host = cpal::default_host();
        state.write().active_output_device = None;
        let selected = state.read().selected_output_device.clone();
        let device = pick_device(
            selected.as_deref(),
            host.default_output_device(),
            host.output_devices().ok(),
        )
        .ok_or_else(|| "no default output device".to_string())?;
        let device_name = crate::audio_device::name(&device).unwrap_or_else(|_| "<unknown>".into());
        let config = crate::audio_device::config(
            &device,
            false,
            &[cpal::SampleFormat::F32, cpal::SampleFormat::I16],
        )
        .map_err(|e| format!("output config: {e}"))?;
        let device_channels = config.channels() as usize;
        let sample_format = config.sample_format();
        let device_rate = config.sample_rate();
        eprintln!(
            "[voice] playback: device={device_name} format={sample_format:?} rate={device_rate} ch={device_channels}"
        );

        let tracks_cb = self.handle.tracks.clone();
        let cb_counter_cb = self.callbacks.clone();
        let pulled_cb = self.pulled.clone();
        let gains_f32 = controls.gains.clone();
        let gains_i16 = controls.gains.clone();
        let stream_gains_f32 = controls.stream_gains.clone();
        let stream_gains_i16 = controls.stream_gains.clone();
        let board_f32 = controls.soundboard_pct.clone();
        let board_i16 = controls.soundboard_pct.clone();
        let deafened_f32 = controls.deafened.clone();
        let deafened_i16 = controls.deafened.clone();
        let mut dither_rng = DITHER_SEED;
        let mut reference_f32 = crate::echo::Reference::new(self.reference_tx.clone(), device_rate);
        let mut reference_i16 = crate::echo::Reference::new(self.reference_tx.clone(), device_rate);
        let mut limiter = crate::playback_limiter::Limiter::new(device_rate);
        let mut recording = crate::voice_record::Playback::new(device_rate);

        let report_fault = |faults: &UnboundedSender<AudioFault>| {
            let faults = faults.clone();
            move |e: cpal::Error| {
                eprintln!("output stream error: {e}");
                if crate::audio_device::needs_reopen(e.kind()) {
                    let _ = faults.send(AudioFault::Output);
                }
            }
        };
        let stream = match sample_format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                config.into(),
                move |data: &mut [f32], _| {
                    let started = Instant::now();
                    cb_counter_cb.fetch_add(1, Ordering::Relaxed);
                    let mut tracks = tracks_cb.lock();
                    recording.begin(
                        started,
                        data.len() / device_channels,
                        started.elapsed(),
                        tracks
                            .buffers
                            .values()
                            .map(|t| t.samples.len())
                            .min()
                            .unwrap_or(0),
                        tracks.buffers.values().map(|t| t.starved).sum(),
                    );
                    refresh_gains(
                        &mut tracks,
                        &gains_f32,
                        &stream_gains_f32,
                        &board_f32,
                        &deafened_f32,
                    );
                    let mut pulled = 0u64;
                    for frame in data.chunks_mut(device_channels) {
                        let raw = next_playback_sample(&mut tracks);
                        let sample = limiter.process(raw);
                        recording.push(raw, sample, limiter.gain());
                        reference_f32.push(sample);
                        if sample != 0.0 {
                            pulled += 1;
                        }
                        for s in frame.iter_mut() {
                            *s = sample;
                        }
                    }
                    recording.finish();
                    pulled_cb.fetch_add(pulled, Ordering::Relaxed);
                },
                report_fault(faults),
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                config.into(),
                move |data: &mut [i16], _| {
                    let started = Instant::now();
                    cb_counter_cb.fetch_add(1, Ordering::Relaxed);
                    let mut tracks = tracks_cb.lock();
                    recording.begin(
                        started,
                        data.len() / device_channels,
                        started.elapsed(),
                        tracks
                            .buffers
                            .values()
                            .map(|t| t.samples.len())
                            .min()
                            .unwrap_or(0),
                        tracks.buffers.values().map(|t| t.starved).sum(),
                    );
                    refresh_gains(
                        &mut tracks,
                        &gains_i16,
                        &stream_gains_i16,
                        &board_i16,
                        &deafened_i16,
                    );
                    let mut pulled = 0u64;
                    for frame in data.chunks_mut(device_channels) {
                        let raw = next_playback_sample(&mut tracks);
                        let sample = limiter.process(raw);
                        let s16 = dither_to_i16(sample, &mut dither_rng);
                        recording.push(raw, s16 as f32 / i16::MAX as f32, limiter.gain());
                        if sample != 0.0 {
                            pulled += 1;
                        }
                        reference_i16.push(s16 as f32 / i16::MAX as f32);
                        for s in frame.iter_mut() {
                            *s = s16;
                        }
                    }
                    recording.finish();
                    pulled_cb.fetch_add(pulled, Ordering::Relaxed);
                },
                report_fault(faults),
                None,
            ),
            other => return Err(format!("unsupported output format: {other:?}")),
        }
        .map_err(|e| format!("build_output_stream: {e}"))?;

        // After play(), not before: set_rate drops queued PCM and repoints the
        // rate producers read, which a failed play() would do for nothing.
        stream.play().map_err(|e| format!("play output: {e}"))?;
        self.handle.set_rate(device_rate);

        if device_rate != SAMPLE_RATE {
            eprintln!("[voice] playback: resampling {SAMPLE_RATE}Hz → {device_rate}Hz via rubato");
        }
        state.write().active_output_device = Some(device_name.clone());
        self.device_name = Some(device_name);
        self.stream = Some(stream);
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.stream.is_some()
    }

    fn device_name(&self) -> Option<&str> {
        self.device_name.as_deref()
    }

    fn drop_stream(&mut self) {
        self.device_name = None;
        if let Some(stream) = self.stream.take() {
            match stream.pause() {
                Ok(()) => eprintln!("[voice] playback stream stopped"),
                Err(e) => eprintln!("[voice] playback stream would not stop: {e}"),
            }
        }
    }

    fn handle(&self) -> PlaybackHandle {
        self.handle.clone()
    }
}

async fn consume_remote_track(
    mut stream: NativeAudioStream,
    handle: PlaybackHandle,
    identity: String,
    kind: TrackKind,
) {
    let mut frames = 0u64;
    let mut sample_count = 0u64;
    let mut peak_recent: i16 = 0;
    let mut rate = handle.rate();
    let mut resampler = remote_resampler(rate);
    let mut drift = drift::Control::default();
    let mut ratio = 1.0;
    let mut f32_buf: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
    let mut resampled_buf: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
    let who = match identity.split_once('#') {
        Some((pk, suffix)) => format!("{}#{suffix}", crate::identity::truncate_pubkey(pk)),
        None => crate::identity::truncate_pubkey(&identity),
    };
    let track_id = handle.add_track(identity, kind);
    let _cleanup = RemotePlaybackGuard(handle.clone(), track_id);
    let mut cap = (rate / PLAYBACK_CAP_DIVISOR) as usize;
    let record_name = format!("recv-{who}-{kind:?}");
    let mut recv_wav = crate::voice_record::Wav::new(&record_name, SAMPLE_RATE);
    let mut recv_csv = crate::voice_record::Csv::new(
        &record_name,
        "samples,rate,rms_dbfs,peak,gap_ms,mixer_ms,mixer_starved_samples,gain,rebuffered_samples,ratio",
    );
    let mut arrived: Option<Instant> = None;
    let mut record_generation = 0;
    while let Some(frame) = stream.next().await {
        // The output stream can be reopened at a different native rate mid-call.
        let current = handle.rate();
        if current != rate {
            rate = current;
            resampler = remote_resampler(rate);
            drift = drift::Control::default();
            cap = (rate / PLAYBACK_CAP_DIVISOR) as usize;
        }
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
        f32_buf.clear();
        f32_buf.extend(frame.data.iter().map(|s| *s as f32 / i16::MAX as f32));
        let mixer = match resampler.as_mut() {
            Some(r) => {
                r.process_into(&f32_buf, &mut resampled_buf);
                handle.push(track_id, &resampled_buf, rate, cap)
            }
            None => handle.push(track_id, &f32_buf, rate, cap),
        };
        if let (Some(r), Some((depth, ..))) = (resampler.as_mut(), mixer) {
            ratio = drift.observe(depth as f64 * 1000.0 / f64::from(rate.max(1)));
            r.set_relative_ratio(ratio);
        }
        let generation = crate::voice_record::generation();
        if generation != record_generation {
            arrived = None;
            record_generation = generation;
        }
        if recv_csv.is_on() {
            recv_wav.write_i16(&frame.data);
            let now = Instant::now();
            let gap = arrived.map_or(0, |at| now.duration_since(at).as_millis());
            arrived = Some(now);
            let (depth, starved, gain, rebuffered) = mixer.unwrap_or_default();
            recv_csv.row(|| {
                format!(
                    "{},{},{:.1},{frame_peak},{gap},{},{starved},{gain:.2},{rebuffered},{ratio:.5}",
                    frame.data.len(),
                    frame.sample_rate,
                    crate::voice_record::rms_dbfs(&f32_buf),
                    depth as u64 * 1000 / u64::from(rate.max(1)),
                )
            });
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
    eprintln!("[voice] remote-track {who} stream ended after {frames} frames");
}

/// Adaptive first; the fixed converter is the fallback, and at a matching
/// rate that means no correction at all rather than no audio.
fn remote_resampler(rate: u32) -> Option<AudioResampler> {
    AudioResampler::adaptive(SAMPLE_RATE, rate).or_else(|| AudioResampler::new(SAMPLE_RATE, rate))
}

struct RemotePlaybackGuard(PlaybackHandle, u64);

impl Drop for RemotePlaybackGuard {
    fn drop(&mut self) {
        self.0.remove_track(self.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn direct_sources_take_one_dsp_block_per_call_and_refuse_other_sizes() {
        let source =
            NativeAudioSource::new(inert_apm_options(), SAMPLE_RATE, CHANNELS, SOURCE_QUEUE_MS);
        source
            .capture_frame(&pcm_frame(&[0; FRAME_SAMPLES]))
            .await
            .unwrap();
        for wrong in [FRAME_SAMPLES - 1, FRAME_SAMPLES + 1, FRAME_SAMPLES * 2] {
            let data = vec![0_i16; wrong];
            let frame = AudioFrame {
                data: std::borrow::Cow::Borrowed(&data),
                sample_rate: SAMPLE_RATE,
                num_channels: CHANNELS,
                samples_per_channel: wrong as u32,
            };
            assert!(
                source.capture_frame(&frame).await.is_err(),
                "{wrong} samples"
            );
        }
    }

    #[test]
    fn microphone_track_starts_muted_before_publication() {
        let source =
            NativeAudioSource::new(inert_apm_options(), SAMPLE_RATE, CHANNELS, SOURCE_QUEUE_MS);
        let track = new_muted_microphone_track(source);
        assert!(track.is_muted());
        assert!(!track.is_enabled());
    }

    #[test]
    fn microphone_mute_updates_dsp_native_track_and_publication_state() {
        let source =
            NativeAudioSource::new(inert_apm_options(), SAMPLE_RATE, CHANNELS, SOURCE_QUEUE_MS);
        let track = new_muted_microphone_track(source);
        let muted = AtomicBool::new(true);
        for value in [true, false, true] {
            apply_microphone_mute(&track, &muted, value);
            assert_eq!(muted.load(Ordering::Relaxed), value);
            assert_eq!(track.is_muted(), value);
            assert_eq!(track.is_enabled(), !value);
        }
    }

    #[test]
    fn microphone_overload_drains_input_and_keeps_new_audio_after_recovery() {
        let (tx, mut rx) = crate::audio_queue::channel();
        let accum = Arc::new(Mutex::new(MicBuffer::new()));
        let resampler = Arc::new(Mutex::new(None));
        let mut mono = Vec::new();
        let mut resampled = Vec::new();
        let stereo: Vec<_> = (0..FRAME_SAMPLES).flat_map(|_| [0.75, 0.25]).collect();
        let mut accepted = 0;
        for _ in 0..100 {
            accepted += forward_mic(
                &tx,
                &stereo,
                2,
                &accum,
                &resampler,
                &mut mono,
                &mut resampled,
            );
            assert!(accum.lock().samples.is_empty());
        }
        assert_eq!(accepted, rx.len());
        assert!(accepted < 100);
        let capture_id = accum.lock().id;
        let mut expected_sample = 0;
        while let Ok(packet) = rx.try_recv() {
            assert_eq!(packet.samples, [0.5; FRAME_SAMPLES]);
            assert_eq!(
                packet.position,
                Some(crate::audio_queue::CapturePosition {
                    capture_id,
                    first_sample: expected_sample,
                })
            );
            expected_sample += FRAME_SAMPLES as u64;
        }
        assert_eq!(
            forward_mic(
                &tx,
                &[0.25; FRAME_SAMPLES],
                1,
                &accum,
                &resampler,
                &mut mono,
                &mut resampled
            ),
            1
        );
        let resumed = rx.try_recv().unwrap();
        assert_eq!(resumed.samples, [0.25; FRAME_SAMPLES]);
        assert_eq!(
            resumed.position,
            Some(crate::audio_queue::CapturePosition {
                capture_id,
                first_sample: 100 * FRAME_SAMPLES as u64,
            })
        );
        assert_ne!(MicBuffer::new().id, capture_id);
    }

    #[test]
    fn stale_dsp_rows_keep_the_same_columns_as_processed_frames() {
        let row = stale_dsp_row();
        assert_eq!(row.split(',').count(), MIC_DSP_HEADER.split(',').count());
        assert_eq!(row.split(',').nth(5), Some("Stale"));
    }

    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    #[test]
    fn native_screen_stats_separate_capture_from_cpu_limited_encoding() {
        use livekit::webrtc::stats::{CodecStats, OutboundRtpStats, QualityLimitationReason};
        let mut outbound = OutboundRtpStats::default();
        outbound.stream.codec_id = "video-codec".into();
        outbound.outbound.frame_width = 1920;
        outbound.outbound.frame_height = 1080;
        outbound.outbound.frames_per_second = 9.0;
        outbound.outbound.quality_limitation_reason = QualityLimitationReason::Cpu;
        outbound.outbound.encoder_implementation = "libvpx".into();
        let mut codec = CodecStats::default();
        codec.rtc.id = "video-codec".into();
        codec.codec.mime_type = "video/VP8".into();
        let stats = native_screen_stats(&outbound, &[RtcStats::Codec(codec)], 60.0, Some(3602));
        assert_eq!(stats.capture_fps, Some(60.0));
        assert_eq!(stats.encoded_fps, Some(9.0));
        assert_eq!(stats.encoded_width, Some(1920));
        assert_eq!(stats.quality_limitation_reason.as_deref(), Some("cpu"));
        assert_eq!(stats.codec.as_deref(), Some("video/VP8"));
        assert_eq!(stats.bitrate_kbps, Some(3602));
        let absent = native_screen_stats(&OutboundRtpStats::default(), &[], 0.0, None);
        assert_eq!(absent.encoded_width, None);
        assert_eq!(absent.codec_implementation, None);
        assert_eq!(absent.power_efficient, None);
    }

    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    #[test]
    fn screen_share_negotiates_full_resolution_without_a_three_fps_layer() {
        let settings = crate::sysvideo::Settings {
            width: 1920,
            height: 1080,
            fps: 60,
            max_bitrate: 18_000_000,
            adaptive_quality: false,
            priority: crate::sysvideo::Priority::Motion,
            codec: crate::sysvideo::Codec::H264,
            encoder: crate::sysvideo::Encoder::Auto,
        };
        let encodings = livekit::options::compute_video_encodings(
            settings.width,
            settings.height,
            &screen_video_options(settings),
        );
        assert_eq!(encodings.len(), 1);
        assert_eq!(encodings[0].scale_resolution_down_by, Some(1.0));
        assert_eq!(encodings[0].max_framerate, Some(60.0));
        assert_eq!(encodings[0].max_bitrate, Some(18_000_000));
    }

    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    #[test]
    fn principal_encoding_is_high_quality_with_one_two_or_three_layers() {
        for count in 1..=3 {
            let presets: Vec<_> = (0..count)
                .map(|i| livekit::options::VideoPreset::new(320 << i, 180 << i, 500_000 << i, 30.0))
                .collect();
            let encodings = livekit::options::into_rtp_encodings(1280, 720, &presets);
            assert_eq!(encodings[0].rid, "f");
            let layers = livekit::options::video_layers_from_encodings(1280, 720, &encodings);
            assert_eq!(layers[0].quality, livekit::track::VideoQuality::High as i32);
            let expected_rids = ["f", "h", "q"];
            let expected_qualities = [
                livekit::track::VideoQuality::High,
                livekit::track::VideoQuality::Medium,
                livekit::track::VideoQuality::Low,
            ];
            for (index, preset) in presets.iter().rev().enumerate() {
                assert_eq!(encodings[index].rid, expected_rids[index]);
                assert_eq!(
                    encodings[index].max_bitrate,
                    Some(preset.encoding.max_bitrate)
                );
                assert_eq!(layers[index].quality, expected_qualities[index] as i32);
                assert_eq!(
                    (layers[index].width, layers[index].height),
                    (preset.width, preset.height)
                );
                assert_eq!(layers[index].bitrate as u64, preset.encoding.max_bitrate);
            }
        }
    }

    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    #[test]
    fn optional_screen_simulcast_advertises_two_distinct_qualities() {
        for (width, height, alternative_height) in [
            (1280, 720, 360),
            (1920, 1080, 720),
            (2560, 1440, 720),
            (3840, 2160, 720),
        ] {
            for fps in [15, 30, 60] {
                let mut settings = crate::features::screenshare::native_settings("smooth");
                settings.width = width;
                settings.height = height;
                settings.fps = fps;
                settings.adaptive_quality = true;
                let options = screen_video_options(settings);
                let encodings = livekit::options::compute_video_encodings(width, height, &options);
                assert_eq!(encodings.len(), 2);
                assert_eq!(encodings[0].rid, "f");
                assert_eq!(encodings[0].scale_resolution_down_by, Some(1.0));
                assert_eq!(encodings[0].max_framerate, Some(fps as f64));
                assert_eq!(encodings[1].rid, "h");
                assert_eq!(
                    encodings[1].scale_resolution_down_by,
                    Some(height as f64 / alternative_height as f64)
                );
                assert_eq!(encodings[1].max_framerate, Some(fps.min(30) as f64));
                assert_eq!(
                    encodings[1].max_bitrate,
                    Some(if alternative_height == 360 {
                        500_000
                    } else {
                        1_500_000
                    })
                );
                let layers =
                    livekit::options::video_layers_from_encodings(width, height, &encodings);
                assert_eq!(layers[0].quality, livekit::track::VideoQuality::High as i32);
                assert_eq!(
                    layers[1].quality,
                    livekit::track::VideoQuality::Medium as i32
                );
                assert_eq!(layers[1].height, alternative_height);
            }
        }
        let mut small = crate::features::screenshare::native_settings("720");
        small.width = 640;
        small.height = 360;
        small.adaptive_quality = true;
        assert!(!screen_video_options(small).simulcast);
    }

    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    #[test]
    fn screen_startup_bitrate_reaches_sdk_and_stays_inside_budget() {
        for (height, fps, cap, expected) in [
            (720, 30, 8_000_000, 2_000_000),
            (720, 60, 8_000_000, 3_000_000),
            (1080, 15, 14_000_000, 4_000_000),
            (1080, 30, 14_000_000, 4_000_000),
            (1080, 60, 17_000_000, 6_000_000),
            (1440, 30, 21_000_000, 8_000_000),
            (1440, 60, 34_000_000, 10_000_000),
            (1440, 15, 21_000_000, 5_000_000),
            (2160, 30, 42_000_000, 12_000_000),
            (2160, 60, 50_000_000, 14_000_000),
        ] {
            let mut settings = crate::features::screenshare::native_settings("smooth");
            settings.height = height;
            settings.fps = fps;
            settings.max_bitrate = cap;
            let options = screen_video_options(settings);
            assert_eq!(options.video_start_bitrate, Some(expected));
            assert_eq!(options.video_encoding.unwrap().max_bitrate, cap);
            assert_eq!(
                crate::livekit_start_bitrate_tests::start_bitrate_kbps(
                    Some(cap),
                    options.video_start_bitrate,
                ),
                Some((expected / 1000) as u32),
            );
            settings.max_bitrate = 2_000_000;
            assert!(screen_video_options(settings).video_start_bitrate.unwrap() <= 2_000_000);
        }
    }

    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    #[test]
    fn native_screen_publisher_preserves_codec_and_content_priority() {
        use crate::sysvideo::{Codec, Priority};
        for (preset, priority) in [
            ("smooth", DegradationPreference::MaintainFramerate),
            ("crisp", DegradationPreference::MaintainResolution),
            ("balanced", DegradationPreference::Balanced),
        ] {
            let settings = crate::features::screenshare::native_settings(preset);
            let options = screen_video_options(settings);
            assert_eq!(options.video_codec, VideoCodec::H264);
            assert_eq!(options.video_encoder, VideoEncoderBackend::Hardware);
            assert_eq!(options.degradation_preference, Some(priority));
            assert!(!options.simulcast);
        }
        let mut settings = crate::features::screenshare::native_settings("smooth");
        settings.codec = Codec::Vp8;
        settings.priority = Priority::Detail;
        let options = screen_video_options(settings);
        assert_eq!(options.video_codec, VideoCodec::VP8);
        settings.encoder = crate::sysvideo::Encoder::Cpu;
        assert_eq!(
            screen_video_options(settings).video_encoder,
            VideoEncoderBackend::Software
        );
        settings.encoder = crate::sysvideo::Encoder::Gpu;
        assert_eq!(
            screen_video_options(settings).video_encoder,
            VideoEncoderBackend::Hardware
        );
        assert_eq!(
            options.degradation_preference,
            Some(DegradationPreference::MaintainResolution)
        );
    }

    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    #[test]
    fn encoder_timing_uses_new_frames_and_handles_counter_resets() {
        assert_eq!(
            screen_encode_ms(Some((100, 1.0)), 160, 1.3).map(|v| v.round()),
            Some(5.0)
        );
        assert_eq!(screen_encode_ms(None, 160, 1.3), None);
        assert_eq!(screen_encode_ms(Some((160, 1.3)), 160, 1.3), None);
        assert_eq!(screen_encode_ms(Some((160, 1.3)), 1, 0.1), None);
        assert_eq!(screen_encode_ms(Some((100, 1.0)), 160, f64::NAN), None);
    }

    #[cfg(any(target_os = "android", target_os = "macos", target_os = "windows"))]
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "starts a bundled SFU; needs local media ports and a native encoder"]
    async fn native_screen_codecs_reach_a_real_decoder() {
        use discordia_server::livekit_bundle::{Credentials, ports, spawn_livekit};
        use livekit::webrtc::video_frame::I420Buffer;
        use livekit::webrtc::video_stream::native::NativeVideoStream;
        use livekit_api::access_token::{AccessToken, VideoGrants};
        use std::time::Duration;

        let available_backends: Vec<_> =
            VideoEncoderBackend::list_available().into_iter().collect();
        eprintln!("Available video encoders: {available_backends:?}");
        let verify_simulcast = std::env::var_os("DISCORDIA_TEST_SIMULCAST").is_some();
        let require_nvenc = std::env::var_os("DISCORDIA_TEST_REQUIRE_NVENC").is_some();
        let verify_nvenc_rates = std::env::var_os("DISCORDIA_TEST_NVENC_RATES").is_some();
        let verify_1440p60 = std::env::var_os("DISCORDIA_TEST_NVENC_1440P60").is_some();
        let verify_1440p =
            verify_1440p60 || std::env::var_os("DISCORDIA_TEST_NVENC_1440P").is_some();
        let legacy_start = std::env::var_os("DISCORDIA_TEST_LEGACY_START").is_some();
        assert!(!verify_1440p || verify_nvenc_rates);
        let require_mf = std::env::var_os("DISCORDIA_TEST_REQUIRE_MF").is_some();
        let verify_mf_fallback = std::env::var_os("DISCORDIA_TEST_MF_FALLBACK").is_some();
        assert!(!require_mf || !verify_mf_fallback);
        let modern_codec = std::env::var("DISCORDIA_TEST_MODERN_CODEC").ok();
        assert!(
            modern_codec
                .as_deref()
                .is_none_or(|codec| matches!(codec, "av1" | "h265"))
        );
        if require_mf || verify_mf_fallback || verify_nvenc_rates || modern_codec.is_some() {
            let _subscriber = tracing_subscriber::fmt()
                .with_env_filter(
                    std::env::var("DISCORDIA_TEST_MEDIA_LOG")
                        .unwrap_or_else(|_| "libwebrtc=debug,webrtc_sys=debug,livekit=warn".into()),
                )
                .with_test_writer()
                .try_init();
        }
        if require_nvenc {
            assert!(available_backends.contains(&VideoEncoderBackend::Nvenc));
        }

        let data_dir =
            std::env::temp_dir().join(format!("discordia-video-test-{}", uuid::Uuid::new_v4()));
        let credentials = Credentials::generate();
        let _sfu = spawn_livekit(
            discordia_server::livekit_bundle::Advertise::Mapped(
                "127.0.0.1".parse().expect("loopback"),
            ),
            &credentials,
            &data_dir,
        )
        .await
        .expect("start SFU");
        let url = format!("ws://127.0.0.1:{}", ports().ws);
        let encrypted_options = || {
            if std::env::var_os("DISCORDIA_TEST_MODERN_NO_E2EE").is_some() {
                return RoomOptions::default();
            }
            let provider = livekit::e2ee::key_provider::KeyProvider::with_shared_key(
                livekit::e2ee::key_provider::KeyProviderOptions::default(),
                vec![7; crate::mediakey::KEY_LEN],
            );
            let mut options = RoomOptions::default();
            options.encryption = Some(livekit::e2ee::E2eeOptions {
                encryption_type: livekit::e2ee::EncryptionType::Gcm,
                key_provider: provider,
            });
            options
        };
        for (codec, encoder) in [
            (crate::sysvideo::Codec::H264, crate::sysvideo::Encoder::Auto),
            (crate::sysvideo::Codec::Vp8, crate::sysvideo::Encoder::Auto),
            (crate::sysvideo::Codec::H264, crate::sysvideo::Encoder::Cpu),
        ] {
            if (verify_nvenc_rates || verify_mf_fallback || modern_codec.is_some())
                && (codec != crate::sysvideo::Codec::H264
                    || encoder != crate::sysvideo::Encoder::Auto)
            {
                continue;
            }
            let name = format!("screen-{}", uuid::Uuid::new_v4());
            let token = |identity| {
                AccessToken::with_api_key(&credentials.key, &credentials.secret)
                    .with_identity(identity)
                    .with_grants(VideoGrants {
                        room_join: true,
                        room: name.clone(),
                        can_publish: true,
                        can_subscribe: true,
                        ..Default::default()
                    })
                    .to_jwt()
                    .expect("token")
            };
            let (publisher, mut publisher_events) =
                Room::connect(&url, &token("publisher"), encrypted_options())
                    .await
                    .expect("publisher");
            let (listener, mut listener_events) =
                Room::connect(&url, &token("listener"), encrypted_options())
                    .await
                    .expect("listener");
            let events =
                tokio::spawn(async move { while publisher_events.recv().await.is_some() {} });
            let low_frames = Arc::new(AtomicU64::new(0));
            let recovered_frames = Arc::new(AtomicU64::new(0));
            let mut low_listener = None;
            let mut low_reader = None;
            if verify_simulcast {
                let (receiver, mut rx) =
                    Room::connect(&url, &token("low-listener"), encrypted_options())
                        .await
                        .expect("low listener");
                low_listener = Some(receiver);
                let low = low_frames.clone();
                let recovered = recovered_frames.clone();
                low_reader = Some(tokio::spawn(async move {
                    while let Some(event) = rx.recv().await {
                        if let RoomEvent::TrackSubscribed {
                            track: RemoteTrack::Video(video),
                            publication,
                            ..
                        } = event
                        {
                            publication.set_video_quality(livekit::track::VideoQuality::Medium);
                            let mut stream = NativeVideoStream::new(video.rtc_track());
                            while let Some(frame) = stream.next().await {
                                if frame.buffer.height() == 720 {
                                    let count = low.fetch_add(1, Ordering::Relaxed) + 1;
                                    if count == 45 {
                                        publication
                                            .set_video_quality(livekit::track::VideoQuality::High);
                                    }
                                } else if frame.buffer.height() == 1080
                                    && low.load(Ordering::Relaxed) >= 45
                                {
                                    recovered.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                }));
            }
            let received = Arc::new(AtomicU64::new(0));
            let decoded = received.clone();
            let reader = tokio::spawn(async move {
                while let Some(event) = listener_events.recv().await {
                    if let RoomEvent::TrackSubscribed {
                        track: RemoteTrack::Video(video),
                        ..
                    } = event
                    {
                        let mut stream = NativeVideoStream::new(video.rtc_track());
                        while let Some(frame) = stream.next().await {
                            assert!(frame.buffer.width() > 0 && frame.buffer.height() > 0);
                            if !verify_simulcast || frame.buffer.height() == 1080 {
                                decoded.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                }
            });
            let mut settings = crate::features::screenshare::native_settings("smooth");
            settings.codec = codec;
            settings.encoder = encoder;
            settings.adaptive_quality = verify_simulcast;
            if verify_nvenc_rates {
                settings.width = 1280;
                settings.height = 720;
                settings.fps = 30;
                settings.max_bitrate = 8_000_000;
                if verify_1440p {
                    settings.width = 2560;
                    settings.height = 1440;
                    settings.max_bitrate = 12_000_000;
                    if verify_1440p60 {
                        settings = crate::features::screenshare::native_settings("ultra");
                        settings.codec = codec;
                        settings.encoder = encoder;
                    }
                }
            }
            if modern_codec.is_some() {
                settings.width = 640;
                settings.height = 360;
                settings.fps = 30;
                settings.max_bitrate = 1_000_000;
            }
            let source = NativeVideoSource::new(
                VideoResolution {
                    width: settings.width,
                    height: settings.height,
                },
                true,
            );
            let track = LocalVideoTrack::create_video_track(
                "screen",
                RtcVideoSource::Native(source.clone()),
            );
            let mut publish_options = screen_video_options(settings);
            if legacy_start {
                publish_options.video_start_bitrate = None;
            }
            match modern_codec.as_deref() {
                Some("av1") => {
                    publish_options.video_codec = VideoCodec::AV1;
                    publish_options.video_encoder = VideoEncoderBackend::Software;
                }
                Some("h265") => {
                    publish_options.video_codec = VideoCodec::H265;
                    publish_options.video_encoder = VideoEncoderBackend::Hardware;
                }
                _ => {}
            }
            let requested_codec = publish_options.video_codec;
            let requested_encoder = publish_options.video_encoder;
            publisher
                .local_participant()
                .publish_track(LocalTrack::Video(track.clone()), publish_options)
                .await
                .expect("publish video");
            let mut interval =
                tokio::time::interval(Duration::from_secs_f64(1.0 / settings.fps as f64));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut last_bytes = None;
            let mut measured_bps = 0.0;
            let mut peak_target_bps = 0.0_f64;
            let frame_count = if verify_simulcast {
                360
            } else if verify_1440p {
                u64::from(settings.fps) * 30
            } else if verify_nvenc_rates {
                450
            } else {
                300
            };
            let mut noise_tile = vec![0; 65_536];
            let motion_started = Instant::now();
            for index in 0..frame_count {
                interval.tick().await;
                let mut buffer = I420Buffer::new(settings.width, settings.height);
                let (y, u, v) = buffer.data_mut();
                if verify_nvenc_rates {
                    let mut noise = index as u32 + 1;
                    for pixel in noise_tile.iter_mut() {
                        noise ^= noise << 13;
                        noise ^= noise >> 17;
                        noise ^= noise << 5;
                        *pixel = 16 + (noise % 220) as u8;
                    }
                    for chunk in y.chunks_mut(noise_tile.len()) {
                        chunk.copy_from_slice(&noise_tile[..chunk.len()]);
                    }
                } else {
                    y.fill((16 + index % 200) as u8);
                }
                u.fill(128);
                v.fill(128);
                source.capture_frame(&VideoFrame {
                    rotation: VideoRotation::VideoRotation0,
                    timestamp_us: 0,
                    frame_metadata: None,
                    buffer,
                });
                if verify_nvenc_rates && index % 60 == 59 {
                    let report = track.get_stats().await.expect("rate stats");
                    let outbound = report
                        .iter()
                        .find_map(|entry| match entry {
                            RtcStats::OutboundRtp(o) if o.stream.kind == "video" => Some(o),
                            _ => None,
                        })
                        .expect("outbound rate");
                    peak_target_bps = peak_target_bps.max(outbound.outbound.target_bitrate);
                    if index == 59 {
                        eprintln!(
                            "NVENC startup legacy={legacy_start} target={:.0} bps",
                            outbound.outbound.target_bitrate
                        );
                        if verify_1440p && !legacy_start {
                            assert!(
                                outbound.outbound.target_bitrate > 1_500_000.0,
                                "startup hint must reach WebRTC allocation"
                            );
                        }
                    }
                    let now = Instant::now();
                    if let Some((bytes, sampled)) = last_bytes {
                        measured_bps = (outbound.sent.bytes_sent - bytes) as f64 * 8.0
                            / now.duration_since(sampled).as_secs_f64();
                        eprintln!(
                            "NVENC motion frame={} actual={measured_bps:.0} target={:.0} cap={} bps",
                            index + 1,
                            outbound.outbound.target_bitrate,
                            settings.max_bitrate,
                        );
                    }
                    last_bytes = Some((outbound.sent.bytes_sent, now));
                }
            }
            let report = track.get_stats().await.expect("video stats");
            let outbound = report
                .iter()
                .find_map(|entry| match entry {
                    RtcStats::OutboundRtp(o) if o.stream.kind == "video" => Some(o),
                    _ => None,
                })
                .expect("outbound video");
            let stats = native_screen_stats(outbound, &report, 60.0, None);
            if verify_mf_fallback {
                assert_eq!(stats.codec_implementation.as_deref(), Some("OpenH264"));
                assert_eq!(stats.power_efficient, Some(false));
                assert!(received.load(Ordering::Relaxed) >= frame_count * 4 / 5);
                assert!(outbound.outbound.key_frames_encoded <= 8);
            }
            if verify_nvenc_rates {
                if verify_1440p {
                    assert_eq!(stats.encoded_width, Some(2560));
                    assert_eq!(stats.encoded_height, Some(1440));
                    assert!(
                        stats
                            .encoded_fps
                            .is_some_and(|fps| fps >= f64::from(settings.fps) * 0.85)
                    );
                    if !legacy_start {
                        assert!(
                            peak_target_bps >= settings.max_bitrate as f64 * 0.9,
                            "loopback must reach the 1440p budget: {}",
                            peak_target_bps
                        );
                    }
                }
                assert!(
                    outbound.outbound.key_frames_encoded <= 8,
                    "bitrate changes must not continuously force keyframes: {}",
                    outbound.outbound.key_frames_encoded
                );
                assert!(
                    outbound.outbound.target_bitrate > 2_000_000.0,
                    "test must exercise an increased budget"
                );
                assert!(
                    measured_bps > outbound.outbound.target_bitrate * 0.65,
                    "NVENC must apply WebRTC budget increases: actual={measured_bps} target={}",
                    outbound.outbound.target_bitrate
                );
            }
            if require_mf
                && codec == crate::sysvideo::Codec::H264
                && encoder != crate::sysvideo::Encoder::Cpu
            {
                assert!(
                    stats
                        .codec_implementation
                        .as_deref()
                        .is_some_and(|name| name.starts_with("Media Foundation H264 Encoder"))
                );
                assert_eq!(stats.power_efficient, Some(true));
            }
            if require_nvenc
                && codec == crate::sysvideo::Codec::H264
                && encoder != crate::sysvideo::Encoder::Cpu
            {
                assert_eq!(
                    stats.codec_implementation.as_deref(),
                    Some("NVIDIA H264 Encoder")
                );
                assert_eq!(stats.power_efficient, Some(true));
            }
            if encoder == crate::sysvideo::Encoder::Cpu {
                assert_eq!(
                    stats.codec_implementation.as_deref(),
                    Some(if verify_simulcast {
                        "SimulcastEncoderAdapter (OpenH264, OpenH264)"
                    } else {
                        "OpenH264"
                    })
                );
                assert_eq!(stats.power_efficient, Some(false));
            }
            eprintln!(
                "{requested_codec:?}/{requested_encoder:?}: codec={:?}, encoder={:?}, efficient={:?}, {}x{} @ {} FPS; {} decoded frames; {} keyframes; {:.2} ms/encode",
                stats.codec,
                stats.codec_implementation,
                stats.power_efficient,
                stats.encoded_width.unwrap_or(0),
                stats.encoded_height.unwrap_or(0),
                stats.encoded_fps.unwrap_or(0.0),
                received.load(Ordering::Relaxed),
                outbound.outbound.key_frames_encoded,
                outbound.outbound.total_encode_time * 1000.0
                    / f64::from(outbound.outbound.frames_encoded.max(1)),
            );
            if verify_nvenc_rates {
                let elapsed = motion_started.elapsed().as_secs_f64();
                eprintln!(
                    "Motion totals: {frame_count} supplied; {} encoded; {:.3} seconds; {:.2} encoded FPS average; {:.2} decoded FPS average; {} bps cap",
                    outbound.outbound.frames_encoded,
                    elapsed,
                    f64::from(outbound.outbound.frames_encoded) / elapsed,
                    received.load(Ordering::Relaxed) as f64 / elapsed,
                    settings.max_bitrate,
                );
            }
            assert!(
                received.load(Ordering::Relaxed) > 0,
                "no video reached the decoder"
            );
            assert!(outbound.outbound.frames_encoded > 0);
            assert_eq!(
                stats.codec.as_deref(),
                Some(match modern_codec.as_deref() {
                    Some("av1") => "video/AV1",
                    Some("h265") => "video/H265",
                    _ => match codec {
                        crate::sysvideo::Codec::Auto | crate::sysvideo::Codec::H264 => "video/H264",
                        crate::sysvideo::Codec::Vp8 => "video/VP8",
                    },
                })
            );
            if verify_simulcast {
                eprintln!(
                    "simulcast: {} principal frames, {} fallback frames, {} recovered frames",
                    received.load(Ordering::Relaxed),
                    low_frames.load(Ordering::Relaxed),
                    recovered_frames.load(Ordering::Relaxed)
                );
                assert!(
                    received.load(Ordering::Relaxed) > 60,
                    "main listener keeps full resolution"
                );
                assert!(
                    low_frames.load(Ordering::Relaxed) >= 45,
                    "second listener decodes the lower layer"
                );
                assert!(
                    recovered_frames.load(Ordering::Relaxed) > 30,
                    "second listener recovers full resolution"
                );
            }
            if let Some(receiver) = low_listener {
                receiver.close().await.expect("close low listener");
            }
            if let Some(reader) = low_reader {
                reader.abort();
            }
            listener.close().await.expect("close listener");
            publisher.close().await.expect("close publisher");
            reader.abort();
            events.abort();
        }
    }

    #[test]
    fn a_lone_speaker_is_not_quietened() {
        for s in [0.0, 0.1, 0.5, 0.9, 1.0, -0.7] {
            let mut limiter = crate::playback_limiter::Limiter::new(SAMPLE_RATE);
            let output = (0..=SAMPLE_RATE / 200)
                .map(|_| limiter.process(s))
                .last()
                .unwrap();
            assert_eq!(output, s);
        }
    }

    /// The f32 output stream writes what it is handed, so a sum that leaves
    /// full scale reaches the device as clipping it never asked for.
    #[test]
    fn a_sound_follows_the_soundboard_volume_and_a_local_mute() {
        let mut gains = HashMap::new();
        let streams = HashMap::new();
        gains.insert("loud".to_string(), 1.8);
        gains.insert("muted".to_string(), 0.0);
        let board = |who| track_gain(TrackKind::Soundboard, who, &gains, &streams, 40);
        assert_eq!(board("stranger"), 0.4);
        assert_eq!(
            board("loud"),
            0.4,
            "a person's voice boost is not a soundboard boost"
        );
        assert_eq!(board("muted"), 0.0);
        assert_eq!(
            track_gain(TrackKind::Soundboard, "x", &gains, &streams, 900),
            1.0
        );
    }

    #[test]
    fn native_stream_publishers_use_the_viewers_volume_and_mute() {
        let gains = HashMap::new();
        let mut streams = HashMap::from([("alice".to_string(), 0.35)]);
        assert_eq!(
            track_gain(TrackKind::Stream, "alice#video", &gains, &streams, 100),
            0.35
        );
        streams.insert("alice".into(), 0.0);
        assert_eq!(
            track_gain(TrackKind::Stream, "alice#video", &gains, &streams, 100),
            0.0
        );
        assert!(!subscriptions::wanted(
            TrackSource::ScreenshareAudio,
            "screen-audio",
            "alice#video",
            "alice",
            true,
            &HashSet::from(["alice".into()])
        ));
    }

    #[test]
    fn rejoining_without_watching_clears_the_previous_stream_gain() {
        let mut state = AppState::empty();
        state.screen_viewing.insert("alice".into());
        state.stream_volumes.insert("alice".into(), 50);
        let controls = AudioControls::from_state(&state);
        let mut watched = controls.watched_streams.subscribe();
        controls.sync_stream_gains(&state);
        assert_eq!(
            track_gain(
                TrackKind::Stream,
                "alice#video",
                &HashMap::new(),
                &controls.stream_gains.lock(),
                100
            ),
            0.5
        );
        state.end_voice_locally();
        state
            .screen_shares
            .insert(uuid::Uuid::new_v4(), vec!["alice".into()]);
        controls.sync_stream_gains(&state);
        assert!(watched.has_changed().unwrap());
        assert!(watched.borrow_and_update().is_empty());
        assert_eq!(
            track_gain(
                TrackKind::Stream,
                "alice#video",
                &HashMap::new(),
                &controls.stream_gains.lock(),
                100
            ),
            0.0
        );
        assert_eq!(
            track_gain(
                TrackKind::Voice,
                "alice",
                &HashMap::new(),
                &controls.stream_gains.lock(),
                100
            ),
            1.0
        );
    }

    #[test]
    fn a_track_is_a_sound_by_its_name_whatever_its_source() {
        assert_eq!(
            TrackKind::of(TrackSource::Unknown, SOUNDBOARD_TRACK),
            TrackKind::Soundboard
        );
        assert_eq!(
            TrackKind::of(TrackSource::ScreenshareAudio, "screen-audio"),
            TrackKind::Stream
        );
        assert_eq!(
            TrackKind::of(TrackSource::Microphone, "mic"),
            TrackKind::Voice
        );
    }

    #[test]
    fn a_sum_never_leaves_full_scale() {
        let mut limiter = crate::playback_limiter::Limiter::new(SAMPLE_RATE);
        for sample in [1.8, -2.4].into_iter().cycle().take(SAMPLE_RATE as usize) {
            assert!(limiter.process(sample).abs() <= 1.0);
        }
    }

    #[test]
    fn quiet_speech_passes_and_agc_does_not_keep_room_tone_open() {
        let bar = crate::settings::ClientSettings::default().mic_sensitivity as i32;
        let mut agc = crate::agc::Agc::new();
        let mut gate = GateState::default();
        for _ in 0..600 {
            let mut hop = [0.02_f32; FRAME_SAMPLES];
            let gate_peak = peak_fixed(&hop);
            agc.process(&mut hop).unwrap();
            assert!(matches!(
                gate.step(gate_peak, bar),
                GateAction::RampIn | GateAction::Pass
            ));
        }
        let mut leaked = 0;
        for _ in 0..500 {
            let mut hop = [0.003_f32; FRAME_SAMPLES];
            let gate_peak = peak_fixed(&hop);
            agc.process(&mut hop).unwrap();
            if !matches!(gate.step(gate_peak, bar), GateAction::Drop) {
                leaked += 1;
            }
        }
        assert!(leaked < 60, "room tone stayed open for {leaked} hops");
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
}
