use std::time::{Duration, Instant};

use nnnoiseless::DenoiseState;

pub const HOP: usize = 480;
const ATTEN_LIM_DB: f32 = 30.0;
// Echo, AGC and encoding share the rest of each 10 ms capture deadline.
const HOP_BUDGET: Duration = Duration::from_micros(6_000);
const OVERRUN_TRIP: u32 = 100;
const PCM_SCALE: f32 = 32_768.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    RNNoise,
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Self::RNNoise => "RNNoise",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    Off,
}

pub struct Denoiser {
    model: Option<Box<RnNoise>>,
    original_mix: f32,
    budget: Duration,
    overruns: u32,
}

impl Denoiser {
    pub fn new() -> Self {
        Self {
            model: Some(Box::new(RnNoise::new())),
            original_mix: 10f32.powf(-ATTEN_LIM_DB / 20.0),
            budget: HOP_BUDGET,
            overruns: 0,
        }
    }

    #[cfg(test)]
    pub fn stopped() -> Self {
        Self {
            model: None,
            ..Self::new()
        }
    }

    pub fn engine(&self) -> Option<Engine> {
        self.model.as_ref().map(|_| Engine::RNNoise)
    }

    pub fn fallback(&self) -> Option<Fallback> {
        self.model.is_none().then_some(Fallback::Off)
    }

    pub fn set_atten_lim(&mut self, db: f32) {
        let db = if db.is_finite() {
            db.clamp(0.0, 100.0)
        } else {
            ATTEN_LIM_DB
        };
        self.original_mix = 10f32.powf(-db / 20.0);
    }

    pub fn process_hop(&mut self, frame: &mut [f32]) {
        let Ok(frame) = <&mut [f32; HOP]>::try_from(frame) else {
            return;
        };
        let Some(model) = self.model.as_mut() else {
            return;
        };
        let started = Instant::now();
        model.process(frame, self.original_mix);
        self.overruns = if started.elapsed() > self.budget {
            self.overruns + 1
        } else {
            self.overruns.saturating_sub(1)
        };
        if self.overruns >= OVERRUN_TRIP {
            self.model = None;
            tracing::warn!("RNNoise paused after sustained processing overload");
        }
    }
}

struct RnNoise {
    state: Box<DenoiseState<'static>>,
    input: [f32; HOP],
    output: [f32; HOP],
    /// RNNoise's overlap-add returns the previous hop, so the limit mixes that one back.
    previous: [f32; HOP],
}

impl RnNoise {
    fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            input: [0.0; HOP],
            output: [0.0; HOP],
            previous: [0.0; HOP],
        }
    }

    fn process(&mut self, frame: &mut [f32; HOP], keep: f32) {
        for (scaled, sample) in self.input.iter_mut().zip(frame.iter()) {
            *scaled = sample * PCM_SCALE;
        }
        self.state.process_frame(&mut self.output, &self.input);
        for ((sample, enhanced), noisy) in frame.iter_mut().zip(self.output).zip(&mut self.previous)
        {
            let enhanced = enhanced / PCM_SCALE;
            let current = *sample;
            *sample = enhanced + (*noisy - enhanced) * keep;
            *noisy = current;
        }
    }
}

// VAD must keep its history when noise suppression is toggled or paused.
pub struct Vad {
    state: Box<DenoiseState<'static>>,
    input: [f32; HOP],
    output: [f32; HOP],
}

