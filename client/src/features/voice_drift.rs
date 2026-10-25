//! Playback drift: the device clock against libwebrtc's playout clock.
//!
//! NetEq already absorbs network jitter inside libwebrtc, so the mixer queue
//! only has to absorb the two clocks' difference, which is parts per thousand.
//! Dropping or repeating single samples to do that wobbled the pitch; the
//! remote track's resampler ratio is nudged instead, and the queue depth it
//! holds is the controlled quantity.

use std::collections::VecDeque;

/// Where the queue settles, and the band around it where nothing is corrected.
pub(super) const TARGET_MS: f64 = 40.0;
const DEAD_BAND_MS: f64 = 10.0;
/// Resample-ratio correction per millisecond of depth error, and its ceiling:
/// twice the worst clock mismatch seen, so a queue that got ahead can be
/// pulled back rather than only stopped from growing.
const GAIN_PER_MS: f64 = 0.0005;
const MAX_CORRECTION: f64 = 0.01;
/// Learns the clock mismatch itself, so the proportional term is not left
/// holding a standing depth error; critically damped against `GAIN_PER_MS`.
const INTEGRAL_PER_MS: f64 = 6.25e-7;
/// Depth is sampled once per 10 ms frame; the device pulls in blocks of up to
/// 20 ms, so this smooths that sawtooth (time constant about 200 ms).
const FILTER: f64 = 0.05;

#[derive(Default)]
pub(super) struct Control {
    filtered_ms: Option<f64>,
    learned: f64,
}

impl Control {
    /// Observes the queue depth after a push and returns the output-to-input
    /// ratio relative to nominal: above 1 slows playback and fills the queue.
    pub fn observe(&mut self, depth_ms: f64) -> f64 {
        let filtered = match self.filtered_ms {
            Some(previous) => previous + (depth_ms - previous) * FILTER,
            None => depth_ms,
        };
        self.filtered_ms = Some(filtered);
        let error = filtered - TARGET_MS;
        let outside = if error > DEAD_BAND_MS {
            error - DEAD_BAND_MS
        } else if error < -DEAD_BAND_MS {
            error + DEAD_BAND_MS
        } else {
            0.0
        };
        self.learned =
            (self.learned + outside * INTEGRAL_PER_MS).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        1.0 - (outside * GAIN_PER_MS + self.learned).clamp(-MAX_CORRECTION, MAX_CORRECTION)
    }
}

