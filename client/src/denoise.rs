use std::time::{Duration, Instant};

use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::Array2;
use nnnoiseless::DenoiseState;

/// Fixed by the model. `features::voice::FRAME_SAMPLES` must agree, and one
/// `Denoiser` must serve one continuous stream — it carries state across hops.
pub const HOP: usize = 480;

const ATTEN_LIM_DB: f32 = 30.0;

/// 60% of a 10 ms hop: echo, AGC and the encoder share the rest of the deadline.
const HOP_BUDGET: Duration = Duration::from_micros(6_000);
/// Net hops over budget, so a sustained overload trips it and stray spikes decay.
const OVERRUN_TRIP: u32 = 100;
const FAILURE_TRIP: u32 = 5;

/// RNNoise works in 16-bit sample units.
const PCM_SCALE: f32 = 32_768.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    DeepFilterNet,
    RNNoise,
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Self::DeepFilterNet => "DeepFilterNet",
            Self::RNNoise => "RNNoise",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    Lighter,
    Off,
}

/// Steps down from DeepFilterNet to RNNoise, then to nothing, when a model
/// cannot keep up or keeps failing — Krisp's CPU and failure thresholds.
pub struct Denoiser {
    model: Option<Model>,
    atten_lim_db: f32,
    budget: Duration,
    overruns: u32,
    failures: u32,
}

enum Model {
    Deep(Box<Deep>),
    Light(Box<Light>),
}

pub struct Hop {
    pub lsnr_db: Option<f32>,
}

impl Denoiser {
    pub fn new() -> Self {
        let model = match Deep::new() {
            Ok(deep) => Model::Deep(Box::new(deep)),
            Err(error) => {
                tracing::warn!(%error, "DeepFilterNet unavailable; denoising with RNNoise");
                Model::Light(Box::new(Light::new()))
            }
        };
        Self {
            model: Some(model),
            atten_lim_db: ATTEN_LIM_DB,
            budget: HOP_BUDGET,
            overruns: 0,
            failures: 0,
        }
    }

    #[cfg(test)]
    fn light() -> Self {
        Self {
            model: Some(Model::Light(Box::new(Light::new()))),
            atten_lim_db: ATTEN_LIM_DB,
            budget: HOP_BUDGET,
            overruns: 0,
            failures: 0,
        }
    }

    #[cfg(test)]
    pub fn stopped() -> Self {
        Self {
            model: None,
            ..Self::light()
        }
    }

    #[cfg(test)]
    pub(crate) fn for_replay(engine: Engine) -> Self {
        let mut denoiser = match engine {
            Engine::DeepFilterNet => Self::new(),
            Engine::RNNoise => Self::light(),
        };
        assert_eq!(denoiser.engine(), Some(engine));
        // Offline runs must not switch models because of debug builds or machine load.
        denoiser.budget = Duration::MAX;
        denoiser
    }

    pub fn engine(&self) -> Option<Engine> {
        match self.model.as_ref()? {
            Model::Deep(_) => Some(Engine::DeepFilterNet),
            Model::Light(_) => Some(Engine::RNNoise),
        }
    }

    pub fn fallback(&self) -> Option<Fallback> {
        match self.model {
            Some(Model::Deep(_)) => None,
            Some(Model::Light(_)) => Some(Fallback::Lighter),
            None => Some(Fallback::Off),
        }
    }

    pub fn set_atten_lim(&mut self, db: f32) {
        self.atten_lim_db = db;
        if let Some(Model::Deep(deep)) = &mut self.model {
            deep.model.set_atten_lim(db);
        }
    }

    pub fn process_hop(&mut self, frame: &mut [f32]) -> Hop {
        let Ok(frame) = <&mut [f32; HOP]>::try_from(frame) else {
            return Hop { lsnr_db: None };
        };
        let started = Instant::now();
        let (lsnr_db, failed) = match &mut self.model {
            None => return Hop { lsnr_db: None },
            Some(Model::Deep(deep)) => match deep.process(frame) {
                Ok(lsnr) => (Some(lsnr), false),
                Err(error) => {
                    tracing::debug!(%error, "denoise hop failed");
                    (None, true)
                }
            },
            Some(Model::Light(light)) => {
                light.process(frame, self.atten_lim_db);
                (None, false)
            }
        };
        self.failures = if failed { self.failures + 1 } else { 0 };
        self.overruns = if started.elapsed() > self.budget {
            self.overruns + 1
        } else {
            self.overruns.saturating_sub(1)
        };
        if self.failures >= FAILURE_TRIP || self.overruns >= OVERRUN_TRIP {
            self.step_down();
        }
        Hop { lsnr_db }
    }

