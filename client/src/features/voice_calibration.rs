use super::*;
use crate::mic_profile::{Config, Context, Profile};

pub(super) const SECONDS: u64 = 15;

pub(super) struct Hop {
    pub at: f32,
    pub samples: [f32; FRAME_SAMPLES],
}

pub(super) struct Analysis {
    pub config: Config,
    pub current: Vec<f32>,
    pub recommended: Vec<f32>,
    pub limited_gain: bool,
    pub background_noise: bool,
    pub soft_speech_unmeasured: bool,
}

pub(super) fn context(state: &AppState) -> Result<Context, String> {
    let host = cpal::default_host();
    if let Some(selected) = &state.selected_input_device {
        let count = host
            .input_devices()
            .map_err(|e| e.to_string())?
            .filter(|d| crate::audio_device::name(d).ok().as_deref() == Some(selected.as_str()))
            .count();
        if count > 1 {
            return Err(
                "Several microphones have the same name. Choose a unique device before adjusting."
                    .into(),
            );
        }
    }
    let input = pick_device(
        state.selected_input_device.as_deref(),
        host.default_input_device(),
        host.input_devices().ok(),
    )
    .ok_or_else(|| "No microphone is available. Connect one and try again.".to_string())?;
    let output = pick_device(
        state.selected_output_device.as_deref(),
        host.default_output_device(),
        host.output_devices().ok(),
    )
    .ok_or_else(|| "No audio output is available for the microphone test.".to_string())?;
    let identity = |device: cpal::Device| -> Result<String, String> {
        let id = device.id().map_err(|e| e.to_string())?;
        Ok(format!("{:?}:{}", id.host(), id.id()))
    };
    Ok(Context {
        input: identity(input)?,
        output: identity(output)?,
        bypass_requested: state.bypass_system_audio_processing,
        raw_capture: state.bypass_system_audio_processing
            && crate::rawmic::supported()
            && state.mic_bypass_error.is_none(),
        headphones: state.mic_processing.headphones,
    })
}

fn percentile(values: &mut [f32], fraction: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable_by(f32::total_cmp);
    values[((values.len() - 1) as f32 * fraction) as usize]
}

struct Metrics {
    noise_peaks: Vec<f32>,
    noise_rms: Vec<f32>,
    speech_rms: Vec<f32>,
    soft_peaks: Vec<f32>,
    peaks: Vec<f32>,
}

impl Metrics {
    fn new() -> Self {
        Self {
            noise_peaks: vec![],
            noise_rms: vec![],
            speech_rms: vec![],
            soft_peaks: vec![],
            peaks: vec![],
        }
    }

    fn push(&mut self, at: f32, samples: &[f32], peak: f32) {
        let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
        if (0.5..3.0).contains(&at) || (13.0..15.0).contains(&at) {
            self.noise_peaks.push(peak);
            self.noise_rms.push(rms);
        } else if (3.0..10.0).contains(&at) {
            self.speech_rms.push(rms);
            self.peaks.push(peak);
        } else if (10.0..13.0).contains(&at) {
            self.soft_peaks.push(peak);
            self.peaks.push(peak);
        }
    }
}

fn render(
    input: &[Hop],
    config: Config,
    cancel: &AtomicBool,
) -> Result<(Vec<f32>, Metrics), String> {
    let (_tx, reference) = crate::audio_queue::channel();
    let mut processor = dsp::Processor::new(reference);
    let mut output = Vec::with_capacity(input.len() * FRAME_SAMPLES);
    let mut metrics = Metrics::new();
    for frame in input {
        if cancel.load(Ordering::Relaxed) {
            return Err("Microphone adjustment cancelled.".into());
        }
        let processed = processor.process(frame.samples, config, 0, false)?;
        metrics.push(
            frame.at,
            &processed.samples,
            processed.gate_peak as f32 / 1000.0,
        );
        if processed.action == GateAction::Drop {
            output.extend_from_slice(&[0.0; FRAME_SAMPLES]);
        } else {
            output.extend_from_slice(&processed.samples);
        }
    }
    Ok((output, metrics))
}

