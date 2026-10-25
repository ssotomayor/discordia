use super::*;

#[test]
fn stream_controls_reach_the_mixer_without_polling_the_voice_command_loop() {
    let controls = AudioControls::from_state(&AppState::empty());
    let playback = StreamPlaybackControls(controls.stream_gains.clone());
    playback.apply(&[("stream".into(), 0.35)]);
    assert_eq!(controls.stream_gains.lock().get("stream"), Some(&0.35));
    playback.apply(&[("stream".into(), 0.0)]);
    assert_eq!(controls.stream_gains.lock().get("stream"), Some(&0.0));
    playback.apply(&[]);
    assert!(controls.stream_gains.lock().is_empty());
}

#[test]
fn overlapping_voice_stream_and_soundboard_are_limited_after_their_volumes() {
    for rate in [44_100, 48_000, 96_000] {
        let mut tracks = MixerTracks::default();
        let samples: std::collections::VecDeque<_> = (0..rate)
            .map(|i| 0.4 * (std::f32::consts::TAU * 200.0 * i as f32 / rate as f32).sin())
            .collect();
        for (id, kind, gain) in [
            (0, TrackKind::Voice, 1.0),
            (1, TrackKind::Voice, 2.0),
            (2, TrackKind::Stream, 0.5),
            (3, TrackKind::Soundboard, 0.5),
        ] {
            tracks.buffers.insert(
                id,
                TrackBuf {
                    samples: samples.clone(),
                    identity: format!("peer-{id}"),
                    gain,
                    kind,
                    starved: 0,
                    drift_correction: false,
                    recovery: playback_recovery::Recovery::new(rate),
                    source_rate: rate,
                },
            );
        }
        let mut limiter = crate::playback_limiter::Limiter::new(rate);
        let mut raw_peak = 0.0_f32;
        let mut limited_peak = 0.0_f32;
        let mut error = 0.0_f64;
        let mut clipped_error = 0.0_f64;
        let delay = rate / 200;
        for i in 0..rate {
            let raw = next_playback_sample(&mut tracks);
            let limited = limiter.process(raw);
            let mut rng = DITHER_SEED;
            let integer = dither_to_i16(limited, &mut rng);
            raw_peak = raw_peak.max(raw.abs());
            limited_peak = limited_peak.max(limited.abs());
            assert!(limited.abs() <= 1.0);
            assert!((f32::from(integer) / f32::from(i16::MAX) - limited).abs() < 0.0001);
            if i > rate / 10 {
                let expected =
                    (std::f32::consts::TAU * 200.0 * (i - delay) as f32 / rate as f32).sin();
                error += f64::from(limited - expected).powi(2);
                clipped_error += f64::from((expected * 1.6).clamp(-1.0, 1.0) - expected).powi(2);
            }
        }
        assert!(raw_peak > 1.5);
        assert!(limited_peak > 0.99);
        assert!(error < clipped_error * 0.01, "rate={rate}, error={error}");
        assert!(tracks.buffers.values().all(|track| track.starved == 0));
    }
}

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
    let mut state = AppState::empty();
    let controls = AudioControls::from_state(&state);
    state.auto_gain_control = agc;
    state.noise_cancellation = false;
    state.mic_volume = gain as u16;
    state.mic_sensitivity = 10;
    direct::update_controls(&controls, &state);
    processed_controls(amplitudes, controls, muted)
}

fn processed_controls(
    amplitudes: &[f32],
    controls: AudioControls,
    muted: bool,
) -> Vec<crate::audio_queue::Frame<i16>> {
    let hops = amplitudes.iter().enumerate().map(|(hop, amplitude)| {
        std::array::from_fn(|i| {
            let phase = (hop * FRAME_SAMPLES + i) as f32 * std::f32::consts::TAU * 440.0
                / SAMPLE_RATE as f32;
            amplitude * phase.sin()
        })
    });
    processed_hops(hops.collect(), controls, muted)
}

fn processed_speech(pcm: &[f32], gain: i32) -> Vec<crate::audio_queue::Frame<i16>> {
    let mut state = AppState::empty();
    let controls = AudioControls::from_state(&state);
    state.auto_gain_control = true;
    state.noise_cancellation = false;
    state.mic_volume = gain as u16;
    state.mic_sensitivity = 10;
    direct::update_controls(&controls, &state);
    processed_hops(pcm.as_chunks::<FRAME_SAMPLES>().0.to_vec(), controls, false)
}

