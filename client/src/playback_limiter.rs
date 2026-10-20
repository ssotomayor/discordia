use std::collections::VecDeque;

pub struct Limiter {
    delay: Vec<f32>,
    cursor: usize,
    peaks: VecDeque<(u64, f32)>,
    index: u64,
    gain: f64,
    attack: f64,
    release: f64,
}

impl Limiter {
    pub fn gain(&self) -> f32 {
        self.gain as f32
    }

    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let lookahead = (sample_rate as usize / 200).max(1);
        Self {
            delay: vec![0.0; lookahead],
            cursor: 0,
            peaks: VecDeque::with_capacity(lookahead + 1),
            index: 0,
            gain: 1.0,
            attack: (-1.0 / (0.0005 * rate)).exp(),
            release: (-1.0 / (0.100 * rate)).exp(),
        }
    }

    pub fn process(&mut self, sample: f32) -> f32 {
        let sample = if sample.is_finite() { sample } else { 0.0 };
        let delayed = self.delay[self.cursor];
        self.delay[self.cursor] = sample;
        self.cursor = (self.cursor + 1) % self.delay.len();

        let oldest = self.index.saturating_sub(self.delay.len() as u64);
        while self.peaks.front().is_some_and(|&(index, _)| index < oldest) {
            self.peaks.pop_front();
        }
        let peak = sample.abs();
        while self.peaks.back().is_some_and(|&(_, value)| value <= peak) {
            self.peaks.pop_back();
        }
        self.peaks.push_back((self.index, peak));
        self.index += 1;
        let peak = self.peaks.front().map_or(0.0, |&(_, peak)| peak);
        let target = 1.0 / f64::from(peak.max(1.0));
        let coefficient = if target < self.gain {
            self.attack
        } else {
            self.release
        };
        self.gain = target + coefficient * (self.gain - target);
        // Extreme peaks can outrun the attack ramp; bound gain before the delayed sample escapes.
        self.gain = self.gain.min(1.0 / f64::from(delayed.abs().max(1.0)));
        if target == 1.0 && 1.0 - self.gain < 0.000_001 {
            self.gain = 1.0;
        }
        ((f64::from(delayed) * self.gain) as f32).clamp(-1.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_audio_is_only_delayed_at_each_device_rate() {
        for rate in [16_000, 44_100, 48_000, 96_000, 192_000] {
            let mut limiter = Limiter::new(rate);
            let delay = limiter.delay.len();
            let input: Vec<_> = (0..rate as usize)
                .map(|i| 0.9 * (i as f32 * 0.13).sin())
                .collect();
            for i in 0..input.len() + delay {
                let output = limiter.process(input.get(i).copied().unwrap_or(0.0));
                let expected = i.checked_sub(delay).map_or(0.0, |i| input[i]);
                assert_eq!(output, expected, "rate={rate}, sample={i}");
            }
        }
    }

    #[test]
    fn an_overloaded_wave_is_scaled_instead_of_flattened() {
        let mut limiter = Limiter::new(48_000);
        let delay = limiter.delay.len();
        let input: Vec<_> = (0..48_000)
            .map(|i| 2.0 * (std::f32::consts::TAU * 200.0 * i as f32 / 48_000.0).sin())
            .collect();
        let mut limited_error = 0.0_f64;
        let mut clipped_error = 0.0_f64;
        for i in 0..input.len() {
            let output = limiter.process(input[i]);
            assert!(output.abs() <= 1.0);
            if i > 4800 {
                let raw = input[i - delay];
                let expected = raw * 0.5;
                limited_error += f64::from(output - expected).powi(2);
                clipped_error += f64::from(raw.clamp(-1.0, 1.0) - expected).powi(2);
            }
        }
        assert!(limited_error < clipped_error * 0.01);
    }

    #[test]
    fn overload_recovery_is_gradual_and_does_not_raise_quiet_audio() {
        let mut limiter = Limiter::new(48_000);
        for _ in 0..4800 {
            limiter.process(4.0);
        }
        for _ in 0..limiter.delay.len() + 1 {
            limiter.process(0.1);
        }
        let first = limiter.process(0.1);
        let mut previous = first;
        for _ in 0..48_000 {
            let output = limiter.process(0.1);
            assert!(output >= previous - 0.000_001);
            assert!(output - previous < 0.000_02);
            assert!(output <= 0.1);
            previous = output;
        }
        assert!(first < 0.03);
        assert!(previous > 0.099);
        for _ in 0..48_000 {
            limiter.process(0.1);
        }
        assert_eq!(limiter.process(0.1), 0.1);
    }

    #[test]
    fn abrupt_peaks_and_invalid_samples_never_escape_full_scale() {
        for rate in [8000, 44_100, 48_000, 96_000] {
            let mut limiter = Limiter::new(rate);
            for i in 0..rate {
                let sample = match i % 11 {
                    0 => 1000.0,
                    1 => -1000.0,
                    2 => f32::NAN,
                    3 => f32::INFINITY,
                    _ => 0.7,
                };
                let output = limiter.process(sample);
                assert!(output.is_finite() && output.abs() <= 1.0);
            }
        }
    }
}
