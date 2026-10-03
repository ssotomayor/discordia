use crate::audio_queue::{AudioReceiver, AudioSender, Frame, SAMPLES};

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

pub struct Echo {
    apm: libwebrtc::native::apm::AudioProcessingModule,
    reference: AudioReceiver<i16>,
}

impl Echo {
    pub fn new(reference: AudioReceiver<i16>) -> Self {
        Self {
            apm: libwebrtc::native::apm::AudioProcessingModule::new(true, false, true, false),
            reference,
        }
    }

    pub fn process(&mut self, samples: &mut [f32; SAMPLES], delay_ms: i32) -> Result<(), String> {
        while let Ok(mut frame) = self.reference.try_recv() {
            if frame.is_fresh() {
                self.apm
                    .process_reverse_stream(&mut frame.samples, 48_000, 1)
                    .map_err(|error| error.to_string())?;
            }
        }
        self.apm
            .set_stream_delay_ms(delay_ms.clamp(0, 500))
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
            let (tx, mut rx) = crate::audio_queue::channel();
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
            assert_eq!(rx.len(), 8);
        }
    }

    #[test]
    fn acoustic_echo_is_attenuated_after_convergence() {
        let (tx, rx) = crate::audio_queue::channel();
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
            echo.process(&mut mic, 50).unwrap();
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