fn recommendation(
    metrics: &mut Metrics,
    base: Config,
    profile: Profile,
    allow_background: bool,
) -> Option<(Config, bool, f32)> {
    let speech = percentile(&mut metrics.speech_rms, 0.6);
    let noise_rms = percentile(&mut metrics.noise_rms, 0.95);
    let noise_ratio = noise_rms / speech.max(0.001);
    if speech < 0.001 || noise_ratio > if allow_background { 0.5 } else { 0.06 } {
        return None;
    }
    let mut config = base;
    config.processing.profile = profile;
    if config.processing.gate {
        let noise_peak = percentile(&mut metrics.noise_peaks, 0.95);
        let soft_peak = percentile(&mut metrics.soft_peaks, 0.35);
        // Closing uses half the opening threshold, so room noise needs clearance below both.
        let lower = (noise_peak * 1000.0).round() as u32 * 2 + 2;
        let upper = (soft_peak * 1000.0 / 1.4).floor() as u32;
        if lower > upper || upper == 0 {
            if !allow_background {
                return None;
            }
            // An unsafe gate clips quiet words; an open gate leaves audible noise for preview instead.
            config.processing.gate = false;
            config.processing.profile = Profile::Custom;
        } else {
            config.threshold = lower.clamp(1, 1000);
        }
    }
    let peak = percentile(&mut metrics.peaks, 0.99).max(0.001);
    let gain = (0.1 / speech).min(0.70 / peak);
    config.volume = (gain.clamp(0.01, 2.0) * 100.0).round() as u16;
    Some((config, gain > 2.0, noise_ratio))
}