    fn step_down(&mut self) {
        let reason = if self.failures >= FAILURE_TRIP {
            "keeps failing"
        } else {
            "cannot keep up"
        };
        let from = self.engine().map_or("none", Engine::label);
        self.model = match self.model {
            Some(Model::Deep(_)) => Some(Model::Light(Box::new(Light::new()))),
            _ => None,
        };
        self.overruns = 0;
        self.failures = 0;
        tracing::warn!(
            from,
            to = self.engine().map_or("off", Engine::label),
            reason,
            "noise suppression stepped down"
        );
    }
}

struct Deep {
    model: DfTract,
    noisy: Array2<f32>,
    enh: Array2<f32>,
}

impl Deep {
    fn new() -> Result<Self, String> {
        let params = RuntimeParams::default_with_ch(1).with_atten_lim(ATTEN_LIM_DB);
        let model = DfTract::new(DfParams::default(), &params)
            .map_err(|e| format!("load DeepFilterNet model: {e}"))?;
        if model.hop_size != HOP || model.sr != 48_000 {
            return Err(format!(
                "model expects {} Hz / {} sample hops, pipeline produces 48000 Hz / {HOP}",
                model.sr, model.hop_size
            ));
        }
        Ok(Self {
            noisy: Array2::zeros((1, HOP)),
            enh: Array2::zeros((1, HOP)),
            model,
        })
    }

    fn process(&mut self, frame: &mut [f32; HOP]) -> Result<f32, String> {
        self.noisy
            .as_slice_mut()
            .expect("contiguous")
            .copy_from_slice(frame);
        let lsnr = self
            .model
            .process(self.noisy.view(), self.enh.view_mut())
            .map_err(|e| e.to_string())?;
        frame.copy_from_slice(self.enh.as_slice().expect("contiguous"));
        Ok(lsnr)
    }
}

struct Light {
    state: Box<DenoiseState<'static>>,
    input: [f32; HOP],
    output: [f32; HOP],
    /// RNNoise's overlap-add returns the previous hop, so the limit mixes that one back.
    previous: [f32; HOP],
}

impl Light {
    fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            input: [0.0; HOP],
            output: [0.0; HOP],
            previous: [0.0; HOP],
        }
    }

    fn process(&mut self, frame: &mut [f32; HOP], atten_lim_db: f32) {
        for (scaled, sample) in self.input.iter_mut().zip(frame.iter()) {
            *scaled = sample * PCM_SCALE;
        }
        self.state.process_frame(&mut self.output, &self.input);
        let keep = 10f32.powf(-atten_lim_db / 20.0);
        for ((sample, enhanced), noisy) in frame.iter_mut().zip(self.output).zip(&mut self.previous)
        {
            let enhanced = enhanced / PCM_SCALE;
            let current = *sample;
            *sample = enhanced + (*noisy - enhanced) * keep;
            *noisy = current;
        }
    }
}

/// RNNoise's speech probability, kept apart from whichever model denoises so
/// it means the same after a step-down — Discord runs Krisp VAD the same way.
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

    fn engines() -> [Denoiser; 2] {
        [Denoiser::new(), Denoiser::light()]
    }

    #[test]
    fn model_loads_with_the_pipeline_geometry() {
        let deep = Deep::new().expect("embedded DeepFilterNet model should load");
        assert_eq!(deep.model.sr, 48_000);
        assert_eq!(deep.model.hop_size, HOP);
        assert_eq!(Denoiser::new().engine(), Some(Engine::DeepFilterNet));
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
        for engine in [Engine::DeepFilterNet, Engine::RNNoise] {
            let levels: Vec<_> = [9.0, 30.0]
                .into_iter()
                .map(|strength| {
                    let mut denoiser = match engine {
                        Engine::DeepFilterNet => Denoiser::new(),
                        Engine::RNNoise => Denoiser::light(),
                    };
                    denoiser.set_atten_lim(strength);
                    rms(&run(&mut denoiser, &input)[HOP * 100..])
                })
                .collect();
            eprintln!("{engine:?} noise RMS at 9/30 dB suppression: {levels:?}");
            assert!(
                levels[1] < levels[0] * 0.7,
                "{engine:?}: stronger suppression must reduce noise: {levels:?}"
            );
        }
    }

    #[test]
    fn rnnoise_limit_mixes_back_the_hop_it_returns() {
        let input = noise(HOP * 20, 0.0000001);
        let mut denoiser = Denoiser::light();
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
    fn overloaded_models_step_down_then_stop() {
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
        assert_eq!(
            steps,
            [Some(Engine::DeepFilterNet), Some(Engine::RNNoise), None]
        );
        assert_eq!(denoiser.fallback(), Some(Fallback::Off));
        let mut untouched = hop.clone();
        denoiser.process_hop(&mut untouched);
        assert_eq!(untouched, hop);
    }

    #[test]
    fn a_spike_now_and_then_does_not_trip_the_guard() {
        let mut denoiser = Denoiser::light();
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
