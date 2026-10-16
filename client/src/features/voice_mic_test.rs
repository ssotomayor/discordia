use super::*;

struct MicrophoneTest {
    _mic: MicCapture,
    _playback: PlaybackMixer,
    controls: AudioControls,
    meter: Arc<MicMeter>,
    faults: UnboundedReceiver<AudioFault>,
    monitor: tokio::task::JoinHandle<()>,
    devices: (Option<String>, Option<String>, bool),
}

impl MicrophoneTest {
    fn start(state: Signal<AppState>) -> Result<Self, String> {
        let controls = AudioControls::from_state(&state.peek());
        controls.deafened.store(false, Ordering::Relaxed);
        controls.gains.lock().clear();
        let (input_tx, input_rx) = crate::audio_queue::channel();
        let (processed_tx, processed_rx) = crate::audio_queue::channel();
        let (reference_tx, reference_rx) = crate::audio_queue::channel();
        let (fault_tx, faults) = unbounded_channel();
        let meter = Arc::new(MicMeter::default());
        let stats = Arc::new(GateStats::default());
        let muted = Arc::new(AtomicBool::new(false));
        let playback =
            PlaybackMixer::start(state, controls.clone(), reference_tx, fault_tx.clone());
        if !playback.is_open() {
            return Err("No audio output is available for the microphone test.".into());
        }
        let mic = MicCapture::start(input_tx, state, muted.clone(), stats.clone(), fault_tx)?;
        let dsp_controls = controls.clone();
        let dsp_meter = meter.clone();
        std::thread::Builder::new()
            .name("mic-test-dsp".into())
            .spawn(move || {
                denoise_gate_loop(
                    input_rx,
                    processed_tx,
                    dsp_controls,
                    dsp_meter,
                    muted,
                    stats,
                    reference_rx,
                );
            })
            .map_err(|error| format!("Start microphone test processing: {error}"))?;
        let handle = playback.handle();
        let track = handle.add_track("mic-test".into(), TrackKind::Voice);
        let monitor = tokio::spawn(monitor_loop(processed_rx, handle, track));
        Ok(Self {
            _mic: mic,
            _playback: playback,
            controls,
            meter,
            faults,
            monitor,
            devices: devices(&state.peek()),
        })
    }

    fn update(&mut self, state: &AppState) -> Result<(u32, bool), String> {
        if self.faults.try_recv().is_ok() {
            return Err(
                "The microphone test stopped because an audio device became unavailable.".into(),
            );
        }
        direct::update_controls(&self.controls, state);
        self.controls.deafened.store(false, Ordering::Relaxed);
        self.controls.gains.lock().clear();
        Ok((
            self.meter
                .gate_peak
                .swap(0, Ordering::Relaxed)
                .clamp(0, 1000) as u32,
            self.meter.open.load(Ordering::Relaxed),
        ))
    }
}

impl Drop for MicrophoneTest {
    fn drop(&mut self) {
        self.monitor.abort();
    }
}

fn devices(state: &AppState) -> (Option<String>, Option<String>, bool) {
    (
        state.selected_input_device.clone(),
        state.selected_output_device.clone(),
        state.bypass_system_audio_processing,
    )
}

fn in_call(state: &AppState) -> bool {
    matches!(
        state.voice.phase,
        VoicePhase::Connecting | VoicePhase::Connected
    ) || state.dm_call.is_some()
}

