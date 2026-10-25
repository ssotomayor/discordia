use crate::audio_queue::{AudioReceiver, AudioSender, Frame, SAMPLES};
#[cfg(test)]
use std::time::{Duration, Instant};

pub struct Reference {
    tx: AudioSender<i16>,
    rate: u32,
    phase: u64,
    previous: f32,
    samples: [i16; SAMPLES],
    used: usize,
}

impl Reference {
    pub fn new(tx: AudioSender<i16>, rate: u32) -> Self {
        Self {
            tx,
            rate,
            phase: 0,
            previous: 0.0,
            samples: [0; SAMPLES],
            used: 0,
        }
    }

    pub fn push(&mut self, sample: f32) {
        self.phase += 48_000;
        while self.phase >= self.rate as u64 && self.rate > 0 {
            self.phase -= self.rate as u64;
            let fraction = 1.0 - self.phase as f32 / 48_000.0;
            let value = self.previous + (sample - self.previous) * fraction.clamp(0.0, 1.0);
            self.samples[self.used] = (value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            self.used += 1;
            if self.used == SAMPLES {
                let _ = crate::audio_queue::offer(&self.tx, Frame::new(self.samples));
                self.used = 0;
            }
        }
        self.previous = sample;
    }
}

/// Past this backlog the oldest frames are dropped in one block, so the
/// reference keeps its order and has one gap instead of one per late frame.
const REFERENCE_BACKLOG: usize = 30;
const REFERENCE_KEEP: usize = 10;

/// A constant: AEC3 estimates the delay itself, and a hint that moved with
/// every frame's queue age made it re-estimate.
const STREAM_DELAY_HINT_MS: i32 = 50;

pub struct Echo {
    apm: libwebrtc::native::apm::AudioProcessingModule,
    reference: AudioReceiver<i16>,
    enabled: (bool, bool),
}

impl Echo {
    pub fn new(reference: AudioReceiver<i16>) -> Self {
        Self {
            apm: libwebrtc::native::apm::AudioProcessingModule::new(true, false, true, false),
            reference,
            enabled: (true, true),
        }
    }

    pub fn configure(&mut self, echo: bool, high_pass: bool) {
        if self.enabled != (echo, high_pass) {
            self.apm =
                libwebrtc::native::apm::AudioProcessingModule::new(echo, false, high_pass, false);
            self.enabled = (echo, high_pass);
        }
    }

    pub fn process(&mut self, samples: &mut [f32; SAMPLES]) -> Result<(), String> {
        if self.enabled == (false, false) {
            while self.reference.try_recv().is_ok() {}
            return Ok(());
        }
        let backlog = self.reference.len();
        if backlog > REFERENCE_BACKLOG {
            for _ in REFERENCE_KEEP..backlog {
                let _ = self.reference.try_recv();
            }
        }
        while let Ok(mut frame) = self.reference.try_recv() {
            self.apm
                .process_reverse_stream(&mut frame.samples, 48_000, 1)
                .map_err(|error| error.to_string())?;
        }
        self.apm
            .set_stream_delay_ms(STREAM_DELAY_HINT_MS)
            .map_err(|error| error.to_string())?;
        let mut pcm = samples.map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
        self.apm
            .process_stream(&mut pcm, 48_000, 1)
            .map_err(|error| error.to_string())?;
        for (sample, value) in samples.iter_mut().zip(pcm) {
            *sample = value as f32 / i16::MAX as f32;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_reference_is_resampled_and_bounded() {
        for rate in [44_100, 48_000, 96_000] {
            let (tx, mut rx) = crate::audio_queue::reference_channel();
            let mut writer = Reference::new(tx, rate);
            for _ in 0..rate / 100 {
                writer.push(0.25);
            }
            let frame = rx.try_recv().unwrap();
            assert_eq!(frame.samples.len(), 480);
            assert!(
                frame.samples[1..]
                    .iter()
                    .all(|sample| (*sample as f32 / i16::MAX as f32 - 0.25).abs() < 0.001)
            );
            assert!(rx.try_recv().is_err());
            for _ in 0..rate {
                writer.push(0.25);
            }
            assert_eq!(rx.len(), crate::audio_queue::REFERENCE_CAPACITY);
        }
    }

    /// A reference frame that waited is still the reference; only a backlog
    /// past what AEC3 can align is cut, once, keeping the newest.
    #[test]
    fn a_late_reference_is_kept_and_a_deep_backlog_is_trimmed_in_one_block() {
        let (tx, rx) = crate::audio_queue::reference_channel();
        let mut echo = Echo::new(rx);
        for _ in 0..REFERENCE_BACKLOG {
            tx.try_send(Frame {
                samples: [100; SAMPLES],
                captured: Instant::now() - Duration::from_millis(400),
                position: None,
            })
            .unwrap();
        }
        echo.process(&mut [0.0; SAMPLES]).unwrap();
        assert_eq!(echo.reference.len(), 0, "late frames feed the render path");
        for _ in 0..REFERENCE_BACKLOG + 5 {
            tx.try_send(Frame::new([100; SAMPLES])).unwrap();
        }
        assert_eq!(echo.reference.len(), REFERENCE_BACKLOG + 5);
        echo.process(&mut [0.0; SAMPLES]).unwrap();
        assert_eq!(echo.reference.len(), 0);
    }

    #[test]
    fn acoustic_echo_is_attenuated_after_convergence() {
        let (tx, rx) = crate::audio_queue::reference_channel();
        let mut echo = Echo::new(rx);
        let mut history = std::collections::VecDeque::from(vec![[0.0; SAMPLES]; 5]);
        let mut seed = 79u32;
        let (mut before, mut after) = (0.0f64, 0.0f64);
        for hop in 0..1500 {
            let reference = std::array::from_fn(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                (seed as f32 / u32::MAX as f32 - 0.5) * 0.4
            });
            tx.try_send(Frame::new(
                reference.map(|sample| (sample * i16::MAX as f32) as i16),
            ))
            .unwrap();
            history.push_back(reference);
            let mut mic = history.pop_front().unwrap().map(|sample| sample * 0.5);
            if hop > 1000 {
                before += mic
                    .iter()
                    .map(|sample| (*sample as f64).powi(2))
                    .sum::<f64>();
            }
            echo.process(&mut mic).unwrap();
            if hop > 1000 {
                after += mic
                    .iter()
                    .map(|sample| (*sample as f64).powi(2))
                    .sum::<f64>();
            }
        }
        assert!(after < before * 0.25, "echo power ratio {}", after / before);
    }
}
