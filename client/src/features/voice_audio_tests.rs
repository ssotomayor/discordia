use super::*;

fn rms(frames: &[crate::audio_queue::Frame<i16>]) -> f32 {
    let power: f64 = frames
        .iter()
        .flat_map(|frame| frame.samples)
        .map(|s| (s as f64 / i16::MAX as f64).powi(2))
        .sum();
    (power / (frames.len() * FRAME_SAMPLES).max(1) as f64).sqrt() as f32
}

fn processed(
    amplitudes: &[f32],
    gain: i32,
    agc: bool,
    muted: bool,
) -> Vec<crate::audio_queue::Frame<i16>> {
    let controls = AudioControls::from_state(&AppState::empty());
    controls.agc.store(agc, Ordering::Relaxed);
    controls.denoise.store(false, Ordering::Relaxed);
    controls.mic_gain_pct.store(gain, Ordering::Relaxed);
    controls.threshold.store(10, Ordering::Relaxed);
    let (tx, rx) = crate::audio_queue::channel();
    let (out, mut received) = crate::audio_queue::channel();
    let (_reference, reference_rx) = crate::audio_queue::channel();
    let reader = std::thread::spawn(move || {
        let mut frames = Vec::new();
        while let Some(frame) = received.blocking_recv() {
            frames.push(frame);
        }
        frames
    });
    let worker = std::thread::spawn(move || {
        denoise_gate_loop(
            rx,
            out,
            controls,
            Arc::new(MicMeter::default()),
            Arc::new(AtomicBool::new(muted)),
            Arc::new(GateStats::default()),
            reference_rx,
        )
    });
    for (hop, amplitude) in amplitudes.iter().copied().enumerate() {
        let samples = std::array::from_fn(|i| {
            let phase = (hop * FRAME_SAMPLES + i) as f32 * std::f32::consts::TAU * 440.0
                / SAMPLE_RATE as f32;
            amplitude * phase.sin()
        });
        tx.blocking_send(crate::audio_queue::Frame::new(samples))
            .unwrap();
    }
    drop(tx);
    worker.join().unwrap();
    reader.join().unwrap()
}

#[test]
fn native_pipeline_honors_mute_and_zero_microphone_volume() {
    for (gain, muted) in [(100, true), (0, false)] {
        assert!(processed(&[0.2; 150], gain, false, muted).is_empty());
    }
}

#[test]
fn native_pipeline_manual_gain_is_applied_after_automatic_gain() {
    for agc in [false, true] {
        let levels: Vec<_> = [50, 100, 200]
            .into_iter()
            .map(|gain| {
                let frames = processed(&[0.05; 600], gain, agc, false);
                assert!(frames.len() > 100);
                rms(&frames[frames.len() - 100..])
            })
            .collect();
        assert!((levels[1] / levels[0] - 2.0).abs() < 0.05, "{levels:?}");
        assert!((levels[2] / levels[1] - 2.0).abs() < 0.05, "{levels:?}");
    }
}

#[test]
fn native_pipeline_limits_hot_microphones_and_recovers() {
    let mut signal = vec![0.02; 300];
    signal.extend([0.9; 100]);
    signal.extend([0.05; 600]);
    let frames = processed(&signal, 200, true, false);
    assert!(frames.len() > 500);
    let peak = frames
        .iter()
        .flat_map(|f| f.samples)
        .map(|s| s.unsigned_abs())
        .max()
        .unwrap();
    assert!(peak as f32 <= 0.981 * i16::MAX as f32);
    let settled = rms(&frames[frames.len() - 100..]);
    assert!((0.18..0.29).contains(&settled), "settled RMS {settled}");
}