/// The overflow safety net. Past `cap` the queue is cut back once, to the
/// controller's target depth, and the join is crossfaded over 10 ms rather
/// than left as a step.
pub(super) fn bound(samples: &mut VecDeque<f32>, cap: usize, rate: u32) {
    if samples.len() <= cap {
        return;
    }
    let keep = ((f64::from(rate) * TARGET_MS / 1000.0) as usize).min(cap);
    let fade = (rate as usize / 100).min(keep);
    let cut = samples.len() - keep;
    for i in 0..fade {
        let t = (i + 1) as f32 / (fade + 1) as f32;
        let before = samples[i];
        let after = samples[cut + i];
        samples[cut + i] = before * (1.0 - t) + after * t;
    }
    samples.drain(..cut);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The producer runs at 48 kHz on its own clock, the device at 48 kHz on
    /// another, 0.5% apart either way. Depth has to settle near the target and
    /// the output has to stay a clean sine: no sample-to-sample jump larger
    /// than the sine's own slope.
    #[test]
    fn half_a_percent_of_clock_mismatch_settles_near_target_without_discontinuities() {
        for mismatch in [1.005_f64, 0.995] {
            let rate = 48_000u32;
            let mut resampler = super::super::AudioResampler::adaptive(rate, rate).unwrap();
            let mut control = Control::default();
            let mut queue: VecDeque<f32> = VecDeque::new();
            let mut out = Vec::new();
            let mut resampled = Vec::new();
            let mut phase = 0.0f64;
            let step = std::f64::consts::TAU * 440.0 / f64::from(rate);
            let cap = rate as usize / 5;
            let mut depths = Vec::new();
            // Producer frames arrive every 10 ms of producer time; the device
            // pulls 512 samples every 512 device samples. Both are stepped in
            // device time, the producer's by `mismatch`.
            let mut producer_due = 0.0f64;
            let mut device_due = 0.0f64;
            let total = f64::from(rate) * 60.0;
            let mut now = 0.0f64;
            while now < total {
                if producer_due <= device_due {
                    now = producer_due;
                    let frame: Vec<f32> = (0..480)
                        .map(|_| {
                            phase += step;
                            (0.5 * phase.sin()) as f32
                        })
                        .collect();
                    resampler.process_into(&frame, &mut resampled);
                    queue.extend(resampled.iter().copied());
                    bound(&mut queue, cap, rate);
                    let depth_ms = queue.len() as f64 * 1000.0 / f64::from(rate);
                    depths.push(depth_ms);
                    resampler.set_relative_ratio(control.observe(depth_ms));
                    producer_due += 480.0 / mismatch;
                } else {
                    now = device_due;
                    for _ in 0..512 {
                        out.push(queue.pop_front().unwrap_or(0.0));
                    }
                    device_due += 512.0;
                }
            }
            let settled = &depths[depths.len() / 2..];
            let (min, max) = settled
                .iter()
                .fold((f64::MAX, f64::MIN), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
            eprintln!("mismatch {mismatch}: settled depth {min:.1}..{max:.1} ms");
            assert!(
                min > TARGET_MS - 20.0 && max < TARGET_MS + 20.0,
                "mismatch {mismatch}: depth ranged {min:.1}..{max:.1} ms"
            );
            let settled_out = &out[out.len() / 2..];
            assert!(
                !settled_out.windows(8).any(|w| w.iter().all(|s| *s == 0.0)),
                "the queue ran dry"
            );
            let max_slope = 0.5 * (step * 1.02) as f32;
            let worst = settled_out
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max);
            assert!(
                worst <= max_slope,
                "mismatch {mismatch}: a jump of {worst} exceeds the sine's slope {max_slope}"
            );
        }
    }

    #[test]
    fn a_depth_inside_the_dead_band_is_left_alone_and_a_far_one_is_capped() {
        let mut control = Control::default();
        assert_eq!(control.observe(TARGET_MS + DEAD_BAND_MS - 1.0), 1.0);
        let mut control = Control::default();
        assert!((control.observe(TARGET_MS - 100.0) - (1.0 + MAX_CORRECTION)).abs() < 1e-9);
        let mut control = Control::default();
        assert!((control.observe(TARGET_MS + 100.0) - (1.0 - MAX_CORRECTION)).abs() < 1e-9);
    }

    #[test]
    fn an_overflow_is_cut_once_to_the_target_with_a_crossfaded_join() {
        let rate = 48_000;
        let cap = rate as usize / 5;
        let mut queue: VecDeque<f32> = (0..cap * 2)
            .map(|i| if i < cap { 0.1 } else { 0.7 })
            .collect();
        bound(&mut queue, cap, rate);
        let keep = (rate as usize) * TARGET_MS as usize / 1000;
        assert_eq!(queue.len(), keep);
        assert!(queue.iter().rev().take(480).all(|s| *s == 0.7));
        assert!(
            (queue[0] - 0.1).abs() < 0.01,
            "the join starts from the old audio"
        );
        let worst = queue
            .iter()
            .zip(queue.iter().skip(1))
            .map(|(a, b)| (b - a).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 0.01, "a step of {worst} in the join");
        let mut small: VecDeque<f32> = (0..cap).map(|_| 0.3).collect();
        bound(&mut small, cap, rate);
        assert_eq!(small.len(), cap, "at the cap nothing is cut");
    }
}
