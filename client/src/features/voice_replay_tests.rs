use super::*;
use crate::denoise::Engine;
use crate::mic_profile::Config;
use std::io::{BufWriter, Write};

struct SpeechProtection {
    floor_db: f32,
    sustained: u32,
    hold: u32,
    cap_db: f32,
}

impl SpeechProtection {
    fn new(cap_db: u32) -> Self {
        Self {
            floor_db: -60.0,
            sustained: 0,
            hold: 0,
            cap_db: cap_db as f32,
        }
    }

    fn step(&mut self, level_db: f32, speech: f32, requested: u32) -> u32 {
        if level_db < self.floor_db + 8.0 {
            self.floor_db += (level_db - self.floor_db) * 0.01;
        }
        let strong = level_db > -44.0 && level_db > self.floor_db + 18.0;
        let candidate = strong && (speech >= 0.6 || level_db > self.floor_db + 24.0);
        self.sustained = if candidate {
            (self.sustained + 1).min(12)
        } else {
            self.sustained.saturating_sub(1)
        };
        if self.sustained == 12 {
            self.hold = 35;
        } else {
            self.hold = self.hold.saturating_sub(1);
        }
        let target = if self.hold > 0 { 3.0 } else { requested as f32 };
        let delta: f32 = if target < self.cap_db { -0.6 } else { 0.1 };
        self.cap_db = if (target - self.cap_db).abs() < delta.abs() {
            target
        } else {
            self.cap_db + delta
        };
        self.cap_db.round().clamp(3.0, requested as f32) as u32
    }
}

#[test]
fn experimental_protection_rejects_quiet_false_positives_and_short_impulses() {
    let mut protection = SpeechProtection::new(12);
    for _ in 0..100 {
        assert_eq!(protection.step(-54.0, 1.0, 12), 12);
    }
    for _ in 0..4 {
        assert_eq!(protection.step(-25.0, 1.0, 12), 12);
    }
    for _ in 0..100 {
        assert_eq!(protection.step(-54.0, 0.0, 12), 12);
    }
    // Both detectors can miss speech; sustained energy above the local floor is the fallback.
    for _ in 0..60 {
        protection.step(-28.0, 0.05, 12);
    }
    assert_eq!(protection.step(-28.0, 0.05, 12), 3);
    let mut last = 3;
    for _ in 0..200 {
        let next = protection.step(-54.0, 0.0, 12);
        assert!(next >= last && next - last <= 1);
        last = next;
    }
    assert_eq!(last, 12);
}

