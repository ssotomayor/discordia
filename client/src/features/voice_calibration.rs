use super::*;
use crate::mic_profile::{Config, Context, Profile};

pub(super) const NORMAL_START: f32 = 1.5;
pub(super) const SOFT_START: f32 = 7.5;
pub(super) const QUIET_START: f32 = 13.5;
pub(super) const SECONDS: u64 = 15;
pub(super) const MAX_FRAMES: usize = SECONDS as usize * 100 + 100;

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
    pub keep_current: bool,
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
        if (0.5..NORMAL_START).contains(&at) || (QUIET_START..SECONDS as f32).contains(&at) {
            self.noise_peaks.push(peak);
            self.noise_rms.push(rms);
        } else if (NORMAL_START..SOFT_START).contains(&at) {
            self.speech_rms.push(rms);
            self.peaks.push(peak);
        } else if (SOFT_START..QUIET_START).contains(&at) {
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
    let (_tx, reference) = crate::audio_queue::reference_channel();
    let mut processor = dsp::Processor::new(reference);
    let mut output = Vec::with_capacity(input.len() * FRAME_SAMPLES);
    let mut lead_in = activity::LeadIn::new();
    let mut transmission = Vec::with_capacity(4);
    let mut gate_peaks = Vec::with_capacity(input.len());
    let mut metrics = Metrics::new();
    for frame in input {
        if cancel.load(Ordering::Relaxed) {
            return Err("Microphone adjustment cancelled.".into());
        }
        let processed = processor.process(frame.samples, config, false);
        gate_peaks.push(processed.gate_peak as f32 / 1000.0);
        lead_in.push(
            crate::audio_queue::Frame::new(processed.samples),
            processed.action,
            config,
            false,
            &mut transmission,
        );
        output.extend_from_slice(&[0.0; FRAME_SAMPLES]);
        let start = output.len() - transmission.len() * FRAME_SAMPLES;
        for (slot, outgoing) in output[start..]
            .chunks_mut(FRAME_SAMPLES)
            .zip(transmission.drain(..))
        {
            slot.copy_from_slice(&outgoing.samples);
        }
    }
    for ((frame, samples), peak) in input
        .iter()
        .zip(output.chunks(FRAME_SAMPLES))
        .zip(gate_peaks)
    {
        metrics.push(frame.at, samples, peak);
    }
    Ok((output, metrics))
}

fn checked_preview(
    input: &[Hop],
    config: Config,
    noise_budget: f32,
    cancel: &AtomicBool,
) -> Result<Option<Vec<f32>>, String> {
    let (samples, mut metrics) = render(input, config, cancel)?;
    let noise = percentile(&mut metrics.noise_rms, 0.95);
    if noise > noise_budget {
        tracing::info!(
            ?config,
            noise,
            noise_budget,
            "microphone candidate increases audible background noise"
        );
        return Ok(None);
    }
    if config.processing.gate && config.processing.voice_detection {
        let mut normal = 0;
        let mut soft = 0;
        for (frame, output) in input.iter().zip(samples.chunks(FRAME_SAMPLES)) {
            let peak = output.iter().fold(0.0_f32, |peak, s| peak.max(s.abs()));
            if (NORMAL_START..SOFT_START).contains(&frame.at) && peak > 0.001 {
                normal += 1;
            }
            if (SOFT_START..QUIET_START).contains(&frame.at) && peak > 0.0003 {
                soft += 1;
            }
        }
        if normal < 100 || soft < 40 {
            return Ok(None);
        }
    }
    Ok(Some(samples))
}

fn keep_current(config: Config, samples: Vec<f32>) -> Analysis {
    Analysis {
        config,
        current: samples,
        recommended: Vec::new(),
        limited_gain: false,
        background_noise: false,
        keep_current: true,
    }
}

fn recommendation(
    metrics: &mut Metrics,
    base: Config,
    profile: Profile,
    allow_background: bool,
    noise_budget: f32,
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
            return None;
        }
        config.threshold = lower.clamp(1, 1000);
    }
    let peak = percentile(&mut metrics.peaks, 0.99).max(0.001);
    let wanted_gain = (0.1 / speech).min(0.70 / peak);
    let gain = wanted_gain.min(noise_budget / noise_rms.max(1e-9));
    if speech * gain < speech.min(0.02) {
        return None;
    }
    config.volume = (gain.clamp(0.01, 2.0) * 100.0).round() as u16;
    Some((config, wanted_gain > 2.0 || gain < wanted_gain, noise_ratio))
}