fn processed_hops(
    hops: Vec<[f32; FRAME_SAMPLES]>,
    controls: AudioControls,
    muted: bool,
) -> Vec<crate::audio_queue::Frame<i16>> {
    let (tx, rx) = crate::audio_queue::channel();
    // Room for every frame: the live queue drops when full, and a loaded test
    // run starving the reader was enough to lose frames.
    let (out, mut received) = tokio::sync::mpsc::channel(hops.len().max(1));
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
    for (index, samples) in hops.into_iter().enumerate() {
        // Stamped ahead so waiting behind a busy worker never ages a frame
        // past `MAX_AGE`; staleness is the device's concern, not the gate's.
        let frame = crate::audio_queue::Frame {
            samples,
            captured: std::time::Instant::now() + std::time::Duration::from_secs(60),
            position: Some(crate::audio_queue::CapturePosition {
                capture_id: 1,
                first_sample: (index * FRAME_SAMPLES) as u64,
            }),
        };
        tx.blocking_send(frame).unwrap();
    }
    drop(tx);
    worker.join().unwrap();
    reader.join().unwrap()
}

#[test]
#[ignore = "requires DISCORDIA_VAD_AUDIT_WAV and DISCORDIA_VAD_AUDIT_OUT for an offline local recording replay"]
fn recorded_voice_detection_replay_exports_audio_and_frame_decisions() {
    let path = std::env::var("DISCORDIA_VAD_AUDIT_WAV").unwrap();
    let dir = std::path::PathBuf::from(std::env::var("DISCORDIA_VAD_AUDIT_OUT").unwrap());
    let samples = super::speech_audit_tests::read_speech(std::path::Path::new(&path));
    let samples = &samples[..samples.len().min(SAMPLE_RATE as usize * 30)];
    let mut state = AppState::empty();
    let mut config =
        crate::mic_profile::Config::from_state(&state).preset(crate::mic_profile::Profile::Natural);
    config.processing.headphones = true;
    config.processing.echo = false;
    config.processing.gate = true;
    config.processing.voice_detection = true;
    config.threshold = 1;
    config.apply_state(&mut state);
    let controls = AudioControls::from_state(&state);
    let frames = processed_hops(
        samples.as_chunks::<FRAME_SAMPLES>().0.to_vec(),
        controls,
        false,
    );
    assert!(
        !frames.is_empty(),
        "No speech was passed from the supplied recording"
    );
    let mut output = vec![0.0_f32; samples.len()];
    let mut accepted = std::collections::HashSet::new();
    for frame in &frames {
        let first = frame.position.unwrap().first_sample as usize;
        accepted.insert(first / FRAME_SAMPLES);
        for (slot, sample) in output[first..first + FRAME_SAMPLES]
            .iter_mut()
            .zip(frame.samples)
        {
            *slot = sample as f32 / i16::MAX as f32;
        }
    }
    let mut vad = crate::denoise::Vad::new();
    let mut csv = String::from("time_s,input_dbfs,speech_probability,transmitted\n");
    for (index, frame) in samples.as_chunks::<FRAME_SAMPLES>().0.iter().enumerate() {
        use std::fmt::Write;
        writeln!(
            csv,
            "{:.2},{:.2},{:.4},{}",
            index as f32 / 100.0,
            crate::voice_record::rms_dbfs(frame),
            vad.speech(frame),
            accepted.contains(&index)
        )
        .unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    super::speech_audit_tests::write_wav(&dir.join("input.wav"), samples);
    super::speech_audit_tests::write_wav(&dir.join("voice-detection.wav"), &output);
    std::fs::write(dir.join("decisions.csv"), csv).unwrap();
    println!(
        "VAD replay: {} input frames, {} transmitted frames, {:.2} s retained; annotated listening still required",
        samples.len() / FRAME_SAMPLES,
        frames.len(),
        frames.len() as f32 / 100.0
    );
}

#[test]
fn voice_detection_uses_the_model_and_preserves_capture_order_for_quiet_speech() {
    for dbfs in [-30.0, -45.0] {
        let state = AppState::empty();
        let mut config = crate::mic_profile::Config::from_state(&state)
            .preset(crate::mic_profile::Profile::Natural);
        config.processing.echo = false;
        config.processing.gate = true;
        config.processing.voice_detection = true;
        config.threshold = 1;
        let controls = AudioControls::from_state(&state);
        controls.apply_mic(config);
        let speech = crate::agc::tests::speech(6.0, dbfs, 3);
        let mut samples = vec![0.0; SAMPLE_RATE as usize * 2];
        samples.extend_from_slice(&speech);
        samples.extend_from_slice(&vec![0.0; SAMPLE_RATE as usize * 2]);
        let voiced = processed_hops(
            samples.as_chunks::<FRAME_SAMPLES>().0.to_vec(),
            controls.clone(),
            false,
        );
        assert!(
            voiced.len() > 100,
            "voice was lost at {dbfs} dBFS: {} frames",
            voiced.len()
        );
        assert!(
            voiced
                .windows(2)
                .all(|pair| pair[0].position.unwrap().first_sample
                    < pair[1].position.unwrap().first_sample)
        );
        assert!(voiced.last().unwrap().position.unwrap().first_sample < SAMPLE_RATE as u64 * 9);
        let mut seed = 79_u32;
        let noise: Vec<f32> = (0..SAMPLE_RATE as usize * 10)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed as f32 / u32::MAX as f32 * 2.0 - 1.0) * 0.02
            })
            .collect();
        let unvoiced = processed_hops(
            noise.as_chunks::<FRAME_SAMPLES>().0.to_vec(),
            controls.clone(),
            false,
        );
        assert!(
            unvoiced.len() < voiced.len() / 2,
            "noise was transmitted as speech: {} vs {}",
            unvoiced.len(),
            voiced.len()
        );
        assert!(
            processed_hops(
                samples.as_chunks::<FRAME_SAMPLES>().0.to_vec(),
                controls,
                true
            )
            .is_empty()
        );
    }
}

