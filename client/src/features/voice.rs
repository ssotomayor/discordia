//! Voice service. Owns the LiveKit Room, the microphone capture pipeline,
//! and the remote-audio playback pipeline.
//!
//! LiveKit is a libwebrtc-based SFU. Connecting to a room gives us libwebrtc's
//! AEC3 / NS / AGC / Opus / congestion control end-to-end. The Rust SDK
//! exposes only PCM frames at the edges; cpal handles the actual mic and
//! speaker devices.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use dioxus::core::Task;
use dioxus::prelude::*;
use futures_util::StreamExt;
use livekit::options::{AudioEncoding, TrackPublishOptions};
use livekit::prelude::*;
use livekit::webrtc::audio_frame::AudioFrame;
use livekit::webrtc::audio_source::AudioSourceOptions;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::prelude::RtcAudioSource;
use livekit::webrtc::stats::RtcStats;
// Screen video publishing (`ScreenVideoRoom`). `NativeBuffer` is the zero-copy
// wrapper around a platform image buffer — the reason ScreenCaptureKit frames
// reach the encoder without a conversion pass in this process.
//
// macOS-gated with the code that uses them: `sysvideo` has a backend only there,
// and the frame path ends in `NativeBuffer::from_cv_pixel_buffer`, which is
// CoreVideo. Elsewhere the webview keeps the share, as it always did.
#[cfg(target_os = "macos")]
use livekit::options::VideoEncoding;
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

/// Chunk size for the rubato resampler. Must be a power-of-two-friendly value
/// for FFT efficiency. 512 is a good balance of latency (~10ms @ 48k) and CPU.
const RESAMPLER_CHUNK: usize = 512;

/// Playback jitter buffer cap, as a divisor of the device sample rate: 5 =>
/// ~200ms. Must stay above `DRIFT_OVERRUN_SECS` so the two mechanisms don't
/// fight — see `PlaybackMixer::new`. It stays generous on purpose: it is the
/// ceiling for a burst, not the depth we aim to sit at.
const PLAYBACK_CAP_DIVISOR: u32 = 5;

/// Band the drift compensator keeps each track's buffer inside, in seconds of
/// device-rate samples. Above the overrun mark it drops a sample now and then,
/// below the underrun mark it repeats one; inside the band it does nothing.
///
/// The two marks answer different questions, which is why they moved by
/// different amounts from the 150ms/50ms this used to hold:
///
///  * The **underrun** mark is what costs latency. The buffer spends most of
///    its life being defended up toward it, so it is roughly the delay this
///    stage adds. It can afford to be low because this is the *second* buffer
///    in the path — libwebrtc's NetEq has already absorbed network jitter and
///    concealed losses upstream, leaving this one only the difference between
///    the stream's clock and the output device's. Dipping below it is not a
///    dropout either; it just starts stretching. Silence needs the buffer to
///    reach zero.
///  * The **width** of the band is what costs artefacts. Every correction is a
///    dropped or duplicated sample, so a narrow band means constant fiddling
///    with the signal. Keeping it wide is nearly free — the ceiling only has
///    to stay under `PLAYBACK_CAP_DIVISOR` so the producer's cap can't hide
///    the overrun branch.
///
/// So: floor down (latency), ceiling roughly where it was (burst tolerance,
/// few corrections). Start conservative and tighten with real hardware —
/// dropping the floor further is the next thing to try if latency still reads
/// high, and raising it is the fix if a device turns out to drift harder than
/// this leaves room for.
const DRIFT_OVERRUN_SECS: f64 = 0.12;
const DRIFT_UNDERRUN_SECS: f64 = 0.03;

/// How long the transmit gate stays open after the last frame above threshold,
/// in 10ms frames. Speech has gaps — breaths, stops between words — and a gate
/// that slams shut in them chops the front off the next syllable. 300ms is the
/// usual compromise between "doesn't clip speech" and "doesn't leak the room".
const GATE_HANGOVER_FRAMES: u32 = 30;

/// How far below the opening threshold the gate will hold, in percent.
///
/// A gate that opens and closes at one level chatters on any signal sitting
/// near it, and noise suppression is what puts a signal there: the gate judges
/// the denoised hop, the model is allowed to pull a hop down by
/// `denoise::ATTEN_LIM_DB`, and the material it pulls hardest — word tails,
/// unvoiced consonants, breaths — is the quiet part of ordinary speech. Held to
/// one threshold, that arrives at the far end as a voice that swells and drops.
/// -6 dB of hysteresis is the standard answer.
const GATE_CLOSE_RATIO_PCT: i32 = 50;

/// Per-hop decay of the level the gate reads, in percent.
///
/// The gate follows a released envelope rather than each hop's own peak, so a
/// single dip cannot start the hangover counting down. 75% per 10ms hop is
/// about -2.5 dB a hop. It only ever makes the gate more permissive: on a
/// rising signal the envelope *is* the peak, so the threshold marker still
/// marks where the gate opens.
const GATE_ENVELOPE_DECAY_PCT: i32 = 75;

/// Length of the ramp applied when the gate opens or closes, in samples.
///
/// Dropping a frame outright takes the signal from full to nothing between one
/// 10ms hop and the next, which is a click on the way out and a clipped
/// consonant on the way in. 2.5ms is short enough not to soften a real onset.
const GATE_RAMP_SAMPLES: usize = 120;

/// How long to wait for a clean leave before giving up on it.
const ROOM_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// libwebrtc APM configuration.
///
/// `noise_suppression` is the inverse of the DeepFilterNet toggle: exactly one
/// suppressor should be in the path. AEC is orthogonal (echo, not noise or
/// level) and stays on regardless.
///
/// AGC is the user's call, because it and the manual input-gain slider are two
/// answers to the same question. Left on, it quietly walks the level back
/// toward its own target over a second or two, so a manual boost reads as a
/// laggy control that half-works.
fn apm_options(deepfilter_on: bool, agc: bool) -> AudioSourceOptions {
    AudioSourceOptions {
        echo_cancellation: true,
        noise_suppression: !deepfilter_on,
        auto_gain_control: agc,
    }
}

/// Live audio knobs shared between the UI thread, the cpal callbacks and the
/// publish task.
///
/// These deliberately avoid Dioxus signals: the audio path runs off the UI
/// thread and must never block on a `Signal` read (the cpal callback is
/// realtime and the publish task runs 100 times a second). They're owned by
/// `service_loop` and cloned into each session, so a device change or a
/// reconnect — which tears down and rebuilds `ActiveVoice` — keeps every
/// setting the user picked.
#[derive(Clone)]
struct AudioControls {
    /// Transmit/speaking threshold as peak ×1000 fixed point (1..=1000), the
    /// same scale the VU bar renders, so the slider's marker sits exactly where
    /// the gate actually opens.
    threshold: Arc<AtomicI32>,
    /// Microphone input gain as a percentage (100 = unity). Applied to the hop
    /// before anything measures it, so `threshold` and the VU bar are both in
    /// post-gain terms.
    mic_gain_pct: Arc<AtomicI32>,
    /// libwebrtc AGC. Read when the APM options are (re)applied, not per hop.
    agc: Arc<AtomicBool>,
    /// DeepFilterNet noise suppression on the captured mic signal.
    denoise: Arc<AtomicBool>,
    /// Opus bitrate for the mic track, in kbit/s. Read once, when the track is
    /// published — unlike the knobs above there is nothing to re-read per hop,
    /// because the encoder is configured at publish time.
    bitrate_kbps: Arc<AtomicU32>,
    /// Whether the connection-stats panel is open. Gates the stats poll, which
    /// walks every peer connection once a second and is worth nothing while
    /// nobody is reading it.
    stats_polling: Arc<AtomicBool>,
    /// Playback deafen. A gate rather than a level, and read ahead of the two
    /// maps below: it overrides them instead of being written into them, so the
    /// per-participant volumes the user picked are still there on undeafen.
    deafened: Arc<AtomicBool>,
    /// Per-participant playback gain, keyed by LiveKit identity (= pubkey).
    /// Absent = unity. Applied to *incoming* audio in our own mixer only.
    gains: Arc<Mutex<HashMap<String, f32>>>,
    /// The same, for screen-share audio tracks. Absent = SILENT, not unity:
    /// stream audio plays only while you are watching that person's share, so
    /// the default has to be off.
    stream_gains: Arc<Mutex<HashMap<String, f32>>>,
}

