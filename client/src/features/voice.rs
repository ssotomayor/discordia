//! Voice service. Owns the LiveKit Room, the microphone capture pipeline,
//! and the remote-audio playback pipeline.
//!
//! LiveKit is a libwebrtc-based SFU. Connecting to a room gives us libwebrtc's
//! AEC3 / NS / AGC / Opus / congestion control end-to-end. The Rust SDK
//! exposes only PCM frames at the edges; cpal handles the actual mic and
//! speaker devices.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use dioxus::core::Task;
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

/// How long the transmit gate stays open after the last frame above threshold,
/// in 10ms frames. Speech has gaps — breaths, stops between words — and a gate
/// that slams shut in them chops the front off the next syllable. 300ms is the
/// usual compromise between "doesn't clip speech" and "doesn't leak the room".
const GATE_HANGOVER_FRAMES: u32 = 30;

/// How long to wait for a clean leave before giving up on it.
const ROOM_CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// libwebrtc APM configuration.
///
/// `noise_suppression` is the inverse of the DeepFilterNet toggle: exactly one
/// suppressor should be in the path. AEC and AGC are orthogonal (echo and
/// level, not noise) and stay on either way.
fn apm_options(deepfilter_on: bool) -> AudioSourceOptions {
    AudioSourceOptions {
        echo_cancellation: true,
        noise_suppression: !deepfilter_on,
        auto_gain_control: true,
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
    /// DeepFilterNet noise suppression on the captured mic signal.
    denoise: Arc<AtomicBool>,
    /// Per-participant playback gain, keyed by LiveKit identity (= pubkey).
    /// Absent = unity. Applied to *incoming* audio in our own mixer only.
    gains: Arc<Mutex<HashMap<String, f32>>>,
    /// The same, for screen-share audio tracks. Absent = SILENT, not unity:
    /// stream audio plays only while you are watching that person's share, so
    /// the default has to be off.
    stream_gains: Arc<Mutex<HashMap<String, f32>>>,
}

impl AudioControls {
    fn new(threshold: u32, denoise: bool) -> Self {
        Self {
            threshold: Arc::new(AtomicI32::new(threshold as i32)),
            denoise: Arc::new(AtomicBool::new(denoise)),
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
    /// Set the microphone gate threshold (1..=1000, peak ×1000). Below it the
    /// mic is treated as inactive and nothing is transmitted.
    SetSensitivity {
        threshold: u32,
    },
    /// Toggle DeepFilterNet noise suppression on captured mic audio.
    SetNoiseCancellation {
        enabled: bool,
    },
    /// Start/stop capturing this machine's audio and publishing it alongside a
    /// screen share. Only does anything where `sysaudio` has a backend.
    SetSystemAudio {
        enabled: bool,
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
        AudioControls::new(s.mic_sensitivity, s.noise_cancellation)
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
                    prev.shutdown().await;
                }
                match ActiveVoice::connect(
                    &livekit_url,
                    &token,
                    channel_id,
                    state.clone(),
                    controls.clone(),
                )
                .await
                {
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
                            if is_input { inputs.push(name.clone()); }
                            if is_output { outputs.push(name); }
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
                        match ActiveVoice::connect(url, tok, cid, state.clone(), controls.clone())
                            .await
                        {
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
                let threshold = threshold.clamp(1, 1000);
                eprintln!("[voice] SetSensitivity threshold={threshold}");
                // The live pipeline reads this atomic, so the change lands on
                // the very next 10ms frame — no reconnect, no session restart.
                controls.threshold.store(threshold as i32, Ordering::Relaxed);
                state.write().mic_sensitivity = threshold;
            }
            VoiceCmd::SetNoiseCancellation { enabled } => {
                eprintln!("[voice] SetNoiseCancellation enabled={enabled}");
                controls.denoise.store(enabled, Ordering::Relaxed);
                // Hand libwebrtc's own suppressor over to DeepFilterNet, live —
                // `set_audio_options` reconfigures the APM without republishing
                // the track.
                if let Some(active) = session.as_ref() {
                    active.set_apm_denoise(enabled);
                }
                state.write().noise_cancellation = enabled;
            }
            VoiceCmd::SetSystemAudio { enabled } => {
                if let Some(active) = session.as_mut() {
                    active.set_system_audio(enabled).await;
                } else if enabled {
                    eprintln!("[voice] SetSystemAudio ignored — no voice session");
                }
            }
            VoiceCmd::SetStreamVolume { pubkey, gain } => {
                controls.stream_gains.lock().insert(pubkey, gain.clamp(0.0, 2.0));
            }
            VoiceCmd::SetUserVolume { pubkey, gain } => {
                let gain = gain.clamp(0.0, 2.0);
                eprintln!("[voice] SetUserVolume {} gain={gain:.2}", &pubkey[..pubkey.len().min(8)]);
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
    /// Mirrors the mic meter into `AppState`. Cancelled on shutdown — left
    /// running, one accumulates per reconnect and they fight over the same
    /// `mic_level` / `speaking` fields.
    meter_task: Task,
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
            apm_options(controls.denoise.load(Ordering::Relaxed)),
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
        let muted = Arc::new(AtomicBool::new(false));
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
        let mic = MicCapture::start(frame_tx, state.clone(), muted)?;
        let meter_task = spawn_meter_task(state.clone(), meter);

        // Remote audio mixer.
        let playback = PlaybackMixer::start(state.clone(), controls.clone())?;
        let mixer_handle = playback.handle();

        // Bridge for "this sharer has native screen audio" notices: the event
        // task can't touch a Signal, so it posts identities here and a Dioxus
        // task applies them.
        let (native_audio_tx, mut native_audio_rx) =
            tokio::sync::mpsc::unbounded_channel::<String>();
        {
            let mut state = state;
            dioxus::prelude::spawn(async move {
                while let Some(id) = native_audio_rx.recv().await {
                    state.write().stream_native_audio.insert(id);
                }
            });
        }

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
                    if let RoomEvent::TrackSubscribed { track, publication, participant } = ev {
                        // Screen-share audio and a microphone arrive on the
                        // same event; only the publication says which is which.
                        let is_stream =
                            publication.source() == TrackSource::ScreenshareAudio;
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
                                let _ = native_audio_tx.send(identity.clone());
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
            }
        });

        Ok(Self {
            room,
            source,
            mic,
            local_audio: local_audio_for_mute,
            _playback: playback,
            event_task,
            meter_task,
            system_audio: None,
        })
    }

    /// Start or stop publishing this machine's audio as a second track.
    ///
    /// Published on the voice room rather than the webview's screen room: the
    /// native SDK is already connected here, every peer is already subscribed,
    /// and it lands in the same mixer as everything else — so the per-sharer
    /// volume control works on it without any new delivery path.
    async fn set_system_audio(&mut self, enabled: bool) {
        if enabled == self.system_audio.is_some() {
            return;
        }
        if !enabled {
            if let Some(sa) = self.system_audio.take() {
                let _ = self
                    .room
                    .local_participant()
                    .unpublish_track(&sa.sid)
                    .await;
            }
            eprintln!("[voice] system audio stopped");
            return;
        }
        if !crate::sysaudio::supported() {
            return;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        let capture = match crate::sysaudio::start(tx) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[voice] system audio unavailable: {e}");
                return;
            }
        };
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
                    ..Default::default()
                },
            )
            .await
        {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[voice] publish system audio failed: {e}");
                return;
            }
        };
        let task = tokio::spawn(publish_pcm(rx, source));
        self.system_audio = Some(SystemAudioTrack {
            sid: publication.sid(),
            _capture: capture,
            task,
        });
        eprintln!("[voice] system audio started");
    }

    /// Swap libwebrtc's noise suppressor in or out while the call is live.
    fn set_apm_denoise(&self, deepfilter_on: bool) {
        self.source.set_audio_options(apm_options(deepfilter_on));
    }

    async fn set_muted(&mut self, muted: bool) {
        self.mic.muted.store(muted, Ordering::Relaxed);
        // Disabling the track stops audio from being sent and signals other
        // peers via the muted-track event.
        self.local_audio.rtc_track().set_enabled(!muted);
    }

    async fn shutdown(self) {
        self.event_task.abort();
        self.meter_task.cancel();
        // Stop capturing before leaving, so no frames are published into a
        // room that is on its way out.
        self.mic.stop();
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
    // Frames left before the gate closes. Non-zero = currently transmitting.
    let mut hangover = 0u32;
    let mut gated = 0u64;

    while let Some(mut samples) = frame_rx.blocking_recv() {
        // Muted short-circuits everything except metering: the VU bar should
        // still move so a user who forgot they're muted can see the mic is
        // fine. No point running the model on audio nobody will hear.
        if muted.load(Ordering::Relaxed) {
            meter.bump_peak(peak_fixed(&samples));
            meter.open.store(false, Ordering::Relaxed);
            hangover = 0;
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
        if peak > controls.threshold.load(Ordering::Relaxed) {
            hangover = GATE_HANGOVER_FRAMES;
        } else {
            hangover = hangover.saturating_sub(1);
        }
        let open = hangover > 0;
        meter.open.store(open, Ordering::Relaxed);
        if !open {
            gated += 1;
            continue;
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
        if sent % 500 == 0 {
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
                    if let Ok(name) = d.name() {
                        if name == sel_name {
                            found = Some(d);
                            break;
                        }
                    }
                }
            }
            found.unwrap_or_else(|| host.default_input_device().expect("no default input device"))
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
        let accum: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(FRAME_SAMPLES * 4)));

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
                        f32_buf.extend(
                            data.iter()
                                .map(|s| (*s as f32 - 32768.0) / 32768.0),
                        );
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
                let level = if p < 0.001 { "silent" } else if p < 0.01 { "very quiet" } else { "speaking" };
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
        let inner = FftFixedIn::<f32>::new(
            from_rate as usize,
            to_rate as usize,
            RESAMPLER_CHUNK,
            2,
            1,
        )
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

        let Self { inner, input_accum, chunk_in, scratch_out } = self;

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
            self.stream_gains.lock().get(&identity).copied().unwrap_or(0.0)
        } else {
            self.gains.lock().get(&identity).copied().unwrap_or(1.0)
        };
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
        self.tracks.lock().buffers.remove(&id);
    }
}