#[test]
fn native_pipeline_honors_mute_and_zero_microphone_volume() {
    for (gain, muted) in [(100, true), (0, false)] {
        assert!(processed(&[0.2; 150], gain, false, muted).is_empty());
    }
}

#[test]
fn sensitivity_gates_before_microphone_volume_and_automatic_gain() {
    for agc in [false, true] {
        for gain in [50, 200] {
            let mut state = AppState::empty();
            state.noise_cancellation = false;
            state.auto_gain_control = agc;
            state.mic_volume = gain;
            state.mic_sensitivity = 30;
            let controls = AudioControls::from_state(&state);
            assert!(processed_controls(&[0.015; 200], controls, false).is_empty());
            state.mic_sensitivity = 1;
            let controls = AudioControls::from_state(&state);
            let frames = processed_controls(&[0.015; 200], controls, false);
            assert!(frames.len() > 100 && rms(&frames) > 0.001);
        }
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
        eprintln!("DM microphone gain (AGC {agc}), RMS at 50/100/200%: {levels:?}");
    }
}

#[test]
fn dm_playback_volume_changes_output_without_changing_microphone_gain() {
    let mut state = AppState::empty();
    state.mic_volume = 150;
    let controls = AudioControls::from_state(&state);
    let handle = PlaybackHandle {
        tracks: Arc::new(Mutex::new(MixerTracks::default())),
        device_rate: Arc::new(AtomicU32::new(48000)),
        gains: controls.gains.clone(),
        stream_gains: controls.stream_gains.clone(),
        soundboard_pct: controls.soundboard_pct.clone(),
    };
    let id = handle.add_track("peer".into(), TrackKind::Voice);
    let mut measure = |volume, muted, deafened| {
        state.user_volumes.insert("peer".into(), volume);
        if muted {
            state.user_muted.insert("peer".into());
        } else {
            state.user_muted.remove("peer");
        }
        state.voice.deafened = deafened;
        direct::update_controls(&controls, &state);
        assert_eq!(controls.mic_gain_pct.load(Ordering::Relaxed), 150);
        handle.push(id, &[0.1; 480], SAMPLE_RATE, 480);
        let mut tracks = handle.tracks.lock();
        refresh_gains(
            &mut tracks,
            &controls.gains,
            &controls.stream_gains,
            &controls.soundboard_pct,
            &controls.deafened,
        );
        let power: f32 = (0..480)
            .map(|_| next_playback_sample(&mut tracks).powi(2))
            .sum();
        (power / 480.0).sqrt()
    };
    let levels = [50, 100, 200].map(|volume| measure(volume, false, false));
    eprintln!("DM playback RMS at 50/100/200%: {levels:?}");
    for (actual, expected) in levels.into_iter().zip([0.05, 0.1, 0.2]) {
        assert!((actual - expected).abs() < 0.001);
    }
    assert_eq!(measure(0, false, false), 0.0);
    assert_eq!(measure(200, true, false), 0.0);
    assert_eq!(measure(200, false, true), 0.0);
    assert!((measure(200, false, false) - 0.2).abs() < 0.001);
}