pub(super) fn analyze(
    input: Vec<Hop>,
    current: Config,
    cancel: Arc<AtomicBool>,
) -> Result<Analysis, String> {
    for (from, until, minimum) in [
        (0.5, 3.0, 210),
        (3.0, 10.0, 600),
        (10.0, 13.0, 250),
        (13.0, 15.0, 165),
    ] {
        if input
            .iter()
            .filter(|f| (from..until).contains(&f.at))
            .count()
            < minimum
        {
            return Err("The recording had gaps. Close other audio tools and try again.".into());
        }
    }
    if input.len() > 1600
        || input
            .iter()
            .any(|h| h.samples.iter().any(|s| !s.is_finite()))
    {
        return Err("The microphone returned invalid audio. Try another input device.".into());
    }
    let clipped = input
        .iter()
        .flat_map(|f| f.samples)
        .filter(|s| s.abs() >= 0.995)
        .count();
    if clipped > input.len() * FRAME_SAMPLES / 1000 {
        return Err("The microphone is clipping. Lower its input level or move it farther away, then repeat.".into());
    }
    let mut raw = Metrics::new();
    for hop in &input {
        raw.push(
            hop.at,
            &hop.samples,
            hop.samples.iter().fold(0.0_f32, |m, s| m.max(s.abs())),
        );
    }
    let raw_noise = percentile(&mut raw.noise_rms, 0.95);
    let voiced_normal: Vec<_> = raw
        .speech_rms
        .iter()
        .map(|rms| *rms >= (raw_noise * 2.0).max(0.001))
        .collect();
    if voiced_normal.iter().filter(|voiced| **voiced).count() < 100 {
        return Err(
            "Not enough clear speech was detected. Move the microphone closer and repeat.".into(),
        );
    }
    let soft_floor = (percentile(&mut raw.noise_peaks, 0.95) * 1.5).max(0.001);
    let mut voiced_soft: Vec<_> = raw
        .soft_peaks
        .iter()
        .copied()
        .filter(|peak| *peak > soft_floor)
        .collect();
    let soft_speech_unmeasured = voiced_soft.len() < 40;
    let raw_soft = percentile(&mut voiced_soft, 0.35);
    let (before, _) = render(&input, current, &cancel)?;
    let mut fallback: Option<(Config, bool, f32)> = None;
    let mut preserved_soft_speech = false;
    for (profile, suppression) in [
        (Profile::Natural, 0),
        (Profile::Balanced, 12),
        (Profile::Balanced, 18),
        (Profile::Noisy, 24),
        (Profile::Noisy, 30),
    ] {
        let mut candidate = current.preset(profile);
        candidate.volume = 100;
        candidate.suppression = suppression.max(6);
        let mut measuring = candidate;
        measuring.processing.gate = false;
        let (_, mut metrics) = render(&input, measuring, &cancel)?;
        // Pauses must not set the voice gain, but noise and clipping checks still use the full capture.
        metrics.speech_rms = metrics
            .speech_rms
            .iter()
            .zip(&voiced_normal)
            .filter_map(|(rms, voiced)| voiced.then_some(*rms))
            .collect();
        metrics.soft_peaks = metrics
            .soft_peaks
            .iter()
            .zip(&raw.soft_peaks)
            .filter_map(|(processed, original)| (*original > soft_floor).then_some(*processed))
            .collect();
        if soft_speech_unmeasured {
            // Without measurable soft speech, only an unfiltered, open-gate preview can preserve it.
            if profile == Profile::Natural
                && let Some((config, limited_gain, noise_ratio)) =
                    recommendation(&mut metrics, candidate, profile, true)
            {
                let (recommended, _) = render(&input, config, &cancel)?;
                return Ok(Analysis {
                    config,
                    current: before,
                    recommended,
                    limited_gain,
                    background_noise: noise_ratio > 0.06,
                    soft_speech_unmeasured: true,
                });
            }
            return Err("Not enough soft speech was detected. Speak softly without whispering during seconds 10 to 13, then repeat.".into());
        }
        if percentile(&mut metrics.soft_peaks, 0.35) < raw_soft * 0.45 {
            tracing::info!(
                ?profile,
                suppression,
                reason = "quiet speech attenuated",
                "microphone calibration candidate rejected"
            );
            continue;
        }
        preserved_soft_speech = true;
        if let Some((config, limited_gain, _)) =
            recommendation(&mut metrics, candidate, profile, false)
        {
            let (recommended, _) = render(&input, config, &cancel)?;
            return Ok(Analysis {
                config,
                current: before,
                recommended,
                limited_gain,
                background_noise: false,
                soft_speech_unmeasured: false,
            });
        }
        if let Some(option) = recommendation(&mut metrics, candidate, profile, true) {
            // Keep lighter processing unless another candidate provides a meaningful noise improvement.
            if fallback
                .as_ref()
                .is_none_or(|previous| option.2 < previous.2 * 0.8)
            {
                fallback = Some(option);
            }
        }
    }
    if let Some((config, limited_gain, _)) = fallback {
        tracing::info!(profile = ?config.processing.profile, volume = config.volume, gate = config.processing.gate, "microphone calibration offers a preview with residual background noise");
        let (recommended, _) = render(&input, config, &cancel)?;
        return Ok(Analysis {
            config,
            current: before,
            recommended,
            limited_gain,
            background_noise: true,
            soft_speech_unmeasured: false,
        });
    }
    if !preserved_soft_speech {
        return Err("Processing reduced quiet speech too much. Check your microphone's own noise reduction and repeat.".into());
    }
    Err("Noise and quiet speech overlap too much. Reposition the microphone and repeat.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a local 15 second guided microphone WAV (3 quiet, 7 normal, 3 soft, 2 quiet) via DISCORDIA_MIC_AUDIT_WAV"]
    fn recorded_speech_produces_a_bounded_stable_recommendation() {
        let path = std::env::var("DISCORDIA_MIC_AUDIT_WAV").unwrap();
        let samples = super::super::speech_audit_tests::read_speech(std::path::Path::new(&path));
        assert!(samples.len() >= 15 * SAMPLE_RATE as usize);
        let original: Vec<_> = samples.as_chunks::<FRAME_SAMPLES>().0.iter().collect();
        let input: Vec<_> = (0..1500)
            .map(|i| Hop {
                at: i as f32 / 100.0,
                samples: *original[i],
            })
            .collect();
        let mut current = Config::from_state(&AppState::empty());
        current.processing.headphones = true;
        current.processing.echo = false;
        current.processing.high_pass = false;
        let result = analyze(input, current, Arc::new(AtomicBool::new(false))).unwrap();
        println!(
            "Recorded speech: profile={:?}, volume={}%, threshold={}, suppression={}dB, fixed_gain={}",
            result.config.processing.profile,
            result.config.volume,
            result.config.threshold,
            result.config.suppression,
            !result.config.agc
        );
        assert!(!result.config.agc);
        assert!(result.config.volume <= 200);
        assert!(
            result
                .recommended
                .iter()
                .all(|sample| sample.abs() <= 0.981)
        );
        let quiet = &result.recommended[10 * SAMPLE_RATE as usize..13 * SAMPLE_RATE as usize];
        assert!(
            quiet.iter().any(|sample| sample.abs() > 0.005),
            "quiet speech was removed"
        );
    }

    fn recording(noise: f32, voice: f32, soft: f32) -> Vec<Hop> {
        (0..1500)
            .map(|i| {
                let at = i as f32 / 100.0;
                let amp = if (3.0..10.0).contains(&at) {
                    voice
                } else if (10.0..13.0).contains(&at) {
                    soft
                } else {
                    noise
                };
                Hop {
                    at,
                    samples: std::array::from_fn(|i| if i % 2 == 0 { amp } else { -amp }),
                }
            })
            .collect()
    }

    #[test]
    fn quiet_room_prefers_stable_natural_audio_and_keeps_drafts_separate() {
        let mut original = Config::from_state(&AppState::empty());
        original.processing.headphones = true;
        original.processing.echo = false;
        original.processing.high_pass = false;
        original.denoise = false;
        let result = analyze(
            recording(0.0001, 0.07, 0.02),
            original,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(result.config.processing.profile, Profile::Natural);
        assert!(!result.config.agc);
        assert!((140..=144).contains(&result.config.volume));
        assert_eq!(original.volume, 100);
        let peaks: Vec<_> = result
            .recommended
            .chunks(FRAME_SAMPLES)
            .skip(400)
            .take(500)
            .map(|h| h[0].abs())
            .collect();
        assert!(peaks.iter().all(|peak| (*peak - peaks[0]).abs() < 0.00001));
    }

    #[test]
    fn invalid_clipped_missing_and_cancelled_captures_cannot_be_applied() {
        let original = Config::from_state(&AppState::empty());
        for input in [
            vec![],
            recording(0.0, 0.0, 0.0),
            recording(0.0, 1.0, 0.5),
            recording(0.02, 0.02, 0.02),
        ] {
            assert!(analyze(input, original, Arc::new(AtomicBool::new(false))).is_err());
        }
        assert!(
            analyze(
                recording(0.0001, 0.07, 0.02),
                original,
                Arc::new(AtomicBool::new(true))
            )
            .is_err()
        );
    }

    #[test]
    fn natural_pauses_do_not_hide_audible_normal_and_soft_speech() {
        let mut current = Config::from_state(&AppState::empty());
        current.processing.headphones = true;
        current.processing.echo = false;
        current.processing.high_pass = false;
        let mut input = recording(0.0001, 0.07, 0.02);
        for hop in &mut input {
            if (3.0..13.0).contains(&hop.at) && (hop.at * 100.0).round() as usize % 100 >= 30 {
                hop.samples.fill(0.0001);
            }
        }
        let result = analyze(input, current, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(result.config.processing.profile, Profile::Natural);
        assert!((140..=144).contains(&result.config.volume));
        assert!(!result.config.agc);
        assert!(result.recommended.iter().any(|sample| sample.abs() > 0.02));
    }

    #[test]
    fn unmeasurable_soft_speech_still_allows_a_conservative_volume_preview() {
        let mut current = Config::from_state(&AppState::empty());
        current.processing.headphones = true;
        let result = analyze(
            recording(0.01, 0.07, 0.01),
            current,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(result.soft_speech_unmeasured);
        assert!(result.background_noise);
        assert_eq!(result.config.processing.profile, Profile::Natural);
        assert!(!result.config.processing.gate);
        assert!(!result.config.denoise);
        assert!(!result.config.agc);
        assert_eq!(current.volume, 100);
        assert!(
            result.recommended[10 * SAMPLE_RATE as usize..13 * SAMPLE_RATE as usize]
                .iter()
                .all(|sample| sample.abs() >= 0.01)
        );
    }

    #[test]
    fn a_short_click_cannot_replace_normal_speech() {
        let mut input = recording(0.0001, 0.0001, 0.02);
        input[350].samples.fill(0.1);
        assert!(
            analyze(
                input,
                Config::from_state(&AppState::empty()),
                Arc::new(AtomicBool::new(false)),
            )
            .is_err()
        );
    }

    #[test]
    fn gate_threshold_clears_the_closing_hysteresis_and_quantization() {
        let config = Config::from_state(&AppState::empty()).preset(Profile::Balanced);
        let mut metrics = Metrics::new();
        metrics.noise_rms = vec![0.0005; 250];
        metrics.noise_peaks = vec![0.001; 250];
        metrics.speech_rms = vec![0.1; 700];
        metrics.soft_peaks = vec![0.02; 300];
        metrics.peaks = vec![0.15; 1000];
        let (recommended, _, _) =
            recommendation(&mut metrics, config, Profile::Balanced, false).unwrap();
        assert!(recommended.threshold / 2 > 1);
        metrics.soft_peaks.fill(0.001);
        assert!(recommendation(&mut metrics, config, Profile::Balanced, false).is_none());
    }

    #[test]
    fn strong_unclipped_speech_can_use_the_full_volume_control_range() {
        let mut current = Config::from_state(&AppState::empty());
        current.processing.headphones = true;
        let result = analyze(
            recording(0.0001, 0.4, 0.12),
            current,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(result.config.volume, 25);
        assert!(!result.background_noise);
        assert!(
            result
                .recommended
                .iter()
                .all(|sample| sample.abs() <= 0.101)
        );
    }

    #[test]
    fn usable_speech_with_background_noise_gets_an_honest_preview() {
        let mut current = Config::from_state(&AppState::empty());
        current.processing.headphones = true;
        let result = analyze(
            recording(0.01, 0.07, 0.03),
            current,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(result.background_noise);
        assert!(!result.config.agc);
        assert_eq!(current.volume, 100);
        assert!(
            result.recommended[10 * SAMPLE_RATE as usize..13 * SAMPLE_RATE as usize]
                .iter()
                .any(|s| s.abs() >= 0.02)
        );
    }

    #[test]
    fn fallback_preserves_soft_words_when_a_safe_gate_cannot_be_found() {
        let config = Config::from_state(&AppState::empty()).preset(Profile::Balanced);
        let mut metrics = Metrics::new();
        metrics.noise_rms = vec![0.001; 250];
        metrics.noise_peaks = vec![0.009; 250];
        metrics.speech_rms = vec![0.1; 700];
        metrics.soft_peaks = vec![0.02; 300];
        metrics.peaks = vec![0.15; 1000];
        assert!(recommendation(&mut metrics, config, Profile::Balanced, false).is_none());
        let (recommended, _, _) =
            recommendation(&mut metrics, config, Profile::Balanced, true).unwrap();
        assert!(!recommended.processing.gate);
        assert_eq!(recommended.processing.profile, Profile::Custom);
        assert!(recommended.denoise);
    }
}
