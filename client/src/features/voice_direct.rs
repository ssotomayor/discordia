use super::*;
use libwebrtc::peer_connection_factory::native::PeerConnectionFactoryExt;
use libwebrtc::{audio_track::RtcAudioTrack, peer_connection_factory::PeerConnectionFactory};

pub struct Audio {
    mic: Option<MicCapture>,
    frame_tx: crate::audio_queue::AudioSender<f32>,
    gate_stats: Arc<GateStats>,
    muted: Arc<AtomicBool>,
    input: (Option<String>, bool),
    retry_at: Option<std::time::Instant>,
    pub microphone_error: Option<String>,
    playback: PlaybackMixer,
    faults: UnboundedSender<AudioFault>,
    faults_rx: UnboundedReceiver<AudioFault>,
    output_retry_at: Option<std::time::Instant>,
    controls: AudioControls,
    pub track: RtcAudioTrack,
    publisher: tokio::task::JoinHandle<()>,
    meter: Task,
    diagnostics: Task,
    receivers: Vec<tokio::task::JoinHandle<()>>,
}

impl Audio {
    pub fn start(
        factory: &PeerConnectionFactory,
        mut state: Signal<AppState>,
    ) -> Result<Self, String> {
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
        let (faults, faults_rx) = unbounded_channel::<AudioFault>();
        let playback = PlaybackMixer::start(state, controls.clone(), reference_tx, faults.clone());
        let input = input_preferences(&state.peek());
        let (mic, microphone_error) = match MicCapture::start(
            frame_tx.clone(),
            state,
            muted.clone(),
            gate_stats.clone(),
            faults.clone(),
        ) {
            Ok(mic) => (Some(mic), None),
            Err(error) => {
                tracing::warn!(%error, "DM microphone unavailable; retrying capture");
                state.write().error_toast =
                    Some(format!("Couldn't open the call microphone: {error}"));
                (None, Some(error))
            }
        };
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
            mic,
            frame_tx,
            gate_stats,
            muted,
            input,
            retry_at: None,
            microphone_error,
            playback,
            faults,
            faults_rx,
            output_retry_at: None,
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
        while let Ok(fault) = self.faults_rx.try_recv() {
            match fault {
                AudioFault::Input => {
                    self.mic = None;
                    self.retry_at = None;
                }
                AudioFault::Output => {
                    self.playback.drop_stream();
                    self.output_retry_at = None;
                }
            }
        }
        let muted = state.peek().voice.muted || state.peek().voice.deafened;
        update_controls(&self.controls, &state.peek());
        let input = input_preferences(&state.peek());
        if self.input != input || self.mic.is_none() {
            self.track.set_enabled(false);
            self.muted.store(true, Ordering::Relaxed);
        }
        let result = reopen_capture(
            &mut self.mic,
            &mut self.input,
            &mut self.retry_at,
            input,
            std::time::Instant::now(),
            || {
                let faults = self.faults.clone();
                MicCapture::start(
                    self.frame_tx.clone(),
                    state,
                    self.muted.clone(),
                    self.gate_stats.clone(),
                    faults,
                )
            },
        );
        if let Some(result) = result {
            match result {
                Ok(()) => self.microphone_error = None,
                Err(error) => {
                    if self.microphone_error.as_ref() != Some(&error) {
                        tracing::warn!(%error, "DM microphone unavailable; retrying capture");
                        state.write().error_toast =
                            Some(format!("Couldn't change the call microphone: {error}"));
                    }
                    self.microphone_error = Some(error);
                }
            }
        }
        let transmitting = !muted && self.mic.is_some();
        self.muted.store(!transmitting, Ordering::Relaxed);
        self.track.set_enabled(transmitting);

        if !self.playback.is_open()
            && self
                .output_retry_at
                .is_none_or(|at| std::time::Instant::now() >= at)
        {
            let faults = self.faults.clone();
            match self.playback.try_open(&mut state, &self.controls, &faults) {
                Ok(()) => self.output_retry_at = None,
                Err(_) => {
                    self.output_retry_at = Some(std::time::Instant::now() + DEVICE_RETRY);
                }
            }
        }
    }
}

fn reopen_capture<T>(
    capture: &mut Option<T>,
    attempted_input: &mut (Option<String>, bool),
    retry_at: &mut Option<std::time::Instant>,
    requested_input: (Option<String>, bool),
    now: std::time::Instant,
    open: impl FnOnce() -> Result<T, String>,
) -> Option<Result<(), String>> {
    let changed = *attempted_input != requested_input;
    if !changed && (capture.is_some() || retry_at.is_some_and(|deadline| now < deadline)) {
        return None;
    }
    *attempted_input = requested_input;
    // Realtek and macOS capture must be released before reopening the same device.
    capture.take();
    match open() {
        Ok(new_capture) => {
            *capture = Some(new_capture);
            *retry_at = None;
            Some(Ok(()))
        }
        Err(error) => {
            *retry_at = Some(now + std::time::Duration::from_secs(2));
            Some(Err(error))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_capture_switch_retries_and_recovers_without_another_preference_change() {
        let now = std::time::Instant::now();
        let mut capture = Some(1);
        let mut input = (None, false);
        let requested = (Some("microphone".into()), true);
        let mut retry = None;
        assert!(
            reopen_capture(
                &mut capture,
                &mut input,
                &mut retry,
                requested.clone(),
                now,
                || Err("device busy".into())
            )
            .unwrap()
            .is_err()
        );
        assert!(capture.is_none());
        assert!(
            reopen_capture(
                &mut capture,
                &mut input,
                &mut retry,
                requested.clone(),
                now,
                || panic!("must wait before retrying")
            )
            .is_none()
        );
        let later = now + std::time::Duration::from_secs(2);
        assert!(
            reopen_capture(
                &mut capture,
                &mut input,
                &mut retry,
                requested.clone(),
                later,
                || Ok(2)
            )
            .unwrap()
            .is_ok()
        );
        assert_eq!(capture, Some(2));
        assert!(retry.is_none());
        assert!(
            reopen_capture(
                &mut capture,
                &mut input,
                &mut retry,
                requested,
                later,
                || panic!("working capture must not reopen")
            )
            .is_none()
        );
    }

    #[test]
    fn changing_input_during_retry_wait_attempts_immediately() {
        let now = std::time::Instant::now();
        let mut capture: Option<u8> = None;
        let mut input = (Some("unavailable".into()), true);
        let mut retry = Some(now + std::time::Duration::from_secs(2));
        assert!(
            reopen_capture(
                &mut capture,
                &mut input,
                &mut retry,
                (None, false),
                now,
                || Ok(1)
            )
            .unwrap()
            .is_ok()
        );
        assert_eq!(capture, Some(1));
        assert!(retry.is_none());
    }

    #[cfg(target_os = "windows")]
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
