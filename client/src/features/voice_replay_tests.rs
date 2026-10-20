use super::*;
use crate::denoise::Engine;
use crate::mic_profile::Config;
use std::io::{BufWriter, Write};

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
        ("deep-12-agc", Some(Engine::DeepFilterNet), 12, true, false),
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
        config.agc = name.ends_with("-agc");
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
        for (index, hop) in samples.as_chunks::<FRAME_SAMPLES>().0.iter().enumerate() {
            let processed = processor.process(*hop, config, 0, false);
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
                processed.suppression_db
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
            "gate": gate, "warmup_prefix_seconds": if warm {5} else {0}, "manual_volume": 125, "agc": config.agc,
            "echo": false, "high_pass": true, "threshold": 2, "offline_seconds": elapsed, "speech_protection": engine == Some(Engine::DeepFilterNet)}));
    }
    std::fs::write(output.join("replay.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "sample_rate": SAMPLE_RATE, "samples": samples.len(), "candidates": metadata,
        "notes": "Offline DSP comparison; timing-triggered fallback disabled. Warmup repeats the first five seconds; original pre-recording state is unavailable."
    })).unwrap()).unwrap();
}
