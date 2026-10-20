use super::*;
use crate::mic_profile::Config;

pub(super) struct Processor {
    denoiser: Option<crate::denoise::Denoiser>,
    suppression: Option<u32>,
    vad: crate::denoise::Vad,
    agc: Option<crate::agc::Agc>,
    agc_error_reported: bool,
    gate: GateState,
    echo: crate::echo::Echo,
    echo_error_reported: bool,
}

pub(super) struct Processed {
    pub samples: [f32; FRAME_SAMPLES],
    pub pre: i32,
    pub gate_peak: i32,
    pub peak: i32,
    pub action: GateAction,
    pub model_loaded: bool,
    pub engine: Option<crate::denoise::Engine>,
    pub speech: f32,
    pub lsnr_db: Option<f32>,
    pub echoed_dbfs: f32,
    pub denoised_dbfs: f32,
    pub agc_gain_db: f32,
}

impl Processor {
    pub fn new(reference: crate::audio_queue::AudioReceiver<i16>) -> Self {
        Self {
            denoiser: None,
            suppression: None,
            vad: crate::denoise::Vad::new(),
            agc: None,
            agc_error_reported: false,
            gate: GateState::default(),
            echo: crate::echo::Echo::new(reference),
            echo_error_reported: false,
        }
    }

    pub fn silence(&mut self) {
        self.gate.silence();
    }

    pub fn denoise_fallback(&self) -> Option<crate::denoise::Fallback> {
        self.denoiser
            .as_ref()
            .and_then(crate::denoise::Denoiser::fallback)
    }

    pub fn retry_denoise(&mut self) {
        self.denoiser = None;
        self.suppression = None;
    }

