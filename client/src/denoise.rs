use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::Array2;

/// Fixed by the model. `features::voice::FRAME_SAMPLES` must agree, and one
/// `Denoiser` must serve one continuous stream — it carries state across hops.
pub const HOP: usize = 480;

const ATTEN_LIM_DB: f32 = 30.0;

pub struct Denoiser {
    model: DfTract,
    noisy: Array2<f32>,
    enh: Array2<f32>,
}

impl Denoiser {
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

    pub fn set_atten_lim(&mut self, db: f32) {
        self.model.set_atten_lim(db);
    }

    pub fn process_hop(&mut self, frame: &mut [f32]) {
        if frame.len() != HOP {
            return;
        }
        self.noisy
            .as_slice_mut()
            .expect("contiguous")
            .copy_from_slice(frame);
        if let Err(e) = self.model.process(self.noisy.view(), self.enh.view_mut()) {
            eprintln!("[voice] denoise hop failed: {e}");
            return;
        }
        frame.copy_from_slice(self.enh.as_slice().expect("contiguous"));
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

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    #[test]
    fn model_loads_with_the_pipeline_geometry() {
        let d = Denoiser::new().expect("embedded DeepFilterNet model should load");
        assert_eq!(d.model.sr, 48_000);
        assert_eq!(d.model.hop_size, HOP);
    }

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

    #[test]
    fn wrong_length_frames_pass_through() {
        let mut d = Denoiser::new().expect("model");
        let mut short = vec![0.5f32; HOP - 1];
        d.process_hop(&mut short);
        assert!(short.iter().all(|s| *s == 0.5));
    }
}