#[tokio::test]
async fn cancelling_remote_playback_discards_its_buffer_before_rewatching() {
    let controls = AudioControls::from_state(&AppState::empty());
    let handle = PlaybackHandle {
        tracks: Arc::new(Mutex::new(MixerTracks::default())),
        device_rate: Arc::new(AtomicU32::new(48000)),
        gains: controls.gains.clone(),
        stream_gains: controls.stream_gains.clone(),
        soundboard_pct: controls.soundboard_pct.clone(),
    };
    let id = handle.add_track("alice#video".into(), TrackKind::Stream);
    handle.push(id, &[0.5; 480], SAMPLE_RATE, 9600);
    let guard = RemotePlaybackGuard(handle.clone(), id);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let _guard = guard;
        let _ = ready_tx.send(());
        std::future::pending::<()>().await;
    });
    ready_rx.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(handle.tracks.lock().buffers.is_empty());
    let fresh = handle.add_track("alice#video".into(), TrackKind::Stream);
    assert!(handle.tracks.lock().buffers[&fresh].samples.is_empty());
}

#[test]
fn native_pipeline_limits_hot_microphones_and_recovers() {
    let speech = crate::agc::tests::speech;
    let mut signal = speech(4.0, -34.0, 1);
    signal.extend(speech(1.0, -3.0, 2));
    signal.extend(speech(10.0, -26.0, 3));
    let frames = processed_speech(&signal, 100);
    assert!(frames.len() > 500);
    let peak = frames
        .iter()
        .flat_map(|f| f.samples)
        .map(|s| s.unsigned_abs())
        .max()
        .unwrap();
    assert!(peak as f32 <= 0.981 * i16::MAX as f32);
    let settled = 20.0 * rms(&frames[frames.len() - 300..]).log10();
    assert!(
        (-22.0..-12.0).contains(&settled),
        "settled at {settled:.1} dBFS"
    );
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
                starved: 0,
                drift_correction: true,
                recovery: playback_recovery::Recovery::new(SAMPLE_RATE),
                source_rate: SAMPLE_RATE,
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
fn muting_a_watched_stream_silences_queued_audio_and_restores_its_volume() {
    let mut state = AppState::empty();
    state.screen_viewing.insert("alice".into());
    state.stream_volumes.insert("alice".into(), 50);
    let controls = AudioControls::from_state(&state);
    let mut tracks = MixerTracks::default();
    tracks.buffers.insert(
        1,
        TrackBuf {
            samples: std::collections::VecDeque::from(vec![0.4; 480]),
            identity: "alice#video".into(),
            gain: 1.0,
            kind: TrackKind::Stream,
            starved: 0,
            drift_correction: true,
            recovery: playback_recovery::Recovery::new(SAMPLE_RATE),
            source_rate: SAMPLE_RATE,
        },
    );
    state.stream_muted.insert("alice".into());
    controls.sync_stream_gains(&state);
    refresh_gains(
        &mut tracks,
        &controls.gains,
        &controls.stream_gains,
        &controls.soundboard_pct,
        &controls.deafened,
    );
    assert_eq!(next_playback_sample(&mut tracks), 0.0);
    assert_eq!(tracks.buffers[&1].samples.len(), 479);
    assert!(controls.watched_streams.borrow().contains("alice"));
    state.stream_muted.clear();
    controls.sync_stream_gains(&state);
    refresh_gains(
        &mut tracks,
        &controls.gains,
        &controls.stream_gains,
        &controls.soundboard_pct,
        &controls.deafened,
    );
    assert_eq!(next_playback_sample(&mut tracks), 0.2);
}

#[test]
fn closed_gate_room_tone_does_not_raise_the_next_utterance() {
    let speech = crate::agc::tests::speech;
    let mut signal = speech(10.0, -30.0, 4);
    let settled = rms(&processed_speech(&signal, 100)[400..]);
    signal
        .extend((0..5 * SAMPLE_RATE as usize).map(|i| {
            0.004 * (i as f32 * std::f32::consts::TAU * 120.0 / SAMPLE_RATE as f32).sin()
        }));
    signal.extend(speech(1.5, -30.0, 5));
    let frames = processed_speech(&signal, 100);
    let resumed = rms(&frames[frames.len() - 60..]);
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
        device_rate: Arc::new(AtomicU32::new(48000)),
        gains: controls.gains,
        stream_gains: controls.stream_gains,
        soundboard_pct: controls.soundboard_pct,
    };
    let id = handle.add_track("alice".into(), TrackKind::Voice);
    let cap = 48000 / PLAYBACK_CAP_DIVISOR as usize;
    handle.push(id, &vec![0.1; cap * 2], SAMPLE_RATE, cap);
    handle.push(id, &[0.7; 480], SAMPLE_RATE, cap);
    {
        let tracks = handle.tracks.lock();
        let samples = &tracks.buffers[&id].samples;
        let target = 48000 * drift::TARGET_MS as usize / 1000;
        assert_eq!(samples.len(), target + 480);
        assert!(samples.iter().rev().take(480).all(|s| *s == 0.7));
    }
    handle.remove_track(id);
    assert!(handle.tracks.lock().buffers.is_empty());
}