impl AudioControls {
    fn new(threshold: u32, mic_volume: u16, agc: bool, denoise: bool, bitrate_kbps: u32) -> Self {
        Self {
            threshold: Arc::new(AtomicI32::new(threshold as i32)),
            mic_gain_pct: Arc::new(AtomicI32::new(mic_volume as i32)),
            agc: Arc::new(AtomicBool::new(agc)),
            denoise: Arc::new(AtomicBool::new(denoise)),
            bitrate_kbps: Arc::new(AtomicU32::new(bitrate_kbps)),
            stats_polling: Arc::new(AtomicBool::new(false)),
            deafened: Arc::new(AtomicBool::new(false)),
            gains: Arc::new(Mutex::new(HashMap::new())),
            stream_gains: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

/// What the mic pipeline reports back to the UI: the live input peak and
/// whether the gate is currently passing audio. Written by the capture path,
/// polled by a task that mirrors them into `AppState`.
#[derive(Default)]
struct MicMeter {
    /// Peak since the last poll, ×1000 fixed point. Swapped to 0 on read.
    peak: AtomicI32,
    /// True while the transmit gate is open.
    open: AtomicBool,
}

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
    /// Silence *playback* — every remote voice and every stream at once.
    ///
    /// Deliberately separate from `SetMute`: this is a gate on the mixer, mute
    /// is a gate on capture, and they sit at opposite ends of the pipeline.
    /// That deafening also mutes is a rule of the UI — which sends both — not a
    /// fact about the audio path.
    SetDeafen {
        deafened: bool,
    },
    /// Set the microphone gate threshold (1..=1000, peak ×1000). Below it the
    /// mic is treated as inactive and nothing is transmitted.
    SetSensitivity {
        threshold: u32,
    },
    /// Toggle DeepFilterNet noise suppression on captured mic audio.
    SetNoiseCancellation {
        enabled: bool,
    },
    /// Microphone input gain as a percentage (100 = unity). Takes effect on the
    /// next 10ms hop; no session rebuild.
    SetMicVolume {
        percent: u16,
    },
    /// Toggle libwebrtc's automatic gain control on the live capture.
    SetAutoGainControl {
        enabled: bool,
    },
    /// Opus bitrate for the microphone track, in kbit/s. Stored for the next
    /// publish rather than applied live: the encoder is configured when the
    /// track is published, so changing it mid-call would mean tearing the
    /// publication down and putting it back — a gap in everyone else's audio
    /// for a setting nobody changes mid-sentence.
    SetVoiceBitrate {
        kbps: u32,
    },
    /// Open or close the connection-stats readout. Only a gate on the poll —
    /// the numbers come from the peer connection, which costs a walk of every
    /// track per tick, so nothing is collected while the panel is closed.
    SetStatsPolling {
        enabled: bool,
    },
    /// Start/stop capturing this machine's audio and publishing it alongside a
    /// screen share. Only does anything where `sysaudio` has a backend.
    SetSystemAudio {
        enabled: bool,
        /// Native macOS capture must use the same selected surface as video.
        /// Windows ignores this because its native path applies only after a
        /// webview whole-screen pick, which exposes no native target id.
        target: Option<crate::sysvideo::Target>,
    },
    /// Join or leave the screen-share room as an audio-only subscriber, so a
    /// share captured by someone else's *webview* still plays through our
    /// chosen output device rather than the webview's.
    ///
    /// `None` disconnects. Sent when the set of other people sharing in our
    /// voice channel becomes non-empty / empty.
    SetScreenAudio {
        room: Option<(String, String)>,
    },
    /// Start/stop capturing this machine's *screen* and publishing it as a
    /// video track, for platforms where the webview has no capture API at all
    /// (macOS — see `sysvideo`). `None` stops.
    ///
    /// Carries its own `(url, token)` because this joins the screen room under a
    /// third identity: the webview holds the bare pubkey and the audio
    /// subscriber holds `#audio`, so a publisher needs `#video`. See
    /// `server::livekit::screen_video_identity`.
    SetScreenVideo {
        room: Option<(String, String)>,
        /// Which surface to capture. Ignored when `room` is None.
        target: crate::sysvideo::Target,
        settings: crate::sysvideo::Settings,
    },
    /// Set the local playback gain for a participant's *screen-share* audio,
    /// separately from their voice. 0.0 while you aren't watching them, which
    /// is the default — stream audio follows the watch window.
    SetStreamVolume {
        pubkey: String,
        gain: f32,
    },
    /// Set one participant's *local* playback gain (1.0 = unity, 0.0 = muted
    /// for us only). Never leaves this client — it scales incoming audio in our
    /// mixer, so it cannot touch the speaker's mic or any other listener.
    SetUserVolume {
        pubkey: String,
        gain: f32,
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
    // Audio knobs outlive individual sessions: seeded from the settings the UI
    // already restored into AppState, then mutated in place by the commands
    // below. A reconnect rebuilds the pipeline around the *same* controls, so
    // sensitivity, noise cancellation and per-user volumes all survive it.
    let controls = {
        let s = state.read();
        AudioControls::new(
            s.mic_sensitivity,
            s.mic_volume,
            s.auto_gain_control,
            s.noise_cancellation,
            s.voice_bitrate_kbps,
        )
    };

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
                            // A fresh session owns none of what the old one was
                            // told; whoever has to re-issue those commands
                            // watches this.
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
            VoiceCmd::Disconnect => {
                eprintln!("[voice] Disconnect");
                if let Some(prev) = session.take() {
                    prev.shutdown(state).await;
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
                if session.is_some()
                    && let Some((ref url, ref tok, cid)) = last_connect.clone()
                {
                    eprintln!("[voice] Reconnecting to apply device changes");
                    if let Some(prev) = session.take() {
                        prev.shutdown(state).await;
                    }
                    match ActiveVoice::connect(url, tok, cid, state, controls.clone()).await {
                        Ok(active) => {
                            eprintln!("[voice] reconnected ok");
                            // update phase in its own small scope
                            {
                                let mut s = state.write();
                                s.voice.phase = VoicePhase::Connected;
                                s.voice_session_epoch += 1;
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
            VoiceCmd::SetMute { muted } => {
                eprintln!("[voice] SetMute muted={muted}");
                if let Some(active) = session.as_mut() {
                    active.set_muted(muted).await;
                }
                state.write().voice.muted = muted;
            }
            VoiceCmd::SetDeafen { deafened } => {
                eprintln!("[voice] SetDeafen deafened={deafened}");
                // Nothing to hand to the session: the flag lives on `controls`,
                // which outlives it, so a device change rebuilds the mixer
                // around the same atomic and comes back up still deafened.
                controls.deafened.store(deafened, Ordering::Relaxed);
                state.write().voice.deafened = deafened;
            }
            VoiceCmd::SetSensitivity { threshold } => {
                let threshold = threshold.clamp(1, 1000);
                eprintln!("[voice] SetSensitivity threshold={threshold}");
                // The live pipeline reads this atomic, so the change lands on
                // the very next 10ms frame — no reconnect, no session restart.
                controls
                    .threshold
                    .store(threshold as i32, Ordering::Relaxed);
                state.write().mic_sensitivity = threshold;
            }
            VoiceCmd::SetNoiseCancellation { enabled } => {
                eprintln!("[voice] SetNoiseCancellation enabled={enabled}");
                controls.denoise.store(enabled, Ordering::Relaxed);
                // Hand libwebrtc's own suppressor over to DeepFilterNet, live —
                // `set_audio_options` reconfigures the APM without republishing
                // the track.
                if let Some(active) = session.as_ref() {
                    active.set_apm(enabled, controls.agc.load(Ordering::Relaxed));
                }
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
                // Same live-reconfigure path as the suppressor toggle: the APM
                // is swapped under the running track, nothing republishes.
                if let Some(active) = session.as_ref() {
                    active.set_apm(controls.denoise.load(Ordering::Relaxed), enabled);
                }
                state.write().auto_gain_control = enabled;
            }
            VoiceCmd::SetVoiceBitrate { kbps } => {
                let kbps = if kbps == 24 { 24 } else { 48 };
                eprintln!("[voice] SetVoiceBitrate kbps={kbps} (applies on next connect)");
                controls.bitrate_kbps.store(kbps, Ordering::Relaxed);
                state.write().voice_bitrate_kbps = kbps;
            }
            VoiceCmd::SetStatsPolling { enabled } => {
                // No session plumbing: the poll task reads this atomic every
                // tick and the flag lives on `controls`, which outlives
                // individual sessions — so leaving the panel open across a
                // device change or a channel switch keeps measuring.
                controls.stats_polling.store(enabled, Ordering::Relaxed);
            }
            VoiceCmd::SetSystemAudio { enabled, target } => {
                if let Some(active) = session.as_mut() {
                    // A share that turns out to be silent is the most confusing
                    // outcome there is — the sharer has no way to tell. So the
                    // reason the OS gave goes straight to the user; the share
                    // itself carries on, video-only.
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
                    // Unlike system audio, a failure here is the whole feature:
                    // there is no video-only fallback to carry on with, because
                    // this *is* the video. Say so and clear the sharing flag so
                    // the button doesn't sit lit for a share that isn't running.
                    if let Err(e) = active.set_screen_video(room, target, settings, state).await {
                        eprintln!("[voice] screen video failed: {e}");
                        let mut s = state.write();
                        s.screen_sharing = false;
                        // Clear the surface too: the effect that owns publishing
                        // keys on it, so leaving it set would make the next click
                        // a no-op against an unchanged key.
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

/// Active voice session: holds the LiveKit Room plus the audio I/O streams.
struct ActiveVoice {
    room: Arc<Room>,
    /// Kept so the APM can be reconfigured mid-call.
    source: NativeAudioSource,
    mic: MicCapture,
    local_audio: LocalAudioTrack,
    _playback: PlaybackMixer,
    event_task: tokio::task::JoinHandle<()>,
    /// Live system-audio publication, when sharing a screen with sound.
    system_audio: Option<SystemAudioTrack>,
    /// Audio-only connection to the screen-share room, live while someone else
    /// in the channel is sharing.
    screen_audio: Option<ScreenAudioRoom>,
    /// Publish-only connection to the screen-share room, live while *we* are
    /// sharing on a platform whose webview cannot capture.
    screen_video: Option<ScreenVideoRoom>,
    /// Kept so the screen-audio room can feed the same mixer as voice — which
    /// is the entire point: one output device, one set of gains.
    mixer: PlaybackHandle,
    /// Our own LiveKit identity, so the screen-audio room can skip our own
    /// share. Read once here because the event task cannot touch a Signal.
    ///
    /// `Option` rather than a defaulted empty string: a pubkey is never
    /// legitimately empty, so `""` could only mean "there was no `self_user`" —
    /// and silently comparing identities against it would match nobody, leaving
    /// the sharer subscribed to their own screen audio. Absence is a reason to
    /// refuse the room, not to guess.
    self_pubkey: Option<String>,
    /// Mirrors the mic meter into `AppState`. Cancelled on shutdown — left
    /// running, one accumulates per reconnect and they fight over the same
    /// `mic_level` / `speaking` fields.
    meter_task: Task,
    /// Polls the peer connection for the stats panel. Cancelled on shutdown
    /// for the same reason as `meter_task`, with one more: it holds an
    /// `Arc<Room>`, so leaving it running would keep a closed room alive.
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
        let (room, mut events) = Room::connect(livekit_url, token, RoomOptions::default())
            .await
            .map_err(|e| format!("livekit connect: {e}"))?;
        let room = Arc::new(room);

        // Microphone publish pipeline. APM (AEC + NS + AGC).
        //
        // Echo cancellation and AGC always stay on — they solve problems
        // DeepFilterNet doesn't touch. Noise suppression is the one that
        // overlaps, so it follows the DeepFilterNet toggle (see
        // `apm_options`): running libwebrtc's suppressor over audio the model
        // has already cleaned means a second mask applied to a signal that no
        // longer has the noise it was estimated from, which mostly costs
        // artefacts on quiet consonants.
        //
        // Same-machine testing: AEC is effectively a pass-through because we
        // don't wire a render reference signal to libwebrtc. Real two-machine
        // deployments get the full benefit.
        let source = NativeAudioSource::new(
            apm_options(
                controls.denoise.load(Ordering::Relaxed),
                controls.agc.load(Ordering::Relaxed),
            ),
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
                    // The rest of the speech defaults are right — including
                    // `dtx`, which costs nothing when the transmit gate is
                    // already holding silence back, and `red`, which is what
                    // makes a lost packet survivable. Only the bitrate is
                    // ours: the SDK's SPEECH preset is 24 kbit/s, which is
                    // thin for anything but a close-miked talking head, so the
                    // user picks (see `ClientSettings::voice_bitrate_kbps`).
                    audio_encoding: Some(AudioEncoding {
                        max_bitrate: controls.bitrate_kbps.load(Ordering::Relaxed) as u64 * 1000,
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| format!("publish mic: {e}"))?;

        // The capture path is three stages, for three different reasons.
        //
        // cpal's callback is realtime: it only downmixes, resamples and cuts
        // the buffer into exact 10ms hops. Those hops go to a dedicated DSP
        // thread that does the expensive work — DeepFilterNet inference and the
        // transmit gate — because a 150µs model run inside a callback with a
        // ~10ms budget invites xruns the moment the machine gets busy (and
        // because the model is `!Send`, so it cannot live in a task at all).
        // What survives the gate reaches a tokio task, which exists purely
        // because `NativeAudioSource::capture_frame` is async: calling it from
        // the sync audio thread and dropping the future means it never runs.
        //
        // Hops stay f32 until the last step so the model works at full
        // resolution; quantization to i16 happens after the gate.
        let (frame_tx, frame_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        let (gated_tx, gated_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
        let meter = Arc::new(MicMeter::default());
        // Seeded from state, not hardcoded off: a device change tears this
        // session down and rebuilds it, and nothing re-issues `SetMute`, so
        // starting at false put the mic back on the air with the button still
        // red. Deafen makes that worse — `AudioControls` survives the rebuild,
        // so you would come back deafened *and* transmitting.
        let start_muted = state.peek().voice.muted;
        let muted = Arc::new(AtomicBool::new(start_muted));
        // The atomic only gates the DSP thread; the publication has its own
        // switch, and `set_muted` flips both — so a rebuilt session must too.
        local_audio_for_mute.rtc_track().set_enabled(!start_muted);
        {
            let controls = controls.clone();
            let meter = meter.clone();
            let muted = muted.clone();
            std::thread::Builder::new()
                .name("dxf-mic-dsp".into())
                .spawn(move || denoise_gate_loop(frame_rx, gated_tx, controls, meter, muted))
                .map_err(|e| format!("spawn mic dsp thread: {e}"))?;
        }
        tokio::spawn(publish_loop(gated_rx, source.clone()));
        let mic = MicCapture::start(frame_tx, state, muted)?;
        let meter_task = spawn_meter_task(state, meter);

        // Remote audio mixer.
        let playback = PlaybackMixer::start(state, controls.clone())?;
        let mixer_handle = playback.handle();

        // Bridge for "this sharer has native screen audio" notices: the event
        // task can't touch a Signal, so it posts these here and a Dioxus task
        // applies them.
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
                            // The other half of the flap. `joined=true` was
                            // already logged; without this the log shows the
                            // room arriving and the webview taking playback
                            // back 50ms later with nothing in between.
                            crate::dlog!(
                                "voice screen_audio RoomGone -> joined=false (playback back to webview)"
                            );
                            s.screen_audio_joined = false;
                        }
                    }
                }
            });
        }

        // Same bridge, for how well each participant's connection is holding
        // up. Kept separate from the stream-audio one so a burst of quality
        // updates — they arrive on a timer for everyone in the room — can't
        // delay a "this sharer has sound" notice behind it.
        let (quality_tx, mut quality_rx) = tokio::sync::mpsc::unbounded_channel::<QualityMsg>();
        {
            let mut state = state;
            dioxus::prelude::spawn(async move {
                while let Some(msg) = quality_rx.recv().await {
                    // The SFU reports on a timer, for every participant, and
                    // the reading is almost always the one we already hold.
                    // Taking the write lock regardless would re-render the
                    // channel list once per participant per tick for the whole
                    // length of every call, so look first with a peek — which
                    // doesn't mark the signal dirty — and only write on a real
                    // change.
                    let changed = {
                        let s = state.peek();
                        match &msg {
                            QualityMsg::Set(id, health) => s.voice_quality.get(id) != Some(health),
                            QualityMsg::Drop(id) => s.voice_quality.contains_key(id),
                            QualityMsg::Clear => !s.voice_quality.is_empty(),
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
                    }
                }
            });
        }

        // Event task: subscribe to room events, hook up remote tracks.
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
                            // Their screen-audio volume goes with them. Absent
                            // means silent here — the default is "you are not
                            // watching" — so an entry left behind is a level
                            // set for a share that ended, and the next one they
                            // start would be audible before the watch window is
                            // even open.
                            //
                            // Deliberately NOT `gains`: absent means unity
                            // there, so a leftover entry is the listener's own
                            // "turn this person down", and nothing re-sends it
                            // on rejoin. Dropping it would silently undo their
                            // choice while the slider went on showing it.
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
                            // Only worth a log line when it's bad — this fires
                            // on a timer for every participant, and a healthy
                            // room would drown the log in "Excellent".
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
                            // Take the sharer back out of "has stream audio",
                            // or the watch window goes on offering a volume
                            // slider over a stream that ended.
                            if publication.source() == TrackSource::ScreenshareAudio {
                                let _ = native_audio_tx
                                    .send(StreamAudio::Gone(participant.identity().0.clone()));
                            }
                        }
                        RoomEvent::Disconnected { reason } => {
                            eprintln!("[voice] room disconnected: {reason:?}");
                            let _ = quality_tx.send(QualityMsg::Clear);
                        }
                        // The SDK is recovering the connection on our behalf.
                        // Say so rather than letting the call just go quiet:
                        // silence with no explanation is the thing that makes
                        // people restart the app mid-conversation.
                        RoomEvent::Reconnecting => {
                            eprintln!("[voice] reconnecting");
                        }
                        RoomEvent::Reconnected => {
                            eprintln!("[voice] reconnected");
                        }
                        _ => {}
                    }
                    if let RoomEvent::TrackSubscribed {
                        track,
                        publication,
                        participant,
                    } = ev
                    {
                        // Screen-share audio and a microphone arrive on the
                        // same event; only the publication says which is which.
                        let is_stream = publication.source() == TrackSource::ScreenshareAudio;
                        if let RemoteTrack::Audio(audio) = track {
                            let stream = NativeAudioStream::new(
                                audio.rtc_track(),
                                SAMPLE_RATE as i32,
                                CHANNELS as i32,
                            );
                            let mixer_handle = mixer_handle.clone();
                            // The LiveKit identity is the user's pubkey (the
                            // gateway mints tokens with `with_identity(pubkey)`),
                            // which is what the per-user volume map is keyed by.
                            let identity = participant.identity().0.clone();
                            // Tell the UI this sharer has sound, so the watch
                            // window's volume control comes out of "no stream
                            // audio" — the JS layer can't know about a track
                            // that never touches the webview. Routed through a
                            // channel because this task is `tokio::spawn`ed and
                            // so must be Send, which a Dioxus Signal is not.
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
                // For a stream that ends without a Disconnected event — a
                // dropped room handle, say. Deliberately not the shutdown
                // path: that aborts this task, so nothing here runs and
                // `ActiveVoice::shutdown` clears the map itself.
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
            source,
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

    /// Start or stop publishing this machine's audio as a second track.
    ///
    /// Published on the voice room rather than the webview's screen room: the
    /// native SDK is already connected here, every peer is already subscribed,
    /// and it lands in the same mixer as everything else — so the per-sharer
    /// volume control works on it without any new delivery path.
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
        if let Some(sa) = self.system_audio.take() {
            let _ = self.room.local_participant().unpublish_track(&sa.sid).await;
        }
        if !enabled {
            eprintln!("[voice] system audio stopped");
            // Dropping `_capture` is what stops the OS stream, and on macOS
            // that's a fire-and-forget `stopCaptureWithCompletionHandler(None)`.
            // If call audio stays bad after a share ends, a capture that never
            // actually stopped is the first thing to rule out — this line says
            // we asked.
            crate::dlog!("voice system audio stopped (capture dropped)");
            return Ok(());
        }
        if !crate::sysaudio::supported() {
            return Ok(());
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        let (fatal_tx, mut fatal_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        // The backend's own words: which Windows build, which permission, which
        // service. A generic "capture failed" would send someone hunting through
        // the wrong settings.
        let capture = crate::sysaudio::start(tx, fatal_tx, target)?;
        // Starting is only half of it — a capture that dies mid-share leaves the
        // track published and simply quiet, which the sharer cannot see. Report
        // that the same way an activation failure is reported.
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
        // No APM on this source: it is already-mixed program audio, not a
        // microphone in a room. Echo cancellation and noise suppression would
        // chew on music and game audio for no benefit.
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
                    // Music and game audio, not a voice. The defaults are tuned
                    // for speech and both of them hurt here:
                    //
                    // - the computed bitrate lands around SPEECH (24 kbit/s),
                    //   which is fine for a talking head and poor for anything
                    //   with music in it;
                    // - `dtx` (discontinuous transmission) stops sending during
                    //   quiet passages and substitutes comfort noise. On speech
                    //   that is free bandwidth; on music it is audible dropouts
                    //   every time the track goes quiet.
                    //
                    // 96 kbit/s is generous for the mono downmix we send, and
                    // trivial beside the multi-megabit video it accompanies.
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
        Ok(())
    }

    /// Start (or stop) publishing captured screen video into the screen room.
    ///
    /// This is the macOS share path in full: `sysvideo` captures, and each frame
    /// goes straight into a LiveKit video source. Nothing here touches the
    /// webview, which on macOS has no capture API to touch.
    ///
    /// A *third* connection to the screen room, and it has to be: the webview
    /// holds the bare pubkey (it still renders everyone else's share) and the
    /// audio subscriber holds `#audio`, so publishing needs `#video`. LiveKit
    /// evicts duplicate identities, which would take out one of the other two.
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
        // Already publishing this surface into this room: a repeated command is
        // the effect re-firing, not a request to restart, and restarting would
        // drop every watcher's picture for a second for nothing.
        //
        // The target is part of that identity, so switching surface mid-share
        // *does* fall through and rebuild — which is what makes the picker
        // usable while already sharing.
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
        // Everything above is platform-neutral, including the stop path, which
        // has already returned by now: with `supported()` false there can be
        // nothing publishing, so asking to stop is a no-op and not a failure.
        // Only starting is refused, and the guard above is what refuses it —
        // this arm exists because the call below cannot be compiled at all off
        // macOS. Same shape as `sysaudio::start`.
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

    /// Join (or leave) the screen-share room as an audio-only subscriber.
    ///
    /// A share captured by the *webview* publishes its audio into the
    /// `screen-…` room, where the JS client used to play it through an HTML
    /// element — which follows the app's chosen output device only where
    /// `setSinkId` exists. Subscribing to it here instead puts that audio on the
    /// same cpal mixer as voice, so one device setting governs both. The
    /// per-sharer volume control needs no changes: the mixer is where it
    /// already lived.
    async fn set_screen_audio(
        &mut self,
        room: Option<(String, String)>,
        mut state: Signal<AppState>,
    ) {
        let Some(key) = room else {
            if let Some(prev) = self.screen_audio.take() {
                prev.shutdown().await;
                // Nobody is publishing stream audio to us any more; the volume
                // control should stop claiming otherwise.
                let mut s = state.write();
                s.stream_has_audio.clear();
                s.screen_audio_joined = false;
                eprintln!("[voice] screen audio room left");
            }
            return;
        };
        // Switch rooms when the token changes rather than keeping the old one,
        // and treat a room that has since disconnected as no room at all.
        // Matching on the key alone made "already connected" the answer to every
        // later command, so a dead room could never be replaced by a live one.
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
        // Without our own identity there is nothing to exclude ourselves by, and
        // the room would feed this machine's own screen audio back to it. Refuse
        // rather than join on a guess.
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
                // Only now is the native path actually in charge — the webview
                // stands down on this, not on the token merely existing.
                state.write().screen_audio_joined = true;
                eprintln!("[voice] screen audio room joined");
                // Which path owns playback. If this says joined, the webview is
                // muted and every "still hearing it" question is about the
                // native gains; if it never appears, it's the webview element.
                crate::dlog!("voice screen_audio_joined=true (native owns stream playback)");
            }
            // Leaving this at `false` is what hands playback back to the
            // webview: it plays on the wrong device, which is the behaviour this
            // change replaces, and is strictly better than silence.
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

    /// Reconfigure libwebrtc's APM while the call is live — the suppressor
    /// handover and the AGC switch both land here.
    fn set_apm(&self, deepfilter_on: bool, agc: bool) {
        self.source
            .set_audio_options(apm_options(deepfilter_on, agc));
    }

    async fn set_muted(&mut self, muted: bool) {
        self.mic.muted.store(muted, Ordering::Relaxed);
        // Disabling the track stops audio from being sent and signals other
        // peers via the muted-track event.
        self.local_audio.rtc_track().set_enabled(!muted);
    }

    async fn shutdown(self, mut state: Signal<AppState>) {
        self.event_task.abort();
        self.meter_task.cancel();
        self.stats_task.cancel();
        // Stop capturing before leaving, so no frames are published into a
        // room that is on its way out.
        self.mic.stop();
        if let Some(sa) = self.screen_audio {
            sa.shutdown().await;
        }
        // Same reasoning as the mic: stop the OS capture before the room goes,
        // so ScreenCaptureKit isn't still delivering frames into a source whose
        // room is closing. Leaving voice always ends a share.
        if let Some(sv) = self.screen_video {
            sv.shutdown().await;
        }
        // Cleared here rather than left to the `SetScreenAudio { room: None }`
        // the bridge sends on leaving voice: that command arrives *after*
        // `Disconnect` has already taken the session, so it lands in the
        // no-session branch and clears nothing. A stale entry survives the
        // rejoin and makes the volume control look live over a stream that
        // ended. Doing it here makes it independent of message ordering.
        {
            let mut s = state.write();
            s.stream_has_audio.clear();
            s.screen_audio_joined = false;
            // Same argument for the quality readings, with an extra reason:
            // the event task was just aborted, so nothing it might have sent
            // on the way out will ever run. Left behind, a "weak connection"
            // dot would sit on someone's name after the call that produced it
            // had ended.
            s.voice_quality.clear();
            s.voice_stats.clear();
        }
        // Actually tell the SFU we are leaving.
        //
        // This used to be `drop(self.room)` with a comment claiming the drop
        // triggered a disconnect. It does not: livekit's `Room` has no `Drop`
        // impl at all, and `close()` is async precisely because leaving means
        // sending a message over the signalling channel. Without it the server
        // held the participant until its own timeout, so hanging up left you
        // visible — and audible — in the channel to everyone else.
        // Bounded: this is on the service loop, and a close that never returns
        // (dead network, half-open socket) would wedge every later command —
        // including the Connect for the channel the user is trying to join
        // next. A missed leave costs a stale participant until the SFU times
        // out; a stuck loop costs voice entirely.
        match tokio::time::timeout(ROOM_CLOSE_TIMEOUT, self.room.close()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("[voice] room close failed: {e}"),
            Err(_) => eprintln!("[voice] room close timed out, dropping anyway"),
        }
    }
}

/// Publish-only member of the screen-share room, carrying natively captured
/// screen video.
///
/// The mirror image of `ScreenAudioRoom`: that one subscribes and never
/// publishes, this one publishes and never subscribes. Both exist because the
/// webview is the wrong place for the job on the platform in question — see
/// `ActiveVoice::set_screen_video` and the `sysvideo` module docs.
struct ScreenVideoRoom {
    room: Arc<Room>,
    /// The OS capture. Dropping it stops the stream, which is why it is held
    /// here and not detached: the room outliving the capture would publish a
    /// frozen frame forever.
    ///
    /// Only where `sysvideo` has a backend. The rest of this type — the room,
    /// the key, `shutdown` — is platform-neutral and stays, so the field that
    /// holds one of these needs no gate of its own.
    #[cfg(target_os = "macos")]
    _capture: crate::sysvideo::Capture,
    /// The `(url, token, target)` this was started with, so a repeated command
    /// for the same surface is a no-op while a changed one rebuilds.
    key: (String, String, crate::sysvideo::Target),
}

impl ScreenVideoRoom {
    /// macOS only, and unconditionally so: every line past the room join is
    /// ScreenCaptureKit or CoreVideo. `set_screen_video` refuses to reach here
    /// on other platforms long before the compiler would have to.
    #[cfg(target_os = "macos")]
    async fn connect(
        url: &str,
        token: &str,
        target: crate::sysvideo::Target,
        settings: crate::sysvideo::Settings,
        state: Signal<AppState>,
    ) -> Result<Self, String> {
        // Built by mutation rather than a struct literal: `RoomOptions` is
        // `#[non_exhaustive]`.
        let mut options = RoomOptions::default();
        // We are here to publish, not to watch. The webview is already in this
        // room under our bare pubkey and renders everyone's video, including
        // shares we watch — subscribing here as well would download every
        // stream in the channel a second time.
        options.auto_subscribe = false;
        let (room, mut events) = Room::connect(url, token, options)
            .await
            .map_err(|e| format!("livekit connect: {e}"))?;
        let room = Arc::new(room);

        // `is_screencast: true` is not cosmetic — it tells libwebrtc this is
        // desktop content, which changes the encoder's degradation behaviour
        // towards keeping detail rather than motion.
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
        // The identity and sid are what a watcher has to match to find this
        // track, so they are the first thing worth knowing when a share is
        // running and nobody can see it.
        eprintln!(
            "[voice] screen video published sid={} identity={} target={:?} {}x{}@{}",
            publication.sid(),
            room.local_participant().identity().0,
            target,
            settings.width,
            settings.height,
            settings.fps,
        );

        // A capture that dies mid-share leaves the track published and the
        // picture frozen, which the sharer cannot see for themselves — the
        // exact failure `sysaudio` reports for sound, reported the same way.
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

        // Frames are pushed straight through on ScreenCaptureKit's own queue.
        // `capture_frame` hands the buffer to libwebrtc synchronously, so the
        // `Frame` — and with it the retained pixel buffer — stays alive for
        // exactly the call that needs it. See `sysvideo::FrameSink` for why this
        // is a callback and not a channel.
        let capture = crate::sysvideo::start(
            target,
            settings,
            Box::new(move |frame: crate::sysvideo::Frame| {
                // SAFETY: a live `CVPixelBuffer` with a reference count raised
                // for this call. `from_cv_pixel_buffer` consumes that reference
                // (its ObjC bridge ends in `CVPixelBufferRelease`), which is why
                // the frame hands over a retain of its own rather than the one it
                // keeps — see `Frame::into_consumable_pixel_buffer`.
                let buffer = unsafe {
                    NativeBuffer::from_cv_pixel_buffer(frame.into_consumable_pixel_buffer())
                };
                source.capture_frame(&VideoFrame {
                    rotation: VideoRotation::VideoRotation0,
                    // libwebrtc wants capture time in microseconds and uses it
                    // for pacing; the frame's own presentation timestamp would
                    // be on ScreenCaptureKit's clock, not this one.
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

        // Nothing subscribes here, but the event stream still has to be drained:
        // an unread channel is what makes livekit's room task block.
        tokio::spawn(async move { while events.recv().await.is_some() {} });

        Ok(Self {
            room,
            _capture: capture,
            key: (url.to_string(), token.to_string(), target),
        })
    }

    async fn shutdown(self) {
        // Drop the capture first, so no frame is handed to a source whose room
        // is already closing. Only exists where there is a capture to drop; the
        // room close below is what shutdown means on every platform.
        #[cfg(target_os = "macos")]
        drop(self._capture);
        match tokio::time::timeout(ROOM_CLOSE_TIMEOUT, self.room.close()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("[voice] screen video room close failed: {e}"),
            Err(_) => eprintln!("[voice] screen video room close timed out, dropping anyway"),
        }
    }
}

/// Audio-only subscriber to the screen-share room.
///
/// Publishes nothing. It exists so screen audio captured by someone else's
/// webview reaches our cpal mixer instead of theirs — see
/// `ActiveVoice::set_screen_audio`.
struct ScreenAudioRoom {
    room: Arc<Room>,
    event_task: tokio::task::JoinHandle<()>,
    /// The `(url, token)` this room was joined with, so a later command carrying
    /// a different one is recognised as a different room instead of ignored.
    key: (String, String),
    /// Cleared when the room reports a terminal disconnect. Without it a dead
    /// room still matches on `key`, so every later command would be answered
    /// with "already connected" and the audio would never come back.
    alive: Arc<AtomicBool>,
}

/// Connection-quality notices, on the same bridge as `StreamAudio` and for the
/// same reason: the event task is `tokio::spawn`ed and so must be `Send`, which
/// a Dioxus Signal is not.
enum QualityMsg {
    Set(String, ConnectionHealth),
    Drop(String),
    /// The room ended. Every reading it produced is now stale, and a stale
    /// "weak connection" dot left on a name after the call is worse than none.
    Clear,
}

/// What the screen/voice event tasks tell the UI about stream audio. An enum
/// rather than `(String, bool)` because a dying room has to say "all of it",
/// and encoding that as an empty identity would be a sentinel nobody expects.
enum StreamAudio {
    Present(String),
    Gone(String),
    /// The room itself went away: everything it reported is stale.
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
        // Built by mutation rather than a struct literal: `RoomOptions` is
        // `#[non_exhaustive]`.
        let mut options = RoomOptions::default();
        // Opt in per track. Subscribing to everything would pull the screen
        // *video* down as well — the exact cost the separate screen room exists
        // to keep away from native peers.
        options.auto_subscribe = false;
        let (room, mut events) = Room::connect(url, token, options)
            .await
            .map_err(|e| format!("livekit connect: {e}"))?;
        let room = Arc::new(room);

        // Same bridge shape as the voice room's: the event task is `spawn`ed and
        // so must be Send, which a Dioxus Signal is not.
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
                            // The other half of the flap. `joined=true` was
                            // already logged; without this the log shows the
                            // room arriving and the webview taking playback
                            // back 50ms later with nothing in between.
                            crate::dlog!(
                                "voice screen_audio RoomGone -> joined=false (playback back to webview)"
                            );
                            s.screen_audio_joined = false;
                        }
                    }
                }
            });
        }

        // Anyone already sharing when we arrive. We connect *because* someone is
        // sharing, so this is the common case, not the edge one — `TrackPublished`
        // only fires for publications that happen after the join.
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
                            // `is_stream: true` — this is program audio, so
                            // it takes the stream gains, not the voice ones.
                            tokio::spawn(consume_remote_track(
                                stream,
                                mixer.clone(),
                                identity,
                                true,
                            ));
                        }
                        // Filtered by source, like the voice room's sibling
                        // handler: this room carries the sharer's *video* too,
                        // and its unpublish would otherwise retract a claim
                        // about audio that is still playing.
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
                            // Nothing else notices this. Left alone, the room
                            // stays `Some` with a matching key, every later
                            // command is answered "already connected", and the
                            // stream is silently gone for good — with the
                            // webview fallback already stood down.
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
        // Same reasoning as `ActiveVoice::shutdown`: `Room` has no `Drop` impl,
        // so leaving without `close()` would hold this subscriber on the SFU
        // until its own timeout instead of freeing the slot immediately.
        match tokio::time::timeout(ROOM_CLOSE_TIMEOUT, self.room.close()).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("[voice] screen audio room close failed: {e}"),
            Err(_) => eprintln!("[voice] screen audio room close timed out, dropping anyway"),
        }
    }
}

/// Screen *audio* from somebody else. Our own share is excluded deliberately:
/// the webview publishes it under our bare pubkey while this connection holds a
/// suffixed identity, so LiveKit sees two different participants and would hand
/// our own machine's sound straight back to us.
fn wanted(source: &TrackSource, publisher: &str, self_pubkey: &str) -> bool {
    *source == TrackSource::ScreenshareAudio && publisher != self_pubkey
}

// ---------------------------------------------------------------------------
// Publish path: mic hop -> denoise -> transmit gate -> NativeAudioSource
// ---------------------------------------------------------------------------

/// Denoise and gate captured hops, on a thread of its own.
///
/// The gate is what makes the sensitivity slider mean something. Before, the
/// threshold only tinted a dot in the UI while every frame — breathing, fans,
/// the neighbour's drill — went out on the wire regardless. Now a frame below
/// the threshold is simply not transmitted, which is what "the microphone is
/// not active" has to mean for the control to be worth having.
///
/// This is a plain OS thread, not a task, for two reasons. `DfTract` holds `Rc`
/// internally so it is `!Send` and cannot be held across an `.await` in a
/// spawned task at all; and the work is a steady 150µs of CPU every 10ms, which
/// is exactly what you don't want sharing a runtime worker with the network I/O
/// that has to keep the call up. It exits when `MicCapture` drops the sender.
fn denoise_gate_loop(
    mut frame_rx: UnboundedReceiver<Vec<f32>>,
    out_tx: UnboundedSender<Vec<i16>>,
    controls: AudioControls,
    meter: Arc<MicMeter>,
    muted: Arc<AtomicBool>,
) {
    let mut denoiser: Option<crate::denoise::Denoiser> = None;
    let mut gate = GateState::default();
    let mut gated = 0u64;

    while let Some(mut samples) = frame_rx.blocking_recv() {
        // Input gain first, before anything measures this hop. That ordering is
        // the whole contract of the slider: the VU bar and the gate threshold
        // are both in post-gain terms, so the bar shows what listeners hear and
        // the marker sits where the gate really opens. The cost is that the two
        // sliders interact — louder input also trips the gate more easily.
        //
        // Clamped here rather than at the end: a boosted hop that would clip is
        // better limited before the model and the peak measurement see it, so
        // neither is fed samples outside ±1.0.
        let gain_pct = controls.mic_gain_pct.load(Ordering::Relaxed);
        if gain_pct != 100 {
            let g = gain_pct as f32 / 100.0;
            for s in samples.iter_mut() {
                *s = (*s * g).clamp(-1.0, 1.0);
            }
        }

        // Muted short-circuits everything except metering: the VU bar should
        // still move so a user who forgot they're muted can see the mic is
        // fine. No point running the model on audio nobody will hear.
        if muted.load(Ordering::Relaxed) {
            meter.bump_peak(peak_fixed(&samples));
            meter.open.store(false, Ordering::Relaxed);
            // The gate sees none of this, so it must not remember what came
            // before it either. See `GateState`.
            gate.silence();
            continue;
        }

        // Noise suppression first: the gate should judge the signal the
        // listener will actually hear, so a fan the model removes shouldn't be
        // holding the gate open.
        if controls.denoise.load(Ordering::Relaxed) {
            if denoiser.is_none() {
                match crate::denoise::Denoiser::new() {
                    Ok(d) => {
                        eprintln!("[voice] DeepFilterNet loaded, noise cancellation active");
                        denoiser = Some(d);
                        // Loading took ~200ms, during which the capture callback
                        // kept queueing hops. Playing catch-up would put that
                        // 200ms into the call as permanent extra latency, so
                        // throw the backlog away and resume at real time.
                        let mut dropped = 0;
                        while frame_rx.try_recv().is_ok() {
                            dropped += 1;
                        }
                        if dropped > 0 {
                            eprintln!("[voice] dropped {dropped} hops queued during model load");
                        }
                        // Same reason as the mute path: hops the gate never
                        // saw, and the ones it did see were not denoised, so
                        // its envelope is on the wrong scale for what follows.
                        gate.silence();
                    }
                    Err(e) => {
                        eprintln!("[voice] noise cancellation unavailable: {e}");
                        // Don't retry a model that won't load, 100 times a second.
                        controls.denoise.store(false, Ordering::Relaxed);
                    }
                }
            }
            if let Some(d) = denoiser.as_mut() {
                d.process_hop(&mut samples);
            }
        } else if denoiser.is_some() {
            // Release the model when switched off. Its state would be stale on
            // re-enable anyway: the GRUs would resume from whatever the audio
            // looked like at the moment the user turned it off.
            eprintln!("[voice] noise cancellation off, releasing model");
            denoiser = None;
        }

        // Peak of the hop, on the same ×1000 fixed-point scale as the VU bar
        // and the threshold slider — so the marker the user drags sits exactly
        // where the gate opens.
        let peak = peak_fixed(&samples);
        meter.bump_peak(peak);

        let action = gate.step(peak, controls.threshold.load(Ordering::Relaxed));
        // A ramping-out hop is still sent, but the gate is shut — the speaking
        // indicator should say so on the same frame the fade starts.
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
                continue;
            }
        }

        let data: Vec<i16> = samples
            .iter()
            // No dither here: these frames go straight into Opus. Dither is for
            // the final quantization to an output device — feeding noise to a
            // lossy encoder only costs bitrate.
            .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .collect();
        if out_tx.send(data).is_err() {
            break;
        }
    }
    eprintln!("[voice] denoise/gate thread ended ({gated} frames gated)");
}

/// What the gate decided about the hop just measured.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GateAction {
    /// Send it as it is — the gate was already open and stays open.
    Pass,
    /// Send it faded in: the gate just opened.
    RampIn,
    /// Send it faded out: the gate just closed, and a phrase should end on
    /// zero rather than on whatever sample the cut landed on.
    RampOut,
    /// Send nothing.
    Drop,
}

/// The transmit gate's memory between hops.
///
/// A struct because the three fields have to move together, which is exactly
/// what the first version of this got wrong: it reset the hangover counter when
/// the mic was muted and left the envelope and the open flag alone. Unmuting
/// into a quiet room then resumed from the level the user had last been
/// *speaking* at — the envelope decays 25% a hop, so a peak of 900 is still
/// over a threshold of 21 fifteen hops later — and the gate passed a third of a
/// second of room noise before deciding it was silence. Review of #46 traced
/// it. Anything that interrupts the audio the gate is watching has to say so
/// through `silence`, not by poking one field.
#[derive(Default)]
struct GateState {
    /// Frames left before the gate closes. Non-zero = currently transmitting.
    hangover: u32,
    /// Released peak, so a single dip cannot start the hangover counting down.
    envelope: i32,
    /// Whether the previous hop was sent — what the ramps key off.
    was_open: bool,
}

impl GateState {
    /// Forget everything, because the next hop does not continue the last one.
    fn silence(&mut self) {
        *self = Self::default();
    }

    /// Judge one hop's peak, on the ×1000 fixed-point scale the slider uses.
    fn step(&mut self, peak: i32, open_at: i32) -> GateAction {
        self.envelope = peak.max(self.envelope * GATE_ENVELOPE_DECAY_PCT / 100);
        // Once open, hold to a lower bar than it took to get there.
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

/// Fade a hop in or out over `GATE_RAMP_SAMPLES`, in place.
///
/// Applied to the hop the gate changes state on: rising, so an onset is not
/// asserted at full level from a standing start; falling, so the last hop of a
/// phrase ends on zero instead of on whatever sample the cut happened to land
/// on. A hop shorter than the ramp uses its whole length.
fn ramp(samples: &mut [f32], rising: bool) {
    let n = GATE_RAMP_SAMPLES.min(samples.len());
    if n == 0 {
        return;
    }
    for (i, s) in samples.iter_mut().take(n).enumerate() {
        let g = i as f32 / n as f32;
        *s *= if rising { g } else { 1.0 - g };
    }
    // Everything past the ramp is silence on the way out, and untouched signal
    // on the way in.
    if !rising {
        samples[n..].fill(0.0);
    }
}

/// Map a peak (×1000 fixed point) to a 0..=100 meter position on a dB scale.
///
/// Amplitude is linear but hearing is not, which is why the old linear bar
/// always looked broken: ordinary speech peaks around 0.03–0.3, so a linear
/// meter sat between 3% and 30% and the default threshold rendered as "2%" —
/// numbers that look like something is wrong when the audio is perfectly fine.
/// On a dBFS scale (-60 dB at the bottom, 0 dBFS at the top) that same speech
/// lands between 50% and 90%, which is where a meter is meant to sit.
pub fn peak_to_meter_pct(peak_fixed: u32) -> u32 {
    if peak_fixed == 0 {
        return 0;
    }
    let db = 20.0 * (peak_fixed as f64 / 1000.0).log10();
    (((db - METER_FLOOR_DB) / -METER_FLOOR_DB) * 100.0).clamp(0.0, 100.0) as u32
}

/// Inverse of `peak_to_meter_pct`, so the sensitivity slider can move in even
/// dB steps while the stored value stays the ×1000 peak the gate compares.
pub fn meter_pct_to_peak(pct: u32) -> u32 {
    let db = (pct.min(100) as f64 / 100.0) * -METER_FLOOR_DB + METER_FLOOR_DB;
    let amp = 10f64.powf(db / 20.0);
    ((amp * 1000.0).round() as i64).clamp(1, 1000) as u32
}

/// A peak as a dBFS label for the UI ("-32 dB").
pub fn peak_to_db_label(peak_fixed: u32) -> String {
    if peak_fixed == 0 {
        return "−∞".into();
    }
    format!("{:.0} dB", 20.0 * (peak_fixed as f64 / 1000.0).log10())
}

/// Bottom of the meter. -60 dBFS is below the noise floor of any usable mic,
/// so nothing interesting is hidden underneath it.
const METER_FLOOR_DB: f64 = -60.0;

/// A published system-audio track and the capture feeding it.
struct SystemAudioTrack {
    sid: livekit::prelude::TrackSid,
    /// Dropping this stops the OS-level capture.
    _capture: crate::sysaudio::Capture,
    task: tokio::task::JoinHandle<()>,
    /// The selected surface this capture follows. `None` on Windows, whose
    /// native path is whole-machine loopback after a webview monitor pick.
    target: Option<crate::sysvideo::Target>,
}

impl Drop for SystemAudioTrack {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Forward captured PCM frames to a libwebrtc source. Same shape as the mic's
/// publish loop, without the gate — program audio isn't voice-activated.
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

/// Peak absolute sample as ×1000 fixed point — the scale the threshold slider
/// and the VU bar both speak.
fn peak_fixed(samples: &[f32]) -> i32 {
    (samples.iter().fold(0.0f32, |m, s| m.max(s.abs())) * 1_000.0) as i32
}

/// Hand gated frames to libwebrtc. `capture_frame` is async, which is the only
/// reason this is a task at all — everything expensive already happened on the
/// denoise/gate thread.
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
    /// Raise the stored peak to `value` if it's louder. Reset on read.
    fn bump_peak(&self, value: i32) {
        let mut current = self.peak.load(Ordering::Relaxed);
        while value > current {
            match self.peak.compare_exchange_weak(
                current,
                value,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(c) => current = c,
            }
        }
    }
}

/// Mirror the mic meter into `AppState` for the VU bar and the speaking dot.
///
/// Both come from the *same* gate the publish path uses, so the indicator can't
/// disagree with what is actually being transmitted — which it did when the UI
/// ran its own copy of the threshold comparison.
fn spawn_meter_task(mut state: Signal<AppState>, meter: Arc<MicMeter>) -> Task {
    dioxus::prelude::spawn(async move {
        let mut last_speaking = false;
        let mut last_level = u32::MAX;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let level = meter.peak.swap(0, Ordering::Relaxed).clamp(0, 1000) as u32;
            let speaking = meter.open.load(Ordering::Relaxed);
            // Only write on change: this ticks 6x a second and every write
            // re-renders the app.
            if level != last_level {
                last_level = level;
                state.write().mic_level = level;
            }
            if speaking != last_speaking {
                last_speaking = speaking;
                state.write().voice.speaking = speaking;
            }
        }
    })
}

/// Poll the peer connection for the numbers behind "it sounds bad": loss,
/// jitter, and how much delay the decoder is holding.
///
/// Gated rather than started and stopped. Spawning the task once and having it
/// check a flag costs one atomic load a second while the panel is closed, and
/// avoids threading a `Task` handle through the command loop to be cancelled
/// and respawned — the flag already lives on `AudioControls`, which survives
/// the session rebuilds a device change causes.
fn spawn_stats_task(
    mut state: Signal<AppState>,
    room: Arc<Room>,
    local_audio: LocalAudioTrack,
    self_pubkey: Option<String>,
    enabled: Arc<AtomicBool>,
) -> Task {
    dioxus::prelude::spawn(async move {
        let mut was_enabled = false;
        // Previous reading of our own send counters, for the rates below.
        let mut prev_out: Option<(u64, u64, Instant)> = None;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;

            if !enabled.load(Ordering::Relaxed) {
                // Clear once on the way down. Left populated, a reopened panel
                // would show numbers from minutes ago as if they were live.
                if was_enabled {
                    was_enabled = false;
                    // The previous reading goes with them. Kept, the first tick
                    // after reopening would spread its delta over however long
                    // the panel was shut and report a rate nobody sent.
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
                    // A sharer publishes their system audio into *this* room
                    // too, under the same identity. Taking whichever track came
                    // out of the map first would sometimes report the screen
                    // audio's numbers on the person's voice row.
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
                        // A once-a-second time series, which the panel is not.
                        // The panel keeps the newest reading, and on a path
                        // that misbehaves in bursts — any Wi-Fi link — the
                        // interesting part is the shape between readings, gone
                        // by the time anyone looks.
                        //
                        // `rx` and `lost` are logged raw as well as through
                        // `loss_pct`, because they are cumulative counters: the
                        // percentage is a session average and cannot show a
                        // ten-second spike at all. Differencing consecutive
                        // lines gives the per-interval loss the average hides.
                        // Logged from the same `inbound_stats` the panel gets,
                        // rather than re-deriving the three formulas here: two
                        // copies three lines apart would drift the first time
                        // one of them is corrected, and the log is the copy
                        // nobody would notice going wrong.
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

            // Our own row is the other direction: what we are sending, which is
            // also the only place the bitrate setting can be seen taking effect.
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
                // Split once and used by both, the way `target_kbps` already
                // is. Nothing here can drift the way the inbound formulas
                // could — `rates` is a single source and these only pick a
                // field off it — but the log and the panel showing the same
                // number should be visible in the code rather than left to two
                // call sites happening to agree.
                let bitrate_kbps = rates.map(|(_, kbit)| kbit);
                let packets_per_sec = rates.map(|(pkt, _)| pkt);
                // The send side of the same series. `target` is what the
                // encoder was told to aim for, so the pair is what says whether
                // congestion control has pulled the real rate down under it —
                // which is the first thing a bad link does and the panel only
                // ever shows as a number that looks a bit low.
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

            // Same discipline as the meter task: this ticks once a second and
            // every write re-renders.
            if state.peek().voice_stats != next {
                state.write().voice_stats = next;
            }
        }
    })
}

/// Reduce a raw inbound report to the four numbers the panel shows.
fn inbound_stats(s: &livekit::webrtc::stats::InboundRtpStats) -> TrackStats {
    let received = s.received.packets_received;
    let lost = s.received.packets_lost.max(0) as u64;
    let total = received + lost;
    // `jitterBufferTargetDelay` is a running sum in seconds, one addend per
    // emitted sample, so it only means anything divided by that count.
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

/// Turn two readings of libwebrtc's monotonic send counters into
/// `(packets_per_sec, kbit_s)`.
///
/// Divided by the time actually elapsed rather than by the poll interval: the
/// loop sleeps a second and *then* awaits one `get_stats()` per participant, so
/// the real gap is a second plus however long that walk took. Same reason the
/// Windows capture path reads its clock instead of counting iterations — it is
/// self-correcting, which also means a tick where the read failed costs
/// accuracy on nothing.
///
/// `None` until there is a previous reading to subtract from, and `None` again
/// if a counter went backwards — which is what a renegotiated ssrc looks like
/// from here, and which would otherwise wrap into an enormous number. Reporting
/// zero instead would be indistinguishable from sending nothing.
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

// ---------------------------------------------------------------------------
// Microphone capture: cpal -> mono 48kHz hops -> publish task
// ---------------------------------------------------------------------------

struct MicCapture {
    _stream: cpal::Stream,
    /// Shared with the DSP thread, which is where muted frames are dropped
    /// (after metering, so the VU bar keeps working while muted).
    muted: Arc<AtomicBool>,
}

impl MicCapture {
    fn start(
        frame_tx: tokio::sync::mpsc::UnboundedSender<Vec<f32>>,
        state: Signal<AppState>,
        muted: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let host = cpal::default_host();
        // Prefer user-selected device (by name) if present in AppState.
        let selected = state.read().selected_input_device.clone();
        let device = if let Some(sel_name) = selected {
            // Try to find a device whose name matches the selected name.
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

        let raw_peak = Arc::new(AtomicI32::new(0));
        let raw_peak_cb = raw_peak.clone();
        let frames_pushed = Arc::new(AtomicU64::new(0));
        let frames_pushed_cb = frames_pushed.clone();

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
                // Reusable buffers for the realtime callback — see `forward_mic`.
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

        // Heartbeat — reports loudest raw sample seen since last tick.
        let peak_log = raw_peak.clone();
        let frames_log = frames_pushed.clone();
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
                prev_frames = f;
            }
        });
        // Speaking detection and the VU bar now live on the publish task (see
        // `publish_loop` / `spawn_meter_task`), which is the only place that
        // sees the exact frames being transmitted. Running a second, separate
        // threshold comparison here is what let the indicator and the actual
        // audio disagree.
        Ok(Self {
            _stream: stream,
            muted,
        })
    }

    fn stop(self) {
        drop(self._stream);
    }
}

/// Downmix, resample and cut the device's callback buffer into exact 10ms
/// hops for the publish task. Mute is *not* checked here: the publish task
/// still wants muted frames so the VU bar keeps moving (a muted user watching
/// a dead meter can't tell a mute from a broken mic), and it drops them there.
///
/// `mono_buf` and `resampled_buf` are caller-provided reusable buffers —
/// allocating inside cpal's realtime callback can trigger a page fault or
/// allocator lock and cause an xrun (dropout). Both are cleared at entry.
fn forward_mic(
    frame_tx: &tokio::sync::mpsc::UnboundedSender<Vec<f32>>,
    samples: &[f32],
    device_channels: u32,
    accum: &Arc<Mutex<Vec<f32>>>,
    resampler: &Arc<Mutex<Option<AudioResampler>>>,
    mono_buf: &mut Vec<f32>,
    resampled_buf: &mut Vec<f32>,
) -> usize {
    // Downmix to mono reusing the caller's buffer instead of allocating.
    mono_buf.clear();
    mono_buf.extend(
        samples
            .chunks(device_channels as usize)
            .map(|c| c.iter().copied().sum::<f32>() / c.len() as f32),
    );
    // High-quality resampling via rubato (FFT + anti-aliasing). Falls back to
    // passthrough if no resampler is needed (device already at SAMPLE_RATE).
    // Uses `process_into` (realtime-safe, no Vec allocation) when resampling,
    // or copies directly into `resampled_buf` when passing through.
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
        // Stays f32 all the way to the publish task: DeepFilterNet wants
        // floats in [-1, 1], and quantizing here just to widen it again would
        // throw away resolution before the model ever sees the signal.
        //
        // This drain+collect is the one remaining allocation per 10ms hop: it
        // hands ownership of the chunk to the channel sender. Using a rotating
        // pool of pre-allocated Vecs would remove it, but the channel API
        // (`send(Vec<f32>)`) takes ownership, so the Vec must exist. One alloc
        // per 10ms (100/s) is far less costly than the 2-3 per callback we had
        // before, and it happens on the DSP side, not under the cpal lock.
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

    /// Realtime-safe variant of resampling: writes into a caller-provided
    /// buffer instead of allocating a new Vec per call. Used inside cpal's
    /// audio callback, where a heap allocation can trigger a page fault or
    /// allocator lock and cause an xrun (dropout).
    ///
    /// Feeds the internal resampler in chunks of `RESAMPLER_CHUNK` frames;
    /// leftover input is kept for the next call.
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
    buffers: std::collections::HashMap<u64, TrackBuf>,
    next_id: u64,
}

/// One remote participant's jitter buffer plus the gain we play them back at.
struct TrackBuf {
    samples: std::collections::VecDeque<f32>,
    /// LiveKit identity of whoever is speaking — the user's pubkey. Keyed by
    /// identity rather than track id so a participant who leaves and rejoins
    /// (new track, new id) comes back at the level you last set for them.
    identity: String,
    /// Gain refreshed once per output callback from the shared map, so the
    /// per-sample loop never touches a HashMap or a second lock.
    gain: f32,
    /// Screen-share audio rather than someone's voice. The two are mixed the
    /// same way but take their gain from different maps, so turning a stream
    /// down doesn't quieten the person talking over it.
    is_stream: bool,
}

#[derive(Clone)]
struct PlaybackHandle {
    tracks: Arc<Mutex<MixerTracks>>,
    device_rate: u32,
    /// Shared with the UI via `AudioControls` — see `VoiceCmd::SetUserVolume`.
    gains: Arc<Mutex<HashMap<String, f32>>>,
    stream_gains: Arc<Mutex<HashMap<String, f32>>>,
}

impl PlaybackHandle {
    /// Claim a jitter buffer for one remote track. The id is opaque; the caller
    /// hands it back to `push`/`remove_track`.
    fn add_track(&self, identity: String, is_stream: bool) -> u64 {
        // Read the gain and release that lock BEFORE taking `tracks`: the
        // output callback locks them the other way round (tracks, then gains
        // in `refresh_gains`), and holding both here in the opposite order is
        // a deadlock against the audio thread.
        let gain = if is_stream {
            // Silent until the viewer opens the watch window.
            self.stream_gains
                .lock()
                .get(&identity)
                .copied()
                .unwrap_or(0.0)
        } else {
            self.gains.lock().get(&identity).copied().unwrap_or(1.0)
        };
        // The identity a stream track is filed under is the crux of the
        // "audio keeps playing" bug: the gain map is keyed by bare pubkey, and
        // a screen-room peer arrives as `{pubkey}#audio`. If those disagree the
        // lookup misses and this track answers to no slider.
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
        // "Rejoin a share and hear nothing" needs exactly this: if the stream
        // track leaves the mixer when the viewer closes the window and no
        // matching `add_track` follows on reopen, then no gain the UI sends can
        // make it audible — there is nothing left to apply it to.
        if let Some(t) = removed {
            crate::dlog!(
                "mixer remove_track identity={} is_stream={}",
                t.identity,
                t.is_stream
            );
        }
    }
}

/// Copy the user's chosen volumes onto the live tracks. Called once per output
/// callback: a HashMap lookup per *sample* would be absurd, and the volume only
/// needs to be as fresh as one buffer (a few ms).
fn refresh_gains(
    tracks: &mut MixerTracks,
    gains: &Arc<Mutex<HashMap<String, f32>>>,
    stream_gains: &Arc<Mutex<HashMap<String, f32>>>,
    deafened: &Arc<AtomicBool>,
) {
    if tracks.buffers.is_empty() {
        return;
    }
    // Deafen overrides both maps and returns before either lock is taken: the
    // user's own levels have to survive it, and there is no reason for the
    // realtime callback to contend for a map it is about to ignore. Only the
    // gain is touched — the per-sample loop still pops every track, which is
    // what keeps a 200ms burst from escaping on undeafen.
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
            // Absent means "not watching", which is silence — the opposite
            // default from voice, where absent means normal volume.
            sg.get(&track.identity).copied().unwrap_or(0.0)
        } else {
            g.get(&track.identity).copied().unwrap_or(1.0)
        };
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
        if counter.is_multiple_of(32) {
            buf.pop_front();
        }
        buf.pop_front().unwrap_or(0.0)
    } else if len < underrun {
        // Too empty — repeat every 64th sample to stretch it.
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
        // Try to honour user-selected output device name if present.
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

        // VecDeque for O(1) pop_front — Vec::remove(0) is O(n) and was
        // starving the audio callback at our buffer sizes.
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
        // Only one of these callbacks is ever built, but both closures must be
        // constructible, so each needs its own clone of the gain map.
        let gains_f32 = controls.gains.clone();
        let gains_i16 = controls.gains.clone();
        let stream_gains_f32 = controls.stream_gains.clone();
        let stream_gains_i16 = controls.stream_gains.clone();
        let deafened_f32 = controls.deafened.clone();
        let deafened_i16 = controls.deafened.clone();
        // Dither PRNG state. Moved into the i16 callback so it advances across
        // invocations (a per-callback reseed would emit the same noise pattern
        // every buffer, which is audible as a tone).
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
                    // Drift compensation thresholds (in samples at device rate).
                    // The overrun mark must stay under the producer-side cap
                    // (PLAYBACK_CAP_DIVISOR, ~200ms) or it can never be reached
                    // and the branch is dead.
                    let overrun_threshold = (device_rate_cb as f64 * DRIFT_OVERRUN_SECS) as usize;
                    let underrun_threshold = (device_rate_cb as f64 * DRIFT_UNDERRUN_SECS) as usize;
                    let mut counter = drift_counter_cb.load(Ordering::Relaxed);
                    for frame in data.chunks_mut(device_channels) {
                        counter = counter.wrapping_add(1);
                        // Sum every active track. Each keeps its own depth, so
                        // drift is corrected per speaker rather than globally.
                        //
                        // Per-track soft knee before summing: a single loud
                        // speaker shouldn't be able to saturate the bus before
                        // the global limiter even sees the other tracks. The
                        // knee is gentle (15% gain reduction at full scale) so
                        // a solo speaker at unity gain is effectively untouched.
                        let mut acc = 0.0f32;
                        for track in tracks.buffers.values_mut() {
                            // Always pop, even at zero gain: a locally-muted
                            // participant whose buffer stops draining would
                            // overflow and then blast 200ms of stale audio the
                            // moment they're unmuted.
                            let s = track.gain
                                * pop_drift_compensated(
                                    &mut track.samples,
                                    counter,
                                    overrun_threshold,
                                    underrun_threshold,
                                );
                            // Soft knee: compress gradually as the sample
                            // approaches full scale, instead of a hard clip.
                            acc += s * (1.0 - 0.15 * s.abs().min(1.0));
                        }
                        // Transparent limiter: identity below 1.0 (no
                        // compression of normal-level speech), soft clip above.
                        // tanh compressed everything — even a single speaker at
                        // 0.5 lost ~2% to the curve — and several simultaneous
                        // speakers at unity summed to 3.0 which tanh squashed
                        // to 0.995, flat and slightly distorted.
                        let sample = if acc.abs() > 1.0 {
                            // 2 - 1/|x|: continuous at ±1, asymptotes to ±2.
                            // A sum of 3.0 maps to 1.67 — loud but not flat.
                            acc.signum() * (2.0 - 1.0 / acc.abs())
                        } else {
                            acc
                        };
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
                // Dither RNG state, owned by the callback — no shared state, no
                // lock, no thread-local lookup on the realtime thread.
                move |data: &mut [i16], _| {
                    cb_counter_cb.fetch_add(1, Ordering::Relaxed);
                    let mut tracks = tracks_cb.lock();
                    refresh_gains(&mut tracks, &gains_i16, &stream_gains_i16, &deafened_i16);
                    let overrun_threshold = (device_rate_cb as f64 * DRIFT_OVERRUN_SECS) as usize;
                    let underrun_threshold = (device_rate_cb as f64 * DRIFT_UNDERRUN_SECS) as usize;
                    let mut counter = drift_counter_cb.load(Ordering::Relaxed);
                    for frame in data.chunks_mut(device_channels) {
                        counter = counter.wrapping_add(1);
                        // Per-track soft knee + transparent limiter — see the
                        // f32 callback above for the full rationale.
                        let mut acc = 0.0f32;
                        for track in tracks.buffers.values_mut() {
                            let s = track.gain
                                * pop_drift_compensated(
                                    &mut track.samples,
                                    counter,
                                    overrun_threshold,
                                    underrun_threshold,
                                );
                            acc += s * (1.0 - 0.15 * s.abs().min(1.0));
                        }
                        let sample = if acc.abs() > 1.0 {
                            acc.signum() * (2.0 - 1.0 / acc.abs())
                        } else {
                            acc
                        };
                        // TPDF dither on the f32→i16 conversion.
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
    // Per-track state. The resampler keeps FFT overlap and a partial-chunk
    // accumulator across calls, so it belongs to exactly one stream — sharing
    // one between participants corrupts it for everyone.
    let mut resampler = AudioResampler::new(SAMPLE_RATE, handle.device_rate);
    // Reusable buffers — avoids allocating a fresh Vec for every 10ms frame
    // received from the network. Not as critical as the cpal callback (this
    // runs on a tokio task, not a realtime thread) but still ~100 allocs/s
    // per remote participant that the allocator doesn't need to handle.
    let mut f32_buf: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
    let mut resampled_buf: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
    // The lines below are where a silent participant is told from a working
    // one, and with several tracks live "something is near-silent" is not an
    // answer to that. The suffix is kept because it is what separates someone's
    // microphone from their screen's audio — see `livekit::screen_audio_identity`.
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
            // No per-track limiting here — the callback limits the summed mix
            // instead, which is the only place that can see whether several
            // people talking at once are driving the output past full scale.
            f32_buf.clear();
            f32_buf.extend(frame.data.iter().map(|s| *s as f32 / i16::MAX as f32));
            // High-quality resampling via rubato (48kHz → device_rate).
            // Uses the realtime-safe `process_into` variant.
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
    // Drop this track's jitter buffer, otherwise a participant who left keeps
    // contributing silence to the mix (and leaks a buffer per rejoin).
    handle.remove_track(track_id);
    eprintln!("[voice] remote-track {who} stream ended after {frames} frames");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The complaint that prompted the ramp and the hysteresis: with noise
    /// suppression on, a listener heard the speaker's level swelling and
    /// dropping. The gate reads the denoised hop, the model may pull one down
    /// by 30 dB, and one threshold for both directions turns that into
    /// chatter.
    #[test]
    fn the_gate_holds_below_the_level_it_opened_at() {
        let open_at = 21; // the reporter's own sensitivity setting
        let hold_at = open_at * GATE_CLOSE_RATIO_PCT / 100;
        assert!(
            hold_at < open_at,
            "hysteresis has to hold below the opening"
        );

        // A denoised word tail: too quiet to have opened the gate, loud enough
        // that cutting the speaker off mid-word would be wrong.
        let tail = 14;
        assert!(tail < open_at, "precondition: would not open the gate");
        assert!(
            tail > hold_at,
            "a tail between the two bars is exactly what the hysteresis is for"
        );
    }

    /// The envelope only ever makes the gate more permissive — otherwise the
    /// threshold marker would stop marking where the gate opens.
    #[test]
    fn the_gate_envelope_never_reads_below_the_hop() {
        let mut env = 0i32;
        for peak in [0, 5, 900, 3, 0, 0, 40] {
            env = peak.max(env * GATE_ENVELOPE_DECAY_PCT / 100);
            assert!(env >= peak, "envelope {env} read under its own hop {peak}");
        }
        // And it decays rather than latching: 900 must not still be there.
        assert!(env < 900, "envelope latched at the loudest hop it ever saw");
    }

    /// Review of #46, traced by hand before it was written down here: the first
    /// version of the hysteresis reset the hangover counter on mute and left
    /// the envelope and the open flag standing, so unmuting into a quiet room
    /// resumed from the level the user had last been speaking at.
    ///
    /// The arithmetic is why it lasted long enough to hear. The envelope keeps
    /// 75% a hop, so a peak of 900 is still above a threshold of 21 fifteen
    /// hops later, and the hangover it keeps refreshing is another 30 — about a
    /// third of a second of room noise, transmitted, right after the user
    /// un-muted expecting to be heard only when they speak.
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

    /// Without the reset, the same trace passes room noise for long enough to
    /// be the bug this file's hysteresis was added to fix, in reverse.
    #[test]
    fn a_gate_that_kept_its_envelope_would_hold_open_for_a_third_of_a_second() {
        let open_at = 21;
        let mut gate = GateState::default();
        for _ in 0..5 {
            gate.step(900, open_at);
        }
        // The mute path as it was: hangover cleared, memory kept.
        gate.hangover = 0;

        let held = (0..100)
            .take_while(|_| gate.step(5, open_at) != GateAction::Drop)
            .count();
        assert!(
            held > 30,
            "the trace in the review depends on this being long, not a hop or two; got {held}"
        );
    }

    /// The ramps are what the state machine exists to place, so it has to place
    /// exactly one of each around a phrase.
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

    /// A fade that does not reach zero is the click it was added to remove.
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

        // A hop shorter than the ramp must not panic on the slice.
        let mut short = vec![1.0f32; GATE_RAMP_SAMPLES / 2];
        ramp(&mut short, false);
        assert_eq!(short[0], 1.0);
    }

    /// The panel's own row is two deltas, and getting the arithmetic wrong is
    /// the failure that looks plausible: a rate is believable at any value, so
    /// nothing about the UI would give it away.
    #[test]
    fn outbound_rates_are_per_second_and_need_two_readings() {
        let t0 = Instant::now();
        // Nothing to subtract from yet.
        assert_eq!(outbound_rates(None, 50, 6_000, t0), None);

        // One second of Opus at the default 20ms frame: 50 packets, and 6000
        // bytes is exactly the 48 kbit/s the client defaults to.
        let t1 = t0 + std::time::Duration::from_secs(1);
        assert_eq!(
            outbound_rates(Some((0, 0, t0)), 50, 6_000, t1),
            Some((50, 48))
        );

        // The same delta over twice the gap is half the rate — this is what
        // proves the elapsed time is divided by rather than assumed.
        let t2 = t0 + std::time::Duration::from_secs(2);
        assert_eq!(
            outbound_rates(Some((0, 0, t0)), 50, 6_000, t2),
            Some((25, 24))
        );
    }

    /// A renegotiated ssrc restarts the counters. Subtracting anyway would wrap
    /// into an enormous number presented as a measurement.
    #[test]
    fn outbound_rates_refuse_a_counter_that_went_backwards() {
        let t0 = Instant::now();
        let t1 = t0 + std::time::Duration::from_secs(1);
        assert_eq!(outbound_rates(Some((900, 0, t0)), 10, 6_000, t1), None);
        assert_eq!(outbound_rates(Some((0, 90_000, t0)), 50, 600, t1), None);
        // A gap of zero has nothing to divide by either.
        assert_eq!(outbound_rates(Some((0, 0, t0)), 50, 6_000, t0), None);
    }

    /// The complaint that prompted the dB scale: ordinary speech should not
    /// read as a nearly-empty meter.
    #[test]
    fn speech_sits_in_the_middle_of_the_meter() {
        // -30 dBFS (peak 0.032) is a comfortable speaking level.
        let pct = peak_to_meter_pct(32);
        assert!((45..=55).contains(&pct), "quiet speech read as {pct}%");
        // A loud peak should be near the top, not merely a third of the way up.
        assert!(peak_to_meter_pct(700) > 90);
    }

    #[test]
    fn meter_endpoints_behave() {
        assert_eq!(peak_to_meter_pct(0), 0);
        assert_eq!(peak_to_meter_pct(1000), 100);
        // Below the floor clamps rather than going negative.
        assert_eq!(peak_to_meter_pct(1), 0);
    }

    /// The slider round-trips: what the user sets is what the gate compares.
    #[test]
    fn meter_pct_round_trips_through_the_peak_scale() {
        for pct in [10, 25, 50, 75, 100] {
            let peak = meter_pct_to_peak(pct);
            let back = peak_to_meter_pct(peak);
            assert!(back.abs_diff(pct) <= 1, "{pct}% -> peak {peak} -> {back}%");
        }
    }

    /// Never returns 0: a zero threshold would gate nothing at all and the
    /// stored value is documented as 1..=1000.
    #[test]
    fn threshold_never_collapses_to_zero() {
        assert!(meter_pct_to_peak(0) >= 1);
        assert!(meter_pct_to_peak(1000) <= 1000);
    }
}