#[component]
pub fn MicrophoneTestPanel() -> Element {
    let state = crate::state::use_app_state();
    let mut enabled = use_signal(|| false);
    let mut level = use_signal(|| (0_u32, false));
    let mut error = use_signal(|| None::<String>);
    use_future(move || async move {
        let mut test: Option<MicrophoneTest> = None;
        loop {
            if *enabled.peek() && (!state.peek().audio_settings || in_call(&state.peek())) {
                enabled.set(false);
            }
            if !*enabled.peek() {
                test = None;
            } else {
                if test
                    .as_ref()
                    .is_some_and(|test| test.devices != devices(&state.peek()))
                {
                    test = None;
                }
                if test.is_none() {
                    match MicrophoneTest::start(state) {
                        Ok(started) => test = Some(started),
                        Err(message) => {
                            error.set(Some(message));
                            enabled.set(false);
                        }
                    }
                }
                if let Some(test) = &mut test {
                    match test.update(&state.peek()) {
                        Ok(value) => {
                            if *level.peek() != value {
                                level.set(value);
                            }
                        }
                        Err(message) => {
                            error.set(Some(message));
                            enabled.set(false);
                        }
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    });
    let busy = in_call(&state.read());
    let active = enabled() && !busy;
    let (peak, open) = level();
    let percent = if active { peak_to_meter_pct(peak) } else { 0 };
    rsx! {
        div { class: "mb-4 rounded-lg border border-[var(--border)] p-3",
            div { class: "flex items-center justify-between gap-2",
                span { class: "text-[12px] font-semibold text-[var(--text)]", {crate::i18n::text("Microphone test", &[])} }
                button {
                    class: "rounded px-3 py-1 text-[11px] border border-[var(--border)] text-[var(--text)] disabled:opacity-60",
                    disabled: busy,
                    "aria-pressed": active,
                    onclick: move |_| {
                        error.set(None);
                        enabled.set(!enabled());
                    },
                    if active { {crate::i18n::text("Stop test", &[])} } else { {crate::i18n::text("Test microphone", &[])} }
                }
            }
            p { class: "mt-1 text-[11px] text-[var(--text-dim)]",
                {crate::i18n::text("Speak to hear yourself through the selected output with your current microphone settings. Use headphones to avoid feedback.", &[])}
            }
            div { class: "w-full h-2 mt-2 rounded-full overflow-hidden", style: "background-color: var(--bg2);",
                div { class: "h-full rounded-full transition-all duration-75", style: "width: {percent}%; background-color: var(--accent);" }
            }
            if busy {
                p { class: "mt-1 text-[10px] text-[var(--text-dim)]", {crate::i18n::text("Leave your voice call to test the microphone.", &[])} }
            } else if active {
                p { class: "mt-1 text-[10px] text-[var(--text-dim)]", role: "status",
                    if open { {crate::i18n::text("Playing your voice back", &[])} } else { {crate::i18n::text("Below threshold — not transmitting", &[])} }
                }
            }
            if let Some(message) = error() {
                p { class: "mt-1 text-[11px]", style: "color: var(--danger);", role: "alert", {crate::i18n::message(&message)} }
            }
        }
    }
}

async fn monitor_loop(
    mut processed_rx: crate::audio_queue::AudioReceiver<i16>,
    handle: PlaybackHandle,
    track: u64,
) {
    let mut resampler = AudioResampler::new(SAMPLE_RATE, handle.rate());
    let mut output = Vec::new();
    while let Some(frame) = processed_rx.recv().await {
        if !frame.is_fresh() {
            continue;
        }
        let samples = frame.samples.map(|sample| sample as f32 / 32768.0);
        let samples = if let Some(resampler) = &mut resampler {
            resampler.process_into(&samples, &mut output);
            &output[..]
        } else {
            &samples[..]
        };
        handle.push(
            track,
            samples,
            handle.rate() as usize / PLAYBACK_CAP_DIVISOR as usize,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn monitor_preserves_levels_discards_stale_audio_and_bounds_playback() {
        let controls = AudioControls::from_state(&AppState::empty());
        let handle = PlaybackHandle {
            tracks: Arc::new(Mutex::new(MixerTracks::default())),
            device_rate: Arc::new(AtomicU32::new(SAMPLE_RATE)),
            gains: controls.gains,
            stream_gains: controls.stream_gains,
            soundboard_pct: controls.soundboard_pct,
        };
        let track = handle.add_track("mic-test".into(), TrackKind::Voice);
        let (tx, rx) = crate::audio_queue::channel();
        let monitor = tokio::spawn(monitor_loop(rx, handle.clone(), track));
        tx.send(crate::audio_queue::Frame {
            samples: [i16::MAX; FRAME_SAMPLES],
            captured: Instant::now() - std::time::Duration::from_secs(1),
        })
        .await
        .unwrap();
        for _ in 0..30 {
            tx.send(crate::audio_queue::Frame::new([8192; FRAME_SAMPLES]))
                .await
                .unwrap();
        }
        drop(tx);
        monitor.await.unwrap();
        let tracks = handle.tracks.lock();
        let samples = &tracks.buffers[&track].samples;
        assert_eq!(
            samples.len(),
            SAMPLE_RATE as usize / PLAYBACK_CAP_DIVISOR as usize
        );
        assert!(samples.iter().all(|sample| *sample == 0.25));
    }

    #[test]
    fn test_is_available_without_a_call_and_after_a_failed_connection() {
        let mut state = AppState::empty();
        assert!(!in_call(&state));
        state.voice.phase = VoicePhase::Connecting;
        assert!(in_call(&state));
        state.voice.phase = VoicePhase::Connected;
        assert!(in_call(&state));
        state.voice.phase = VoicePhase::Error;
        assert!(!in_call(&state));
    }
}
