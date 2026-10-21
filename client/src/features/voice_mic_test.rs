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
    async fn start(
        state: Signal<AppState>,
        requested: impl Fn() -> bool,
    ) -> Result<Option<Self>, String> {
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
        let worker_muted = muted.clone();
        let worker_stats = stats.clone();
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
                    worker_muted,
                    worker_stats,
                    reference_rx,
                );
            })
            .map_err(|error| format!("Start microphone test processing: {error}"))?;
        stats.wait_ready().await?;
        if !requested() || !state.peek().audio_settings || in_call(&state.peek()) {
            return Ok(None);
        }
        let mic = MicCapture::start(input_tx, state, muted, stats, fault_tx)?;
        let handle = playback.handle();
        let track = handle.add_track("mic-test".into(), TrackKind::Voice);
        let monitor = tokio::spawn(monitor_loop(processed_rx, handle, track));
        Ok(Some(Self {
            _mic: mic,
            _playback: playback,
            controls,
            meter,
            faults,
            monitor,
            devices: devices(&state.peek()),
        }))
    }

    fn update(&mut self, state: &AppState) -> Result<(u32, bool, u32, u32), String> {
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
            self.meter
                .peak_pre
                .swap(0, Ordering::Relaxed)
                .clamp(0, 1000) as u32,
            self.meter.peak.swap(0, Ordering::Relaxed).clamp(0, 1000) as u32,
        ))
    }

    fn denoise_fallback(&self) -> Option<crate::denoise::Fallback> {
        self.meter.denoise_fallback()
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

#[path = "voice_mic_setup.rs"]
mod setup;
pub use setup::MicrophoneTestPanel;

async fn monitor_loop(
    mut processed_rx: crate::audio_queue::AudioReceiver<i16>,
    handle: PlaybackHandle,
    track: u64,
) {
    let mut rate = handle.rate();
    let mut resampler = AudioResampler::new(SAMPLE_RATE, rate);
    let mut output = Vec::new();
    while let Some(frame) = processed_rx.recv().await {
        if !frame.is_fresh() {
            continue;
        }
        if rate != handle.rate() {
            rate = handle.rate();
            resampler = AudioResampler::new(SAMPLE_RATE, rate);
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
            rate,
            rate as usize / PLAYBACK_CAP_DIVISOR as usize,
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
            position: None,
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