#[test]
#[ignore = "requires a local PCM microphone WAV via DISCORDIA_AUDIO_REPLAY_WAV; exports private comparisons"]
fn recorded_microphone_filter_comparison() {
    let input = std::path::PathBuf::from(
        std::env::var_os("DISCORDIA_AUDIO_REPLAY_WAV").expect("input WAV"),
    );
    let output = std::path::PathBuf::from(
        std::env::var_os("DISCORDIA_AUDIO_REPLAY_OUTPUT").expect("output directory"),
    );
    std::fs::create_dir_all(&output).unwrap();
    let samples = super::speech_audit_tests::read_speech(&input);
    assert!(
        samples.len() >= SAMPLE_RATE as usize * 5,
        "at least five seconds of input"
    );
    assert!(
        samples.len().is_multiple_of(FRAME_SAMPLES),
        "whole 10 ms hops"
    );
    let mut metadata = Vec::new();
    for (name, engine, suppression, gate, warm) in [
        ("baseline", None, 12, false, false),
        ("deep-6", Some(Engine::DeepFilterNet), 6, true, false),
        ("deep-12", Some(Engine::DeepFilterNet), 12, true, false),
        ("rnnoise-6", Some(Engine::RNNoise), 6, true, false),
        ("rnnoise-12", Some(Engine::RNNoise), 12, true, false),
        (
            "deep-12-no-gate",
            Some(Engine::DeepFilterNet),
            12,
            false,
            false,
        ),
        ("deep-12-warm", Some(Engine::DeepFilterNet), 12, true, true),
        (
            "deep-12-protected-experimental",
            Some(Engine::DeepFilterNet),
            12,
            true,
            false,
        ),
    ] {
        let (_, reference) = crate::audio_queue::channel();
        let mut processor = dsp::Processor::new(reference);
        if let Some(engine) = engine {
            processor.replay_engine(engine);
        }
        let mut config = Config::from_state(&AppState::empty());
        config.processing.headphones = true;
        config.processing.echo = false;
        config.processing.high_pass = true;
        config.processing.gate = gate;
        config.threshold = 2;
        config.volume = 125;
        config.agc = false;
        config.denoise = engine.is_some();
        config.suppression = suppression;
        if warm {
            for hop in samples[..SAMPLE_RATE as usize * 5]
                .as_chunks::<FRAME_SAMPLES>()
                .0
            {
                processor.process(*hop, config, 0, false);
            }
        }
        let mut csv =
            BufWriter::new(std::fs::File::create(output.join(format!("{name}.csv"))).unwrap());
        writeln!(
            csv,
            "t_ms,raw_dbfs,after_echo_dbfs,after_denoise_dbfs,sent_dbfs,gate,speech,lsnr_db,suppression_db"
        )
        .unwrap();
        let mut result = Vec::with_capacity(samples.len());
        let started = std::time::Instant::now();
        let protected = name.ends_with("-experimental");
        let mut protection = SpeechProtection::new(suppression);
        let mut previous_speech = 0.0;
        for (index, hop) in samples.as_chunks::<FRAME_SAMPLES>().0.iter().enumerate() {
            if protected {
                config.suppression = protection.step(
                    crate::voice_record::rms_dbfs(hop),
                    previous_speech,
                    suppression,
                );
            }
            let processed = processor.process(*hop, config, 0, false);
            previous_speech = processed.speech;
            assert_eq!(processed.engine, engine, "{name}: engine changed");
            let sent = if processed.action == GateAction::Drop {
                [0.0; FRAME_SAMPLES]
            } else {
                processed.samples
            };
            assert!(
                sent.iter()
                    .all(|sample| sample.is_finite() && sample.abs() <= 0.980_001)
            );
            writeln!(
                csv,
                "{},{:.3},{:.3},{:.3},{:.3},{:?},{:.4},{},{}",
                index * 10,
                crate::voice_record::rms_dbfs(hop),
                processed.echoed_dbfs,
                processed.denoised_dbfs,
                crate::voice_record::rms_dbfs(&sent),
                processed.action,
                processed.speech,
                processed
                    .lsnr_db
                    .map_or_else(String::new, |v| format!("{v:.3}")),
                config.suppression
            )
            .unwrap();
            result.extend_from_slice(&sent);
        }
        csv.flush().unwrap();
        super::speech_audit_tests::write_wav(&output.join(format!("{name}.wav")), &result);
        let elapsed = started.elapsed().as_secs_f64();
        println!(
            "{name}: {:.2} s audio, {elapsed:.3} s offline processing",
            samples.len() as f64 / SAMPLE_RATE as f64
        );
        metadata.push(serde_json::json!({"name": name, "engine": engine.map(Engine::label), "suppression_db": suppression,
            "gate": gate, "warmup_prefix_seconds": if warm {5} else {0}, "manual_volume": 125, "agc": false,
            "echo": false, "high_pass": true, "threshold": 2, "offline_seconds": elapsed, "experimental_protection": protected}));
    }
    std::fs::write(output.join("replay.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "sample_rate": SAMPLE_RATE, "samples": samples.len(), "candidates": metadata,
        "notes": "Offline DSP comparison; timing-triggered fallback disabled. Warmup repeats the first five seconds; original pre-recording state is unavailable."
    })).unwrap()).unwrap();
}