impl Vad {
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            input: [0.0; HOP],
            output: [0.0; HOP],
        }
    }

    pub fn speech(&mut self, frame: &[f32; HOP]) -> f32 {
        for (scaled, sample) in self.input.iter_mut().zip(frame) {
            *scaled = sample * PCM_SCALE;
        }
        self.state.process_frame(&mut self.output, &self.input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, amplitude: f32) -> Vec<f32> {
        let mut rng = 0x9E37_79B9u32;
        (0..n)
            .map(|_| {
                rng ^= rng << 13;
                rng ^= rng >> 17;
                rng ^= rng << 5;
                ((rng >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 2.0 * amplitude
            })
            .collect()
    }

    /// Fan-shaped: RNNoise passes loud white noise as if it were fricatives.
    fn fan(n: usize, amplitude: f32) -> Vec<f32> {
        let mut level = 0.0;
        noise(n, amplitude * 0.2)
            .into_iter()
            .map(|sample| {
                level = 0.98 * level + sample;
                level
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn run(denoiser: &mut Denoiser, input: &[f32]) -> Vec<f32> {
        let mut output = input.to_vec();
        for hop in output.as_chunks_mut::<HOP>().0 {
            denoiser.process_hop(hop);
        }
        output
    }

    fn engines() -> [Denoiser; 1] {
        [Denoiser::new()]
    }

    #[test]
    fn model_loads_with_the_pipeline_geometry() {
        assert_eq!(HOP, 480);
        assert_eq!(Denoiser::new().engine(), Some(Engine::RNNoise));
        assert_eq!(Denoiser::new().fallback(), None);
    }

    #[test]
    fn suppresses_speechless_noise() {
        for mut denoiser in engines() {
            let input = fan(HOP * 50, 0.2);
            let output = run(&mut denoiser, &input);
            let (out, inp) = (rms(&output[HOP * 49..]), rms(&input[HOP * 49..]));
            assert!(
                out < inp * 0.5,
                "{:?} should attenuate noise: in={inp:.4} out={out:.4}",
                denoiser.engine()
            );
        }
    }

    #[test]
    fn suppression_strength_changes_residual_noise() {
        let input = fan(HOP * 300, 0.1);
        let levels: Vec<_> = [9.0, 30.0]
            .into_iter()
            .map(|strength| {
                let mut denoiser = Denoiser::new();
                denoiser.set_atten_lim(strength);
                rms(&run(&mut denoiser, &input)[HOP * 100..])
            })
            .collect();
        eprintln!("RNNoise noise RMS at 9/30 dB suppression: {levels:?}");
        assert!(
            levels[1] < levels[0] * 0.7,
            "RNNoise: stronger suppression must reduce noise: {levels:?}"
        );
    }

    #[test]
    fn rnnoise_limit_mixes_back_the_hop_it_returns() {
        let input = noise(HOP * 20, 0.0000001);
        let mut denoiser = Denoiser::new();
        denoiser.set_atten_lim(0.0);
        let output = run(&mut denoiser, &input);
        for (lag, expected) in [(HOP, true), (0, false)] {
            let aligned = output[HOP * 2..]
                .iter()
                .zip(&input[HOP * 2 - lag..])
                .all(|(out, inp)| (out - inp).abs() < 1e-9);
            assert_eq!(aligned, expected, "output lagging input by {lag} samples");
        }
    }

    #[test]
    fn strength_changes_preserve_finite_output_and_the_same_filter_history() {
        let input = noise(HOP * 20, 0.1);
        let mut denoiser = Denoiser::new();
        for strength in [6.0, 15.0, 30.0, 60.0, f32::NAN, f32::INFINITY, -10.0] {
            denoiser.set_atten_lim(strength);
            assert!(denoiser.original_mix.is_finite());
            let output = run(&mut denoiser, &input);
            assert!(output.iter().all(|s| s.is_finite()));
            assert_eq!(denoiser.engine(), Some(Engine::RNNoise));
            assert_eq!(denoiser.fallback(), None);
        }
    }

    #[test]
    fn sustained_overload_pauses_rnnoise_until_explicit_retry() {
        let mut denoiser = Denoiser::new();
        denoiser.budget = Duration::ZERO;
        let hop = noise(HOP, 0.1);
        let mut steps = vec![denoiser.engine()];
        for _ in 0..2 * OVERRUN_TRIP {
            denoiser.process_hop(&mut hop.clone());
            if steps.last() != Some(&denoiser.engine()) {
                steps.push(denoiser.engine());
            }
        }
        assert_eq!(steps, [Some(Engine::RNNoise), None]);
        assert_eq!(denoiser.fallback(), Some(Fallback::Off));
        let mut untouched = hop.clone();
        denoiser.process_hop(&mut untouched);
        assert_eq!(untouched, hop);
    }

    #[test]
    fn a_spike_now_and_then_does_not_trip_the_guard() {
        let mut denoiser = Denoiser::new();
        let hop = noise(HOP, 0.1);
        for i in 0..10 * OVERRUN_TRIP {
            denoiser.budget = if i % 3 == 0 {
                Duration::ZERO
            } else {
                HOP_BUDGET
            };
            denoiser.process_hop(&mut hop.clone());
        }
        assert_eq!(denoiser.engine(), Some(Engine::RNNoise));
    }

    #[test]
    fn vad_tells_speech_from_noise_at_the_same_level() {
        let mean = |input: &[f32]| {
            let mut vad = Vad::new();
            let probs: Vec<f32> = input
                .as_chunks::<HOP>()
                .0
                .iter()
                .map(|hop| vad.speech(hop))
                .collect();
            probs.iter().sum::<f32>() / probs.len() as f32
        };
        let speech = crate::agc::tests::speech(10.0, -30.0, 3);
        let level = rms(&speech);
        let noise = noise(speech.len(), level * 3f32.sqrt());
        let (speech, noise) = (mean(&speech), mean(&noise));
        eprintln!("mean speech probability: speech={speech:.2} noise={noise:.2}");
        assert!(speech > noise + 0.2, "speech={speech:.2} noise={noise:.2}");
    }

    #[test]
    fn hop_cost_fits_the_budget() {
        let input = noise(HOP * 200, 0.1);
        for mut denoiser in engines() {
            let started = Instant::now();
            run(&mut denoiser, &input);
            let per_hop = started.elapsed() / 200;
            eprintln!("{:?}: {per_hop:?} per hop", denoiser.engine());
            assert!(per_hop < HOP_BUDGET);
        }
        let mut vad = Vad::new();
        let started = Instant::now();
        for hop in input.as_chunks::<HOP>().0 {
            vad.speech(hop);
        }
        eprintln!("VAD: {:?} per hop", started.elapsed() / 200);
    }

    #[test]
    fn wrong_length_frames_pass_through() {
        let mut d = Denoiser::new();
        let mut short = vec![0.5f32; HOP - 1];
        d.process_hop(&mut short);
        assert!(short.iter().all(|s| *s == 0.5));
    }
}
