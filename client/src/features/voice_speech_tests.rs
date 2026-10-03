use super::*;
use std::path::Path;
use std::time::{Duration, Instant};

fn read_speech(path: &Path) -> Vec<f32> {
    let bytes = std::fs::read(path).expect("Open Speech Repository PCM WAV fixture");
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    let mut offset = 12;
    let mut data = None;
    let mut rate = None;
    while offset + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start.checked_add(size).unwrap();
        assert!(end <= bytes.len());
        match &bytes[offset..offset + 4] {
            b"fmt " => {
                assert!(size >= 16);
                assert_eq!(
                    u16::from_le_bytes(bytes[start..start + 2].try_into().unwrap()),
                    1
                );
                assert_eq!(
                    u16::from_le_bytes(bytes[start + 2..start + 4].try_into().unwrap()),
                    1
                );
                assert_eq!(
                    u16::from_le_bytes(bytes[start + 14..start + 16].try_into().unwrap()),
                    16
                );
                rate = Some(u32::from_le_bytes(
                    bytes[start + 4..start + 8].try_into().unwrap(),
                ));
            }
            b"data" => data = Some(&bytes[start..end]),
            _ => {}
        }
        offset = end + (size & 1);
    }
    let rate = rate.unwrap();
    let pcm: Vec<_> = data
        .unwrap()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|s| i16::from_le_bytes(*s) as f32 / i16::MAX as f32)
        .collect();
    if rate == SAMPLE_RATE {
        return pcm;
    }
    let mut resampler = AudioResampler::new(rate, SAMPLE_RATE).unwrap();
    let mut result = Vec::new();
    let mut out = Vec::new();
    for chunk in pcm.chunks((rate / 100) as usize) {
        resampler.process_into(chunk, &mut out);
        result.extend_from_slice(&out);
    }
    result
}