/// Copy the user's chosen volumes onto the live tracks. Called once per output
/// callback: a HashMap lookup per *sample* would be absurd, and the volume only
/// needs to be as fresh as one buffer (a few ms).
fn refresh_gains(
    tracks: &mut MixerTracks,
    gains: &Arc<Mutex<HashMap<String, f32>>>,
    stream_gains: &Arc<Mutex<HashMap<String, f32>>>,
) {
    if tracks.buffers.is_empty() {
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
    fn start(state: Signal<AppState>, controls: AudioControls) -> Result<Self, String> {
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
            found.unwrap_or_else(|| host.default_output_device().expect("no default output device"))
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
                    refresh_gains(&mut tracks, &gains_f32, &stream_gains_f32);
                    let mut pulled = 0u64;
                    // Drift compensation thresholds (in samples at device rate).
                    // The overrun mark must stay under the producer-side cap
                    // (PLAYBACK_CAP_DIVISOR, ~200ms) or it can never be reached
                    // and the branch is dead.
                    let overrun_threshold = (device_rate_cb as f64 * 0.15) as usize; // 150ms
                    let underrun_threshold = (device_rate_cb as f64 * 0.05) as usize; // 50ms
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
                    refresh_gains(&mut tracks, &gains_i16, &stream_gains_i16);
                    let overrun_threshold = (device_rate_cb as f64 * 0.15) as usize;
                    let underrun_threshold = (device_rate_cb as f64 * 0.05) as usize;
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
    let track_id = handle.add_track(identity, is_stream);
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
        if frames % 500 == 0 {
            eprintln!(
                "[voice] remote-track: {frames} frames, {sample_count} samples, peak={peak_recent} ({})",
                if peak_recent < 100 { "near-silent" } else if peak_recent < 1000 { "very quiet" } else { "audible" }
            );
            peak_recent = 0;
        }
    }
    // Drop this track's jitter buffer, otherwise a participant who left keeps
    // contributing silence to the mix (and leaks a buffer per rejoin).
    handle.remove_track(track_id);
    eprintln!("[voice] remote-track stream ended after {frames} frames");
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert!(
                back.abs_diff(pct) <= 1,
                "{pct}% -> peak {peak} -> {back}%"
            );
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