#[test]
fn deafen_and_gain_updates_cover_voice_stream_and_soundboard() {
    let mut initial = AppState::empty();
    initial.voice.deafened = true;
    let controls = AudioControls::from_state(&initial);
    assert!(controls.deafened.load(Ordering::Relaxed));
    controls.gains.lock().insert("alice".into(), 0.5);
    controls.stream_gains.lock().insert("alice".into(), 0.25);
    controls.soundboard_pct.store(60, Ordering::Relaxed);
    let mut tracks = MixerTracks::default();
    for (id, kind, identity) in [
        (1, TrackKind::Voice, "alice"),
        (2, TrackKind::Stream, "alice#video"),
        (3, TrackKind::Soundboard, "alice"),
    ] {
        tracks.buffers.insert(
            id,
            TrackBuf {
                samples: Default::default(),
                identity: identity.into(),
                gain: 1.0,
                kind,
            },
        );
    }
    let update = |tracks: &mut MixerTracks| {
        refresh_gains(
            tracks,
            &controls.gains,
            &controls.stream_gains,
            &controls.soundboard_pct,
            &controls.deafened,
        )
    };
    update(&mut tracks);
    assert!(tracks.buffers.values().all(|t| t.gain == 0.0));
    controls.deafened.store(false, Ordering::Relaxed);
    update(&mut tracks);
    assert_eq!(tracks.buffers[&1].gain, 0.5);
    assert_eq!(tracks.buffers[&2].gain, 0.25);
    assert_eq!(tracks.buffers[&3].gain, 0.6);
    controls.gains.lock().insert("alice".into(), 0.0);
    update(&mut tracks);
    assert_eq!(tracks.buffers[&1].gain, 0.0);
    assert_eq!(tracks.buffers[&3].gain, 0.0);
    assert_eq!(tracks.buffers[&2].gain, 0.25);
}

#[test]
fn closed_gate_room_tone_does_not_raise_the_next_utterance() {
    let baseline = processed(&[0.05; 600], 100, true, false);
    assert!(baseline.len() > 500);
    let settled = rms(&baseline[baseline.len() - 100..]);
    let mut signal = vec![0.05; 600];
    signal.extend([0.004; 500]);
    signal.extend([0.05; 20]);
    let frames = processed(&signal, 100, true, false);
    assert!(frames.len() > 600);
    let resumed = rms(&frames[frames.len() - 15..]);
    println!("AGC settled RMS={settled:.4}, resumed={resumed:.4}");
    assert!(
        resumed < settled * 1.3,
        "gain pumped after pause: {settled} -> {resumed}"
    );
}

#[test]
fn playback_backlog_is_bounded_and_retains_current_audio() {
    let controls = AudioControls::from_state(&AppState::empty());
    let handle = PlaybackHandle {
        tracks: Arc::new(Mutex::new(MixerTracks::default())),
        device_rate: 48000,
        gains: controls.gains,
        stream_gains: controls.stream_gains,
        soundboard_pct: controls.soundboard_pct,
    };
    let id = handle.add_track("alice".into(), TrackKind::Voice);
    let cap = 48000 / PLAYBACK_CAP_DIVISOR as usize;
    handle.push(id, &vec![0.1; cap * 2], cap);
    handle.push(id, &[0.7; 480], cap);
    {
        let tracks = handle.tracks.lock();
        let samples = &tracks.buffers[&id].samples;
        assert_eq!(samples.len(), cap);
        assert!(samples.iter().rev().take(480).all(|s| *s == 0.7));
    }
    handle.remove_track(id);
    assert!(handle.tracks.lock().buffers.is_empty());
}

#[test]
fn native_resampling_preserves_duration_and_level_at_common_device_rates() {
    for (from, to) in [
        (44100, 48000),
        (96000, 48000),
        (48000, 44100),
        (48000, 96000),
    ] {
        let mut resampler = AudioResampler::new(from, to).unwrap();
        let mut received = Vec::new();
        let mut out = Vec::new();
        for hop in 0..100 {
            let input: Vec<_> = (0..from / 100)
                .map(|i| {
                    let phase = (hop * (from / 100) + i) as f64 * std::f64::consts::TAU * 440.0
                        / from as f64;
                    0.2 * phase.sin() as f32
                })
                .collect();
            resampler.process_into(&input, &mut out);
            received.extend_from_slice(&out);
        }
        assert!(
            (received.len() as i64 - to as i64).abs() < (to / 20) as i64,
            "rate {from}->{to}: {} samples",
            received.len()
        );
        let stable = &received[received.len() / 2..];
        let rms = (stable.iter().map(|s| s * s).sum::<f32>() / stable.len() as f32).sqrt();
        assert!(
            (rms - 0.2 / 2.0_f32.sqrt()).abs() < 0.01,
            "rate {from}->{to}: RMS {rms}"
        );
        assert!(received.iter().all(|s| s.is_finite()));
    }
}