fn write_wav(path: &Path, samples: &[f32]) {
    let size = (samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(size as usize + 44);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + size).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&size.to_le_bytes());
    for &sample in samples {
        bytes
            .extend_from_slice(&((sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

fn run_speech(mic: &[f32], far: &[f32], denoise: bool, agc: bool, threshold: i32) -> Vec<f32> {
    let controls = AudioControls::from_state(&AppState::empty());
    controls.denoise.store(denoise, Ordering::Relaxed);
    controls.agc.store(agc, Ordering::Relaxed);
    controls.threshold.store(threshold, Ordering::Relaxed);
    let stats = Arc::new(GateStats::default());
    let (tx, rx) = crate::audio_queue::channel();
    let (out, mut received) = crate::audio_queue::channel();
    let (reference, reference_rx) = crate::audio_queue::channel();
    let reader = std::thread::spawn(move || {
        let mut frames = Vec::new();
        while let Some(frame) = received.blocking_recv() {
            frames.push(frame);
        }
        frames
    });
    let worker_stats = stats.clone();
    let worker = std::thread::spawn(move || {
        denoise_gate_loop(
            rx,
            out,
            controls,
            Arc::new(MicMeter::default()),
            Arc::new(AtomicBool::new(false)),
            worker_stats,
            reference_rx,
        )
    });
    if denoise {
        tx.blocking_send(crate::audio_queue::Frame::new([0.0; FRAME_SAMPLES]))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while stats.atten_lim_applied.load(Ordering::Relaxed) == 0 {
            assert!(Instant::now() < deadline, "model load timeout");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    let start = Instant::now();
    let mut indices = HashMap::new();
    for (hop, input) in mic.as_chunks::<FRAME_SAMPLES>().0.iter().enumerate() {
        let deadline = start + Duration::from_millis(hop as u64 * 10);
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        reference
            .blocking_send(crate::audio_queue::Frame::new(std::array::from_fn(|i| {
                (far[hop * FRAME_SAMPLES + i] * i16::MAX as f32) as i16
            })))
            .unwrap();
        let packet = crate::audio_queue::Frame::new(*input);
        indices.insert(packet.captured, hop);
        tx.blocking_send(packet).unwrap();
    }
    drop(tx);
    drop(reference);
    worker.join().unwrap();
    let mut result = vec![0.0; mic.len()];
    for packet in reader.join().unwrap() {
        if let Some(&hop) = indices.get(&packet.captured) {
            for (out, sample) in result[hop * FRAME_SAMPLES..][..FRAME_SAMPLES]
                .iter_mut()
                .zip(packet.samples)
            {
                *out = sample as f32 / i16::MAX as f32;
            }
        }
    }
    assert!(result.iter().all(|s| s.is_finite() && s.abs() <= 0.981));
    result
}

fn power(samples: &[f32]) -> f64 {
    samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len().max(1) as f64
}

fn speech_alignment(clean: &[f32], output: &[f32], start: usize) -> (usize, f64) {
    let correlation = |delay| {
        let (mut xy, mut xx, mut yy) = (0.0f64, 0.0f64, 0.0f64);
        for i in (start..clean.len() - 2400).step_by(8) {
            let x = clean[i] as f64;
            let y = output[i + delay] as f64;
            xy += x * y;
            xx += x * x;
            yy += y * y;
        }
        xy / (xx * yy).sqrt().max(1e-12)
    };
    let mut best = (0usize, 0.0);
    for delay in (0..=2400).step_by(48) {
        let value = correlation(delay);
        if value > best.1 {
            best = (delay, value);
        }
    }
    for delay in best.0.saturating_sub(48)..=(best.0 + 48).min(2400) {
        let value = correlation(delay);
        if value > best.1 {
            best = (delay, value);
        }
    }
    best
}

#[test]
#[ignore = "requires exported human_speech_audit WAV comparisons"]
fn human_speech_saved_metrics() {
    let dir = std::path::PathBuf::from(std::env::var_os("DIOXUSFUN_SPEECH_FIXTURES").unwrap());
    let clean = read_speech(&dir.join("speech-clean.wav"));
    for name in [
        "baseline",
        "clean-denoise",
        "aec",
        "aec-denoise",
        "aec-denoise-agc",
    ] {
        let output = read_speech(&dir.join(format!("speech-{name}.wav")));
        let (delay, correlation) = speech_alignment(&clean, &output, 0);
        let active: Vec<_> = clean
            .as_chunks::<FRAME_SAMPLES>()
            .0
            .iter()
            .enumerate()
            .filter(|(i, chunk)| {
                (*i + 1) * FRAME_SAMPLES + delay <= output.len() && power(*chunk) > 0.0001
            })
            .map(|(i, _)| i)
            .collect();
        let lost = active
            .iter()
            .filter(|&&i| power(&output[i * FRAME_SAMPLES + delay..][..FRAME_SAMPLES]) < 0.000001)
            .count();
        println!(
            "saved speech {name}: delay {:.2} ms, correlation {correlation:.3}, active silence {lost}/{}",
            delay as f64 / 48.0,
            active.len()
        );
    }
}

#[test]
#[ignore = "requires DIOXUSFUN_SPEECH_FIXTURES containing Open Speech Repository female.wav and male.wav"]
fn human_speech_audit() {
    let dir = std::path::PathBuf::from(
        std::env::var_os("DIOXUSFUN_SPEECH_FIXTURES").expect("fixture directory"),
    );
    let female = read_speech(&dir.join("female.wav"));
    let male = read_speech(&dir.join("male.wav"));
    let warmup = 3 * SAMPLE_RATE as usize;
    let len = warmup + 10 * SAMPLE_RATE as usize;
    assert!(female.len() >= len && male.len() >= len);
    let clean: Vec<_> = (0..len)
        .map(|i| if i < warmup { 0.0 } else { female[i - warmup] })
        .collect();
    let far = &male[..len];
    let mut seed = 79u32;
    let noisy: Vec<_> = (0..len)
        .map(|i| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed as f32 / u32::MAX as f32 - 0.5) * 0.025;
            let hum = 0.006 * (i as f32 * std::f32::consts::TAU * 60.0 / SAMPLE_RATE as f32).sin();
            let echo = if i >= 2400 { far[i - 2400] * 0.5 } else { 0.0 };
            clean[i] + noise + hum + echo
        })
        .collect();
    write_wav(&dir.join("speech-clean.wav"), &clean[warmup..]);
    write_wav(&dir.join("speech-noisy-echo.wav"), &noisy[warmup..]);
    let silence = vec![0.0; len];
    let baseline = run_speech(&clean, &silence, false, false, 10);
    write_wav(&dir.join("speech-baseline.wav"), &baseline[warmup..]);
    let mut failures = Vec::new();
    let selected = std::env::var("DIOXUSFUN_SPEECH_CASE").ok();
    for (name, input, reference, denoise, agc, threshold) in [
        ("clean-denoise", &clean, &silence[..], true, false, 10),
        ("aec", &noisy, far, false, false, 10),
        ("aec-denoise", &noisy, far, true, false, 10),
        ("aec-denoise-agc", &noisy, far, true, true, 10),
        ("aec-denoise-low-gate", &noisy, far, true, true, 1),
    ] {
        if selected.as_deref().is_some_and(|selected| selected != name) {
            continue;
        }
        let output = run_speech(input, reference, denoise, agc, threshold);
        write_wav(&dir.join(format!("speech-{name}.wav")), &output[warmup..]);
        let (delay, correlation) = speech_alignment(&clean, &output, warmup);
        let speech_power = power(&output[warmup..]);
        let baseline_power = power(&baseline[warmup..]);
        let active: Vec<_> = clean
            .as_chunks::<FRAME_SAMPLES>()
            .0
            .iter()
            .enumerate()
            .filter(|(i, chunk)| {
                *i * FRAME_SAMPLES >= warmup
                    && (*i + 1) * FRAME_SAMPLES + delay <= output.len()
                    && power(*chunk) > 0.0001
            })
            .map(|(i, _)| i)
            .collect();
        let silent_active = active
            .iter()
            .filter(|&&i| power(&output[i * FRAME_SAMPLES + delay..][..FRAME_SAMPLES]) < 0.000001)
            .count();
        println!(
            "human speech {name}: output/baseline {:.2} dB, aligned active hops silent {silent_active}/{}, warmup residual RMS {:.4}, delay {} ms, correlation {correlation:.3}",
            10.0 * (speech_power / baseline_power).log10(),
            active.len(),
            power(&output[SAMPLE_RATE as usize..warmup]).sqrt(),
            delay / 48
        );
        if speech_power <= baseline_power * 0.1
            || silent_active >= active.len() / 5
            || correlation < 0.4
        {
            failures.push(name);
        }
    }
    assert!(
        failures.is_empty(),
        "speech retention failures: {failures:?}"
    );
}