fn vad_supports(level: f32, ambient: f32, probability: f32, confidence: f32) -> bool {
    probability >= confidence && level >= (ambient * 1.2).max(0.0003)
}

pub(super) fn analyze(
    input: Vec<Hop>,
    current: Config,
    cancel: Arc<AtomicBool>,
) -> Result<Analysis, String> {
    for (from, until, minimum) in [
        (0.5, NORMAL_START, 85),
        (NORMAL_START, SOFT_START, 510),
        (SOFT_START, QUIET_START, 510),
        (QUIET_START, SECONDS as f32, 125),
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
    if input.len() > MAX_FRAMES
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
    let mut vad = crate::denoise::Vad::new();
    let mut normal_probability = Vec::new();
    let mut soft_probability = Vec::new();
    for hop in &input {
        if cancel.load(Ordering::Relaxed) {
            return Err("Microphone adjustment cancelled.".into());
        }
        let probability = vad.speech(&hop.samples);
        if (NORMAL_START..SOFT_START).contains(&hop.at) {
            normal_probability.push(probability);
        }
        if (SOFT_START..QUIET_START).contains(&hop.at) {
            soft_probability.push(probability);
        }
        raw.push(
            hop.at,
            &hop.samples,
            hop.samples.iter().fold(0.0_f32, |m, s| m.max(s.abs())),
        );
    }
    let raw_noise = percentile(&mut raw.noise_rms, 0.95);
    // Transient room sounds must not raise the speech-detection floor for the entire reading.
    let detection_noise = percentile(&mut raw.noise_rms, 0.5);
    let speech_floor = (detection_noise * 2.0).max(0.001);
    let voiced_normal: Vec<_> = raw
        .speech_rms
        .iter()
        .zip(&normal_probability)
        .map(|(rms, probability)| {
            *rms >= speech_floor || vad_supports(*rms, detection_noise, *probability, 0.6)
        })
        .collect();
    let voiced_frames = voiced_normal.iter().filter(|voiced| **voiced).count();
    let vad_frames = normal_probability
        .iter()
        .filter(|probability| **probability >= 0.6)
        .count();
    tracing::info!(
        voiced_frames,
        vad_frames,
        normal_frames = voiced_normal.len(),
        raw_noise,
        detection_noise,
        speech_floor,
        "microphone calibration speech measurement"
    );
    if voiced_frames < 100 {
        return Err(
            "The adjustment could not separate your voice from background noise. Test the microphone to check the input; your settings have not changed.".into(),
        );
    }
    let soft_floor = (percentile(&mut raw.noise_peaks, 0.5) * 1.5).max(0.001);
    let soft_noise = percentile(&mut raw.noise_peaks, 0.5);
    let soft_mask: Vec<_> = raw
        .soft_peaks
        .iter()
        .zip(&soft_probability)
        .map(|(peak, probability)| {
            *peak > soft_floor || vad_supports(*peak, soft_noise, *probability, 0.35)
        })
        .collect();
    let mut voiced_soft: Vec<_> = raw
        .soft_peaks
        .iter()
        .zip(&soft_mask)
        .filter_map(|(peak, voiced)| voiced.then_some(*peak))
        .collect();
    let soft_speech_unmeasured = voiced_soft.len() < 40;
    let raw_soft = percentile(&mut voiced_soft, 0.35);
    let (before, mut before_metrics) = render(&input, current, &cancel)?;
    // A louder voice must not disguise a louder room; compare actual output after gate and gain.
    let noise_budget = percentile(&mut before_metrics.noise_rms, 0.95).clamp(0.0003, 0.006);
    if soft_speech_unmeasured {
        return Ok(keep_current(current, before));
    }
    let mut fallback: Option<(Config, bool, f32, Vec<f32>)> = None;
    for (profile, suppression) in [
        (Profile::Natural, 0),
        (Profile::Balanced, 12),
        (Profile::Balanced, 18),
        (Profile::Noisy, 24),
        (Profile::Noisy, 30),
        (Profile::Custom, current.suppression),
    ] {
        let mut candidate = current.preset(profile);
        candidate.agc = false;
        candidate.volume = 100;
        candidate.suppression = suppression.max(6);
        // Noisy rooms need suppression and a safe speech gate, even when a raw profile sounds louder.
        if raw_noise > 0.0003 && (!candidate.denoise || !candidate.processing.gate) {
            continue;
        }
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
            .zip(&soft_mask)
            .filter_map(|(processed, voiced)| voiced.then_some(*processed))
            .collect();
        if percentile(&mut metrics.soft_peaks, 0.35) < raw_soft * 0.45 {
            tracing::info!(
                ?profile,
                suppression,
                reason = "quiet speech attenuated",
                "microphone calibration candidate rejected"
            );
            continue;
        }
        if let Some((config, limited_gain, _)) =
            recommendation(&mut metrics, candidate, profile, false, noise_budget)
        {
            let Some(recommended) = checked_preview(&input, config, noise_budget, &cancel)? else {
                continue;
            };
            return Ok(Analysis {
                config,
                current: before,
                recommended,
                limited_gain,
                background_noise: false,
                keep_current: false,
            });
        }
        if let Some(option) = recommendation(&mut metrics, candidate, profile, true, noise_budget) {
            // Keep lighter processing unless another candidate provides a meaningful noise improvement.
            if fallback
                .as_ref()
                .is_none_or(|previous| option.2 < previous.2 * 0.8)
                && let Some(samples) = checked_preview(&input, option.0, noise_budget, &cancel)?
            {
                fallback = Some((option.0, option.1, option.2, samples));
            }
        }
    }
    if let Some((config, limited_gain, _, recommended)) = fallback {
        tracing::info!(profile = ?config.processing.profile, volume = config.volume, gate = config.processing.gate, "microphone calibration offers a preview with residual background noise");
        return Ok(Analysis {
            config,
            current: before,
            recommended,
            limited_gain,
            background_noise: true,
            keep_current: false,
        });
    }
    Ok(keep_current(current, before))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_evidence_can_support_quiet_speech_but_not_silence_or_unchanged_background() {
        assert!(vad_supports(0.0006, 0.0001, 0.9, 0.6));
        assert!(!vad_supports(0.0006, 0.0001, 0.1, 0.6));
        assert!(!vad_supports(0.0, 0.0, 0.99, 0.6));
        assert!(!vad_supports(0.01, 0.01, 0.99, 0.6));
    }

    #[test]
    #[ignore = "requires a local 15 second guided microphone WAV (1.5 quiet, 6 normal, 6 soft, 1.5 quiet) via DISCORDIA_MIC_AUDIT_WAV"]
    fn recorded_speech_produces_a_bounded_stable_recommendation() {
        let path = std::env::var("DISCORDIA_MIC_AUDIT_WAV").unwrap();
        let samples = super::super::speech_audit_tests::read_speech(std::path::Path::new(&path));
        assert!(samples.len() >= SECONDS as usize * SAMPLE_RATE as usize);
        let original: Vec<_> = samples.as_chunks::<FRAME_SAMPLES>().0.iter().collect();
        let input: Vec<_> = (0..SECONDS as usize * 100)
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
        if result.keep_current {
            assert_eq!(result.config, current);
            assert!(result.recommended.is_empty());
            println!(
                "Recorded speech: existing settings retained; no safe noise reduction improvement"
            );
            return;
        }
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
        let quiet = &result.recommended[(SOFT_START * SAMPLE_RATE as f32) as usize
            ..(QUIET_START * SAMPLE_RATE as f32) as usize];
        assert!(
            quiet.iter().any(|sample| sample.abs() > 0.005),
            "quiet speech was removed"
        );
    }

    fn recording(noise: f32, voice: f32, soft: f32) -> Vec<Hop> {
        (0..SECONDS as usize * 100)
            .map(|i| {
                let at = i as f32 / 100.0;
                let amp = if (NORMAL_START..SOFT_START).contains(&at) {
                    voice
                } else if (SOFT_START..QUIET_START).contains(&at) {
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
            .skip((NORMAL_START * 100.0) as usize + 100)
            .take(((SOFT_START - NORMAL_START - 1.0) * 100.0) as usize)
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
    fn brief_noise_during_quiet_measurement_does_not_reject_audible_speech() {
        let mut current = Config::from_state(&AppState::empty());
        current.processing.headphones = true;
        let mut input = recording(0.0001, 0.03, 0.01);
        for hop in &mut input {
            if (0.7..0.9).contains(&hop.at) {
                hop.samples = std::array::from_fn(|i| if i % 2 == 0 { 0.08 } else { -0.08 });
            }
        }
        let result = analyze(input, current, Arc::new(AtomicBool::new(false)));
        assert!(
            result.is_ok(),
            "A brief noise burst must not be treated as the room's sustained noise: {}",
            result.err().unwrap_or_default()
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
            if (NORMAL_START..QUIET_START).contains(&hop.at)
                && (hop.at * 100.0).round() as usize % 100 >= 30
            {
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
    fn unmeasurable_soft_speech_keeps_existing_noise_controls() {
        let mut current = Config::from_state(&AppState::empty());
        current.processing.headphones = true;
        let result = analyze(
            recording(0.01, 0.07, 0.01),
            current,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(result.keep_current);
        assert_eq!(result.config, current);
        assert!(result.recommended.is_empty());
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
    fn soft_speech_near_the_end_of_the_extended_reading_is_measured() {
        let mut current = Config::from_state(&AppState::empty());
        current.processing.headphones = true;
        let mut input = recording(0.0001, 0.07, 0.0001);
        for hop in &mut input {
            if (QUIET_START - 2.0..QUIET_START).contains(&hop.at) {
                hop.samples = std::array::from_fn(|i| if i % 2 == 0 { 0.02 } else { -0.02 });
            }
        }
        let result = analyze(input, current, Arc::new(AtomicBool::new(false))).unwrap();
        assert!(!result.keep_current);
        assert!(!result.background_noise);
        assert_eq!(
            result.recommended.len(),
            SECONDS as usize * SAMPLE_RATE as usize
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
        let (recommended, _, _) = recommendation(
            &mut metrics,
            config,
            Profile::Balanced,
            false,
            f32::INFINITY,
        )
        .unwrap();
        assert!(recommended.threshold / 2 > 1);
        metrics.soft_peaks.fill(0.001);
        assert!(
            recommendation(
                &mut metrics,
                config,
                Profile::Balanced,
                false,
                f32::INFINITY
            )
            .is_none()
        );
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
    fn noisy_sample_never_falls_back_to_unfiltered_amplification() {
        let mut current = Config::from_state(&AppState::empty());
        current.processing.headphones = true;
        let result = analyze(
            recording(0.01, 0.07, 0.03),
            current,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert!(result.keep_current);
        assert_eq!(result.config, current);
        assert!(result.recommended.is_empty());
    }

    #[test]
    fn unsafe_gate_is_rejected_instead_of_leaving_background_noise_open() {
        let config = Config::from_state(&AppState::empty()).preset(Profile::Balanced);
        let mut metrics = Metrics::new();
        metrics.noise_rms = vec![0.001; 250];
        metrics.noise_peaks = vec![0.009; 250];
        metrics.speech_rms = vec![0.1; 700];
        metrics.soft_peaks = vec![0.02; 300];
        metrics.peaks = vec![0.15; 1000];
        assert!(
            recommendation(
                &mut metrics,
                config,
                Profile::Balanced,
                false,
                f32::INFINITY
            )
            .is_none()
        );
        assert!(
            recommendation(&mut metrics, config, Profile::Balanced, true, f32::INFINITY).is_none()
        );
    }

    #[test]
    fn gain_target_cannot_double_background_noise_to_reach_200_percent() {
        let config = Config::from_state(&AppState::empty()).preset(Profile::Natural);
        let mut metrics = Metrics::new();
        metrics.noise_rms = vec![0.002; 250];
        metrics.speech_rms = vec![0.04; 600];
        metrics.peaks = vec![0.1; 1200];
        let (config, _, _) =
            recommendation(&mut metrics, config, Profile::Natural, false, 0.002).unwrap();
        assert_eq!(config.volume, 100);
    }

    #[test]
    fn final_preview_rejects_ambient_amplification_and_measures_audible_gate_output() {
        let input = recording(0.01, 0.07, 0.03);
        let cancel = AtomicBool::new(false);
        let mut config = Config::from_state(&AppState::empty());
        config.processing.headphones = true;
        config = config.preset(Profile::Natural);
        config.volume = 200;
        assert!(
            checked_preview(&input, config, 0.01, &cancel)
                .unwrap()
                .is_none()
        );
        config.volume = 100;
        config.processing.gate = true;
        config.threshold = 60;
        assert!(
            checked_preview(&input, config, 0.0003, &cancel)
                .unwrap()
                .is_some()
        );
    }
}