fn replay_stalled_delivery(rate: u32, recover: bool) -> (usize, u64, u64) {
    let controls = AudioControls::from_state(&AppState::empty());
    let handle = PlaybackHandle {
        tracks: Arc::new(Mutex::new(MixerTracks::default())),
        device_rate: Arc::new(AtomicU32::new(rate)),
        gains: controls.gains,
        stream_gains: controls.stream_gains,
        soundboard_pct: controls.soundboard_pct,
    };
    let id = handle.add_track("peer".into(), TrackKind::Voice);
    let hop = rate as usize / 100;
    let cap = rate as usize / PLAYBACK_CAP_DIVISOR as usize;
    handle.push(id, &vec![0.2; 4 * hop], rate, cap);
    let frame = vec![0.2; hop];
    let mut next_hop = 0;
    let mut silent_runs = 0;
    let mut was_silent = false;
    for sample in 0..rate as usize * 2 {
        loop {
            let due = if next_hop < rate as usize {
                next_hop
            } else if next_hop < rate as usize * 11 / 10 {
                rate as usize * 1064 / 1000 + 2 * (next_hop - rate as usize)
            } else {
                next_hop.max(rate as usize * 1264 / 1000)
            };
            if due > sample {
                break;
            }
            handle.push(id, &frame, rate, cap);
            next_hop += hop;
        }
        let mut tracks = handle.tracks.lock();
        if !recover {
            tracks.buffers.get_mut(&id).unwrap().recovery = playback_recovery::Recovery::new(rate);
        }
        let value = next_playback_sample(&mut tracks);
        let silent = value == 0.0;
        silent_runs += usize::from(silent && !was_silent);
        was_silent = silent;
        // A resume fades in, so anything between silence and the level is a ramp sample.
        assert!((0.0..=0.2 + 1e-6).contains(&value), "{value}");
        assert!(tracks.buffers[&id].samples.len() <= cap);
    }
    let tracks = handle.tracks.lock();
    let track = &tracks.buffers[&id];
    (silent_runs, track.starved, track.recovery.held_samples)
}

#[test]
fn recovery_avoids_chopping_audio_while_a_stalled_producer_catches_up() {
    for rate in [44_100, 48_000, 96_000] {
        let baseline = replay_stalled_delivery(rate, false);
        let recovered = replay_stalled_delivery(rate, true);
        eprintln!("rate={rate}: before={baseline:?}, after={recovered:?}");
        assert!(
            baseline.0 > 1,
            "the stalled producer must reproduce repeated cuts"
        );
        assert!(
            recovered.0 * 2 <= baseline.0,
            "rate={rate}: recovery must reduce fragmented output"
        );
        assert!(recovered.1 < baseline.1);
        assert!(recovered.2 > 0);
        assert!(
            recovered.1 + recovered.2
                <= baseline.1 + u64::from(rate) * u64::from(playback_recovery::RESERVE_MS) / 1000
        );
    }
}

