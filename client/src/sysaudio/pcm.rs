use crate::audio_queue::{Frame, SAMPLES};

pub(super) fn pcm16_frame(samples: [i16; SAMPLES]) -> Frame<f32> {
    Frame::new(samples.map(|sample| sample as f32 / 32768.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_pcm_keeps_silence_polarity_and_full_scale_without_clipping() {
        let mut samples = [0; SAMPLES];
        samples[..5].copy_from_slice(&[i16::MIN, -16384, 0, 16384, i16::MAX]);
        let frame = pcm16_frame(samples);
        assert_eq!(&frame.samples[..4], &[-1.0, -0.5, 0.0, 0.5]);
        assert!(frame.samples[4] < 1.0 && frame.samples[4] > 0.999);
        assert!(frame.samples[5..].iter().all(|sample| *sample == 0.0));
        assert!(frame.is_fresh());
    }
}
