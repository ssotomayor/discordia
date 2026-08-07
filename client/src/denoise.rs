//! Real-time microphone noise suppression, DeepFilterNet 3 via `tract`.
//!
//! Why this model and this runtime: DeepFilterNet 3 runs natively at 48 kHz
//! mono with a 480-sample (10 ms) hop, which is *exactly* the frame our voice
//! pipeline already hands to libwebrtc — no extra resampling, no extra
//! buffering, no added latency beyond the model's own 40 ms lookahead. `tract`
//! is a pure-Rust inference engine, so this builds on Windows MSVC, macOS
//! (Intel + Apple Silicon) and Linux with no C toolchain or ONNX Runtime to
//! ship, and the network weights are compiled into the binary (the crate's
//! `default-model` feature) rather than downloaded at runtime.
//!
//! Measured cost on an Apple Silicon core: ~150 µs per 10 ms hop, i.e. ~1.5% of
//! one core and ~65x realtime. That is comfortably inside the budget, but it is
//! still far too much work for cpal's realtime callback — so this runs on the
//! *publish* task (see `features::voice`), one hop at a time, off the audio
//! thread. Loading the model costs ~200 ms and allocates, so `Denoiser::new`
//! belongs on a blocking task, never on the first hop of a call.
//!
//! Suppression is deliberately not maximal: `atten_lim_db` caps how much the
//! model may pull down, which keeps the artefacts DeepFilterNet can introduce
//! on quiet speech (the "underwater" sound of an over-aggressive mask) out of
//! normal conversation.

use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::Array2;

/// Samples per hop the model consumes and emits: 10 ms of 48 kHz mono.
/// `features::voice::FRAME_SAMPLES` must agree — asserted in `Denoiser::new`.
pub const HOP: usize = 480;

/// Ceiling on suppression, in dB. Full strength (100 dB) scrubs steady noise
/// impressively but also chews on sibilants and room tone between words; 30 dB
/// removes keyboards, fans and street noise while leaving speech untouched.
const ATTEN_LIM_DB: f32 = 30.0;

pub struct Denoiser {
    model: DfTract,
    /// Reused I/O buffers — `process` wants `(channels, hop)` arrays, and
    /// allocating a pair of them 100 times a second is pure waste.
    noisy: Array2<f32>,
    enh: Array2<f32>,
}

impl Denoiser {
    /// Load the embedded DeepFilterNet 3 model. Takes ~200 ms and allocates;
    /// call it from `spawn_blocking`.
    pub fn new() -> Result<Self, String> {
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

    /// Denoise one hop in place. `frame` must be exactly `HOP` samples of mono
    /// 48 kHz audio in [-1, 1]; anything else is left untouched (a short frame
    /// is a pipeline bug, and passing the noisy audio through beats dropping
    /// audio on the floor).
    ///
    /// The model is stateful — it carries STFT overlap and GRU state between
    /// hops — so one `Denoiser` must serve one continuous capture stream, in
    /// order. Interleaving hops from two streams corrupts both.
    pub fn process_hop(&mut self, frame: &mut [f32]) {
        if frame.len() != HOP {
            return;
        }
        self.noisy
            .as_slice_mut()
            .expect("contiguous")
            .copy_from_slice(frame);
        if let Err(e) = self.model.process(self.noisy.view(), self.enh.view_mut()) {
            // Keep the call alive on the raw audio rather than going silent.
            eprintln!("[voice] denoise hop failed: {e}");
            return;
        }
        frame.copy_from_slice(self.enh.as_slice().expect("contiguous"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic broadband noise — xorshift, so the test can't flake on a
    /// lucky seed.
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

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// The embedded model loads and its geometry matches the voice pipeline.
    /// This is the check that catches a bad `deep_filter`/`tract` bump: those
    /// break at model-load time, not at compile time.
    #[test]
    fn model_loads_with_the_pipeline_geometry() {
        let d = Denoiser::new().expect("embedded DeepFilterNet model should load");
        assert_eq!(d.model.sr, 48_000);
        assert_eq!(d.model.hop_size, HOP);
    }

    /// Steady broadband noise with no speech in it should come out markedly
    /// quieter. The model needs a few hops to settle, so the assertion looks at
    /// the tail rather than the first frame.
    #[test]
    fn suppresses_speechless_noise() {
        let mut d = Denoiser::new().expect("model");
        let mut last = Vec::new();
        for _ in 0..50 {
            let mut hop = noise(HOP, 0.2);
            d.process_hop(&mut hop);
            last = hop;
        }
        let out = rms(&last);
        let input = rms(&noise(HOP, 0.2));
        assert!(
            out < input * 0.5,
            "expected noise to be attenuated: in={input:.4} out={out:.4}"
        );
    }

    /// A frame that isn't one hop long is a pipeline bug; pass it through
    /// untouched rather than panicking or emitting silence mid-call.
    #[test]
    fn wrong_length_frames_pass_through() {
        let mut d = Denoiser::new().expect("model");
        let mut short = vec![0.5f32; HOP - 1];
        d.process_hop(&mut short);
        assert!(short.iter().all(|s| *s == 0.5));
    }
}
