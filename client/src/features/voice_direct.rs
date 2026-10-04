use super::*;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::{audio_track::RtcAudioTrack, peer_connection_factory::PeerConnectionFactory};

pub struct Audio {
    mic: Option<MicCapture>,
    frame_tx: crate::audio_queue::AudioSender<f32>,
    gate_stats: Arc<GateStats>,
    muted: Arc<AtomicBool>,
    input: (Option<String>, bool),
    playback: PlaybackMixer,
    controls: AudioControls,
    pub track: RtcAudioTrack,
    publisher: tokio::task::JoinHandle<()>,
    meter: Task,
    diagnostics: Task,
    receivers: Vec<tokio::task::JoinHandle<()>>,
}

impl Audio {
    pub fn start(factory: &PeerConnectionFactory, state: Signal<AppState>) -> Result<Self, String> {
        let controls = AudioControls::from_state(&state.peek());
        let source = NativeAudioSource::new(inert_apm_options(), SAMPLE_RATE, CHANNELS, 100);
        let track = factory.create_audio_track("dm-mic", source.clone());
        let (frame_tx, frame_rx) = crate::audio_queue::channel();
        let (gated_tx, gated_rx) = crate::audio_queue::channel();
        let (reference_tx, reference_rx) = crate::audio_queue::channel();
        let meter = Arc::new(MicMeter::default());
        let gate_stats = Arc::new(GateStats::default());
        let muted = Arc::new(AtomicBool::new(
            state.peek().voice.muted || state.peek().voice.deafened,
        ));
        track.set_enabled(!muted.load(Ordering::Relaxed));
        let playback = PlaybackMixer::start(state, controls.clone(), reference_tx)?;
        let input = input_preferences(&state.peek());
        let mic = MicCapture::start(frame_tx.clone(), state, muted.clone(), gate_stats.clone())?;
        let dsp_controls = controls.clone();
        let dsp_meter = meter.clone();
        let dsp_muted = muted.clone();
        let dsp_stats = gate_stats.clone();
        std::thread::Builder::new()
            .name("dm-mic-dsp".into())
            .spawn(move || {
                denoise_gate_loop(
                    frame_rx,
                    gated_tx,
                    dsp_controls,
                    dsp_meter,
                    dsp_muted,
                    dsp_stats,
                    reference_rx,
                );
            })
            .map_err(|e| format!("Start call audio processing: {e}"))?;
        Ok(Self {
            mic: Some(mic),
            frame_tx,
            gate_stats,
            muted,
            input,
            playback,
            controls,
            track,
            publisher: tokio::spawn(publish_loop(gated_rx, source)),
            meter: spawn_meter_task(state, meter),
            diagnostics: spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    let s = state.peek();
                    eprintln!(
                        "[dm-call] mic pre={} processed={} gain={}% agc={} noise_suppression={} threshold={} muted={} deafened={} playback_gain={:.2}",
                        peak_to_db_label(s.mic_level_pre),
                        peak_to_db_label(s.mic_level),
                        s.mic_volume,
                        s.auto_gain_control,
                        s.noise_cancellation,
                        peak_to_db_label(s.mic_sensitivity),
                        s.voice.muted,
                        s.voice.deafened,
                        s.dm_call
                            .as_ref()
                            .map_or(1.0, |call| s.voice_gain_of(&call.peer)),
                    );
                }
            }),
            receivers: Vec::new(),
        })
    }

    pub fn receive(&mut self, track: RtcAudioTrack, peer: String) {
        if !self.receivers.is_empty() {
            return;
        }
        let stream = NativeAudioStream::new(track, SAMPLE_RATE as i32, CHANNELS as i32);
        self.receivers.push(tokio::spawn(consume_remote_track(
            stream,
            self.playback.handle(),
            peer,
            TrackKind::Voice,
        )));
    }

    pub fn update(&mut self, mut state: Signal<AppState>) {
        let muted = state.peek().voice.muted || state.peek().voice.deafened;
        self.muted.store(muted, Ordering::Relaxed);
        self.track.set_enabled(!muted);
        update_controls(&self.controls, &state.peek());
        let input = input_preferences(&state.peek());
        if self.input != input {
            self.input = input;
            self.mic.take();
            match MicCapture::start(
                self.frame_tx.clone(),
                state,
                self.muted.clone(),
                self.gate_stats.clone(),
            ) {
                Ok(mic) => self.mic = Some(mic),
                Err(error) => {
                    state.write().error_toast =
                        Some(format!("Couldn't change the call microphone: {error}"))
                }
            }
        }
    }
}

fn input_preferences(state: &AppState) -> (Option<String>, bool) {
    (
        state.selected_input_device.clone(),
        state.bypass_system_audio_processing,
    )
}

pub(super) fn update_controls(controls: &AudioControls, state: &AppState) {
    controls
        .deafened
        .store(state.voice.deafened, Ordering::Relaxed);
    controls
        .threshold
        .store(state.mic_sensitivity as i32, Ordering::Relaxed);
    controls
        .mic_gain_pct
        .store(state.mic_volume as i32, Ordering::Relaxed);
    controls
        .agc
        .store(state.auto_gain_control, Ordering::Relaxed);
    controls
        .denoise
        .store(state.noise_cancellation, Ordering::Relaxed);
    controls
        .atten_lim_db
        .store(state.denoise_atten_lim_db, Ordering::Relaxed);
    let mut gains = controls.gains.lock();
    gains.clear();
    for peer in state.user_volumes.keys().chain(state.user_muted.iter()) {
        gains.insert(peer.clone(), state.voice_gain_of(peer));
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.track.set_enabled(false);
        self.publisher.abort();
        self.meter.cancel();
        self.diagnostics.cancel();
        for receiver in self.receivers.drain(..) {
            receiver.abort();
        }
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "opens local microphone and playback devices"]
    async fn changing_dm_input_and_bypass_reopens_capture_without_replacing_track() {
        let mut dom = VirtualDom::new(|| rsx! {});
        dom.rebuild_in_place();
        let factory = PeerConnectionFactory::default();
        let (mut state, mut audio) = dom.in_scope(ScopeId::ROOT, || {
            let mut initial = AppState::empty();
            initial.noise_cancellation = false;
            initial.auto_gain_control = false;
            initial.mic_sensitivity = 1;
            initial.bypass_system_audio_processing = false;
            let state = Signal::new(initial);
            (state, Audio::start(&factory, state).unwrap())
        });
        let track_id = audio.track.id();
        let device = cpal::default_host()
            .default_input_device()
            .unwrap()
            .name()
            .unwrap();
        for (input, bypass) in [
            (Some(device.clone()), false),
            (Some(device), true),
            (None, false),
        ] {
            let before = audio.gate_stats.passed.load(Ordering::Relaxed)
                + audio.gate_stats.dropped.load(Ordering::Relaxed);
            dom.in_scope(ScopeId::ROOT, || {
                state.write().selected_input_device = input;
                state.write().bypass_system_audio_processing = bypass;
                audio.update(state);
            });
            assert!(audio.mic.is_some());
            assert_eq!(audio.track.id(), track_id);
            assert_eq!(audio.input, input_preferences(&state.peek()));
            tokio::time::sleep(std::time::Duration::from_millis(350)).await;
            let after = audio.gate_stats.passed.load(Ordering::Relaxed)
                + audio.gate_stats.dropped.load(Ordering::Relaxed);
            assert!(after > before, "reopened capture must deliver DSP frames");
        }
        dom.in_scope(ScopeId::ROOT, || drop(audio));
    }
}