    pub fn process(
        &mut self,
        mut samples: [f32; FRAME_SAMPLES],
        config: Config,
        delay: i32,
        muted: bool,
    ) -> Processed {
        self.echo
            .configure(config.processing.echo, config.processing.high_pass);
        if let Err(error) = self.echo.process(&mut samples, delay)
            && !self.echo_error_reported
        {
            tracing::warn!(%error, "microphone echo cancellation unavailable");
            self.echo_error_reported = true;
        }
        let pre = peak_fixed(&samples);
        let echoed_dbfs = crate::voice_record::rms_dbfs(&samples);
        if muted {
            self.gate.silence();
            return Processed {
                samples,
                pre,
                gate_peak: pre,
                peak: pre,
                action: GateAction::Drop,
                model_loaded: false,
                engine: None,
                speech: 0.0,
                lsnr_db: None,
                echoed_dbfs,
                denoised_dbfs: echoed_dbfs,
                agc_gain_db: 0.0,
            };
        }
        let speech = self.vad.speech(&samples);
        let mut model_loaded = false;
        let mut lsnr_db = None;
        if config.denoise {
            let denoiser = self.denoiser.get_or_insert_with(|| {
                model_loaded = true;
                crate::denoise::Denoiser::new()
            });
            if self.suppression != Some(config.suppression) {
                denoiser.set_atten_lim(config.suppression as f32);
                self.suppression = Some(config.suppression);
            }
            lsnr_db = denoiser.process_hop(&mut samples).lsnr_db;
        } else {
            self.denoiser = None;
            self.suppression = None;
        }
        let gate_peak = peak_fixed(&samples);
        let denoised_dbfs = crate::voice_record::rms_dbfs(&samples);
        if config.agc {
            let agc = self.agc.get_or_insert_with(crate::agc::Agc::new);
            if let Err(error) = agc.process(&mut samples)
                && !self.agc_error_reported
            {
                tracing::warn!(%error, "microphone gain control unavailable");
                self.agc_error_reported = true;
            }
        } else {
            self.agc = None;
        }
        let gain = config.volume as f32 / 100.0;
        for sample in &mut samples {
            *sample *= gain;
        }
        crate::agc::limit(&mut samples);
        let peak = peak_fixed(&samples);
        let action = if gain == 0.0 {
            self.gate.silence();
            GateAction::Drop
        } else if !config.processing.gate {
            self.gate.silence();
            GateAction::Pass
        } else {
            self.gate.step(gate_peak, config.threshold as i32)
        };
        match action {
            GateAction::RampIn => ramp(&mut samples, true),
            GateAction::RampOut => ramp(&mut samples, false),
            _ => {}
        }
        Processed {
            samples,
            pre,
            gate_peak,
            peak,
            action,
            model_loaded,
            engine: self
                .denoiser
                .as_ref()
                .and_then(crate::denoise::Denoiser::engine),
            speech,
            lsnr_db,
            echoed_dbfs,
            denoised_dbfs,
            agc_gain_db: self.agc.as_ref().map_or(0.0, crate::agc::Agc::gain_db),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn individual_peaks_are_limited_after_manual_gain_without_lowering_normal_speech() {
        let (_, reference) = crate::audio_queue::channel();
        let mut processor = Processor::new(reference);
        let mut config = Config::from_state(&AppState::empty());
        config.processing.headphones = true;
        let mut config = config.preset(crate::mic_profile::Profile::Natural);
        for volume in [50, 100, 200] {
            config.volume = volume;
            let normal = processor.process([0.05; FRAME_SAMPLES], config, 0, false);
            let expected = 0.05 * f32::from(volume) / 100.0;
            assert!(
                normal
                    .samples
                    .iter()
                    .all(|sample| (*sample - expected).abs() < 1e-6)
            );
            let mut peak = [0.1; FRAME_SAMPLES];
            peak[0] = 0.9;
            peak[1] = -0.45;
            let limited = processor.process(peak, config, 0, false);
            assert!(
                limited
                    .samples
                    .iter()
                    .all(|sample| sample.abs() <= 0.980_001)
            );
            assert!((limited.samples[0] / limited.samples[1] + 2.0).abs() < 1e-6);
            let after = processor.process([0.05; FRAME_SAMPLES], config, 0, false);
            assert!(
                after
                    .samples
                    .iter()
                    .all(|sample| (*sample - expected).abs() < 1e-6)
            );
        }
    }

    #[test]
    fn individually_safe_microphones_can_still_overload_the_final_mix() {
        let mut config = Config::from_state(&AppState::empty());
        config.processing.headphones = true;
        let mut config = config.preset(crate::mic_profile::Profile::Natural);
        config.volume = 200;
        let mut processed = Vec::new();
        for _ in 0..2 {
            let (_, reference) = crate::audio_queue::channel();
            let mut processor = Processor::new(reference);
            let output = processor.process([0.9; FRAME_SAMPLES], config, 0, false);
            assert!((output.samples[0] - 0.98).abs() < 1e-6);
            processed.push(output.samples);
        }
        let mut limiter = crate::playback_limiter::Limiter::new(SAMPLE_RATE);
        let mut peak = 0.0_f32;
        for (first, second) in processed[0].iter().zip(&processed[1]) {
            let sum = first + second;
            assert!(sum > 1.95);
            let output = limiter.process(sum);
            assert!(output.abs() <= 1.0);
            peak = peak.max(output.abs());
        }
        assert!(peak > 0.99);
    }

    #[test]
    fn profile_updates_reach_the_dsp_as_one_coherent_configuration() {
        let controls = AudioControls::from_state(&AppState::empty());
        let mut natural = controls.mic_config();
        natural.processing.headphones = true;
        natural = natural.preset(crate::mic_profile::Profile::Natural);
        natural.volume = 75;
        let mut noisy = natural.preset(crate::mic_profile::Profile::Noisy);
        noisy.volume = 160;
        controls.apply_mic(natural);
        let reader = controls.clone();
        let read = std::thread::spawn(move || {
            for _ in 0..10_000 {
                let actual = reader.mic_config();
                assert!(actual == natural || actual == noisy);
            }
        });
        for _ in 0..1000 {
            controls.apply_mic(noisy);
            controls.apply_mic(natural);
        }
        read.join().unwrap();
    }

    #[test]
    fn a_stopped_denoiser_stays_stopped_until_retried() {
        let (_, reference) = crate::audio_queue::channel();
        let mut processor = Processor::new(reference);
        let config = Config::from_state(&AppState::empty());
        assert!(config.denoise);
        processor.denoiser = Some(crate::denoise::Denoiser::stopped());
        let hop = [0.01; FRAME_SAMPLES];
        for _ in 0..3 {
            let processed = processor.process(hop, config, 0, false);
            assert_eq!(processed.engine, None);
            assert_eq!(
                processor.denoise_fallback(),
                Some(crate::denoise::Fallback::Off)
            );
        }
        processor.retry_denoise();
        let processed = processor.process(hop, config, 0, false);
        assert_eq!(
            processed.engine,
            Some(crate::denoise::Engine::DeepFilterNet)
        );
        assert_eq!(processor.denoise_fallback(), None);
    }

    #[test]
    fn natural_preserves_soft_frames_and_a_steady_level_but_obeys_mute_and_zero_gain() {
        let (_, reference) = crate::audio_queue::channel();
        let mut processor = Processor::new(reference);
        let mut config = Config::from_state(&AppState::empty());
        config.processing.headphones = true;
        let mut config = config.preset(crate::mic_profile::Profile::Natural);
        let samples = std::array::from_fn(|i| if i % 2 == 0 { 0.0003 } else { -0.0003 });
        for _ in 0..500 {
            let result = processor.process(samples, config, 0, false);
            assert_eq!(result.samples, samples);
            assert_eq!(result.action, GateAction::Pass);
        }
        assert_eq!(
            processor.process(samples, config, 0, true).action,
            GateAction::Drop
        );
        config.volume = 0;
        assert_eq!(
            processor.process(samples, config, 0, false).action,
            GateAction::Drop
        );
    }
}