#[test]
fn output_rate_changes_flush_old_pcm_and_reject_in_flight_old_rate_blocks() {
    for (old, new) in [(48_000, 96_000), (96_000, 44_100), (44_100, 48_000)] {
        let controls = AudioControls::from_state(&AppState::empty());
        let handle = PlaybackHandle {
            tracks: Arc::new(Mutex::new(MixerTracks::default())),
            device_rate: Arc::new(AtomicU32::new(old)),
            gains: controls.gains,
            stream_gains: controls.stream_gains,
            soundboard_pct: controls.soundboard_pct,
        };
        for (kind, drift) in [
            (TrackKind::Voice, true),
            (TrackKind::Stream, true),
            (TrackKind::Soundboard, true),
            (TrackKind::Voice, false),
        ] {
            let id = handle.insert_track("peer".into(), kind, drift);
            handle.push(id, &[0.2; 480], old, old as usize).unwrap();
        }
        handle.set_rate(new);
        let ids: Vec<_> = handle.tracks.lock().buffers.keys().copied().collect();
        for id in ids {
            assert_eq!(handle.queued_samples(id), Some(0));
            assert!(handle.push(id, &[0.2; 480], old, old as usize).is_none());
            handle.push(id, &[0.3; 480], new, new as usize).unwrap();
            handle.set_rate(new);
            assert_eq!(handle.queued_samples(id), Some(480));
            assert!(
                handle.tracks.lock().buffers[&id]
                    .samples
                    .iter()
                    .all(|sample| *sample == 0.3)
            );
        }
    }
}

#[test]
fn soundboard_restarts_immediately_after_short_and_long_pauses() {
    for rate in [44_100, 48_000, 96_000] {
        let controls = AudioControls::from_state(&AppState::empty());
        controls.soundboard_pct.store(100, Ordering::Relaxed);
        let handle = PlaybackHandle {
            tracks: Arc::new(Mutex::new(MixerTracks::default())),
            device_rate: Arc::new(AtomicU32::new(rate)),
            gains: controls.gains,
            stream_gains: controls.stream_gains,
            soundboard_pct: controls.soundboard_pct,
        };
        let id = handle.add_track("peer".into(), TrackKind::Soundboard);
        let mut tracks = handle.tracks.lock();
        for (gap_ms, value) in [(0, 0.2), (20, 0.3), (200, 0.4), (20, 0.5)] {
            for _ in 0..rate * gap_ms / 1000 {
                assert_eq!(next_playback_sample(&mut tracks), 0.0);
            }
            tracks
                .buffers
                .get_mut(&id)
                .unwrap()
                .samples
                .push_back(value);
            assert_eq!(next_playback_sample(&mut tracks), value);
        }
        assert_eq!(tracks.buffers[&id].recovery.held_samples, 0);
    }
}

#[test]
fn live_playback_restarts_immediately_after_a_long_idle() {
    for rate in [44_100, 48_000, 96_000] {
        let mut recovery = playback_recovery::Recovery::new(rate);
        assert!(recovery.ready(1));
        for _ in 0..rate {
            recovery.ready(0);
        }
        assert!(
            recovery.ready(1),
            "rate={rate}: a new utterance must not wait for the previous one"
        );
        assert_eq!(recovery.held_samples, 0);
    }
}

#[test]
fn recovery_does_not_delay_startup_or_trap_a_short_final_fragment() {
    for rate in [44_100, 48_000, 96_000] {
        let mut recovery = playback_recovery::Recovery::new(rate);
        for _ in 0..rate / 10 {
            assert!(recovery.ready(0));
        }
        assert!(recovery.ready(1));
        assert!(!recovery.ready(0));
        let reserve = rate as usize * playback_recovery::RESERVE_MS as usize / 1000;
        let mut held = 0;
        while !recovery.ready(1) {
            held += 1;
            assert!(held <= reserve);
        }
        assert_eq!(held, reserve);
        assert_eq!(recovery.held_samples, held as u64);
    }
}

#[test]
fn recorded_playback_bypasses_recovery_and_output_rate_changes_reset_its_wait() {
    let mut recovery = playback_recovery::Recovery::new(48_000);
    assert!(recovery.ready(1));
    assert!(!recovery.ready(0));
    assert!(!recovery.ready(1));
    recovery.set_rate(96_000);
    assert!(recovery.ready(0));
    assert!(recovery.ready(1));
    let mut tracks = MixerTracks::default();
    tracks.buffers.insert(
        0,
        TrackBuf {
            samples: [0.2].into(),
            identity: "recorded".into(),
            gain: 1.0,
            kind: TrackKind::Voice,
            starved: 0,
            drift_correction: false,
            recovery,
            source_rate: SAMPLE_RATE,
        },
    );
    assert_eq!(next_playback_sample(&mut tracks), 0.2);
    assert_eq!(next_playback_sample(&mut tracks), 0.0);
    tracks.buffers.get_mut(&0).unwrap().samples.push_back(0.3);
    assert_eq!(next_playback_sample(&mut tracks), 0.3);
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
