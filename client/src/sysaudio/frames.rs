use crate::audio_queue::{AudioSender, Frame, offer};

pub const FRAME: usize = crate::audio_queue::SAMPLES;

pub struct FrameCutter {
    tx: AudioSender<f32>,
    pending: [f32; FRAME],
    filled: usize,
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    emitted: u64,
}

impl FrameCutter {
    pub fn new(tx: AudioSender<f32>) -> Self {
        Self {
            tx,
            pending: [0.0; FRAME],
            filled: 0,
            emitted: 0,
        }
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn emitted(&self) -> u64 {
        self.emitted
    }

    pub fn push_mono(&mut self, mut samples: &[f32]) -> bool {
        while !samples.is_empty() {
            let count = samples.len().min(FRAME - self.filled);
            self.pending[self.filled..self.filled + count].copy_from_slice(&samples[..count]);
            self.filled += count;
            samples = &samples[count..];
            if !self.flush() {
                return false;
            }
        }
        true
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn push_interleaved(&mut self, samples: &[f32], channels: usize) -> bool {
        if channels <= 1 {
            return self.push_mono(samples);
        }
        for chunk in samples.chunks_exact(channels) {
            let mono = chunk.iter().sum::<f32>() / channels as f32;
            if !self.push_mono(&[mono]) {
                return false;
            }
        }
        true
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn push_silence(&mut self, mut count: usize) -> bool {
        while count > 0 {
            let fill = count.min(FRAME - self.filled);
            self.pending[self.filled..self.filled + fill].fill(0.0);
            self.filled += fill;
            count -= fill;
            if !self.flush() {
                return false;
            }
        }
        true
    }

    fn flush(&mut self) -> bool {
        if self.filled < FRAME {
            return true;
        }
        self.filled = 0;
        self.emitted += 1;
        offer(&self.tx, Frame::new(self.pending)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_queue::channel;

    #[test]
    fn partial_packets_keep_order_across_capture_callbacks() {
        let (tx, mut rx) = channel();
        let mut cutter = FrameCutter::new(tx);
        assert!(cutter.push_mono(&[0.25; 170]));
        assert!(rx.try_recv().is_err());
        assert!(cutter.push_mono(&[0.5; 400]));
        let first = rx.try_recv().unwrap().samples;
        assert_eq!(&first[..170], &[0.25; 170]);
        assert_eq!(&first[170..], &[0.5; 310]);
        assert!(cutter.push_silence(390));
        let next = rx.try_recv().unwrap().samples;
        assert_eq!(&next[..90], &[0.5; 90]);
        assert_eq!(&next[90..], &[0.0; 390]);
        assert_eq!(cutter.emitted(), 2);
    }

    #[test]
    fn stereo_mix_and_overload_recover_without_buffering_old_audio() {
        let (tx, mut rx) = channel();
        let mut cutter = FrameCutter::new(tx);
        let stereo: Vec<_> = (0..FRAME).flat_map(|_| [0.75, 0.25]).collect();
        assert!(cutter.push_interleaved(&stereo, 2));
        assert_eq!(rx.try_recv().unwrap().samples, [0.5; FRAME]);
        assert!(cutter.push_silence(FRAME * 100));
        assert_eq!(cutter.emitted(), 101);
        while rx.try_recv().is_ok() {}
        assert!(cutter.push_mono(&[0.75; FRAME]));
        assert_eq!(rx.try_recv().unwrap().samples, [0.75; FRAME]);
        drop(rx);
        assert!(!cutter.push_silence(FRAME));
    }
}
