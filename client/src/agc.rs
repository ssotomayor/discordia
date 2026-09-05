//! Automatic gain control for the microphone.
//!
//! libwebrtc's own AGC never runs on our audio, so whatever it promised has to
//! happen here instead. Issue #170 has why.

/// Where speech should land. Well under full scale, so a syllable louder than
/// the average still has somewhere to go.
const TARGET_RMS: f32 = 0.126;
const MAX_GAIN: f32 = 10.0;
const MIN_GAIN: f32 = 0.25;
/// Below this a hop is room tone, not speech, and lifting it would just raise
/// the noise floor between words.
const FLOOR_RMS: f32 = 0.002;
const CEILING: f32 = 0.98;

/// Slow up, quick down: chasing every quiet moment pumps, and being late on a
/// loud one clips.
const RISE_DB_PER_SEC: f32 = 6.0;
const FALL_DB_PER_SEC: f32 = 40.0;

const HOP_SECS: f32 = crate::denoise::HOP as f32 / 48_000.0;

pub struct Agc {
    gain: f32,
    rise: f32,
    fall: f32,
}

impl Agc {
    pub fn new() -> Self {
        Self {
            gain: 1.0,
            rise: 10f32.powf(RISE_DB_PER_SEC / 20.0 * HOP_SECS),
            fall: 10f32.powf(-FALL_DB_PER_SEC / 20.0 * HOP_SECS),
        }
    }

    pub fn reset(&mut self) {
        self.gain = 1.0;
    }

    /// Brings one hop toward `TARGET_RMS` and applies the gain in place.
    pub fn process(&mut self, hop: &mut [f32]) {
        let rms = (hop.iter().map(|s| s * s).sum::<f32>() / hop.len().max(1) as f32).sqrt();
        if rms > FLOOR_RMS {
            let want = (TARGET_RMS / rms).clamp(MIN_GAIN, MAX_GAIN);
            self.gain = if want > self.gain {
                (self.gain * self.rise).min(want)
            } else {
                (self.gain * self.fall).max(want)
            };
        }

        // Ahead of the ramp, not on it: a transient arrives inside one hop and
        // 40 dB/s would still be lowering the gain long after it clipped.
        let peak = hop.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        if peak * self.gain > CEILING {
            self.gain = CEILING / peak;
        }

        for s in hop.iter_mut() {
            *s *= self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::denoise::HOP;

    /// Alternating full-amplitude samples: rms and peak are both `amp`, so the
    /// arithmetic in a test says what it means.
    fn hop(amp: f32) -> Vec<f32> {
        (0..HOP)
            .map(|i| if i % 2 == 0 { amp } else { -amp })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// The whole point: a quiet mic ends up at the target instead of staying
    /// quiet, which is the difference people hear against other clients.
    #[test]
    fn a_quiet_voice_is_brought_up_to_the_target() {
        let mut agc = Agc::new();
        let mut last = 0.0;
        for _ in 0..600 {
            let mut h = hop(0.02);
            agc.process(&mut h);
            last = rms(&h);
        }
        assert!(
            (last - TARGET_RMS).abs() < 0.02,
            "settled at {last}, wanted {TARGET_RMS}"
        );
    }

    /// A loud one comes down instead of clipping the encoder.
    #[test]
    fn a_loud_voice_is_brought_down() {
        let mut agc = Agc::new();
        for _ in 0..600 {
            let mut h = hop(0.9);
            agc.process(&mut h);
            assert!(rms(&h) <= 0.9, "gain went up on an already loud hop");
        }
        let mut h = hop(0.9);
        agc.process(&mut h);
        assert!(rms(&h) < 0.5, "still at {}", rms(&h));
    }

    /// Room tone between words must not be lifted, or the gain walks up during
    /// every silence and the next word arrives shouting.
    #[test]
    fn silence_does_not_move_the_gain() {
        let mut agc = Agc::new();
        for _ in 0..600 {
            agc.process(&mut hop(0.02));
        }
        let settled = agc.gain;
        for _ in 0..600 {
            agc.process(&mut hop(0.0005));
        }
        assert_eq!(agc.gain, settled);
    }

    /// The ramp cannot answer a transient in time, so the limiter has to.
    #[test]
    fn no_hop_can_leave_full_scale() {
        let mut agc = Agc::new();
        for _ in 0..600 {
            agc.process(&mut hop(0.02));
        }
        for amp in [0.05, 0.3, 0.7, 1.0] {
            let mut h = hop(amp);
            agc.process(&mut h);
            let peak = h.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak <= CEILING + 1e-6, "amp {amp} left at {peak}");
        }
    }

    /// Rising fast enough to hear it happen is the failure this guards.
    #[test]
    fn the_gain_rises_no_faster_than_the_ramp() {
        let mut agc = Agc::new();
        let mut prev = agc.gain;
        for _ in 0..100 {
            agc.process(&mut hop(0.02));
            assert!(
                agc.gain <= prev * 10f32.powf(RISE_DB_PER_SEC / 20.0 * HOP_SECS) + 1e-6,
                "jumped from {prev} to {}",
                agc.gain
            );
            prev = agc.gain;
        }
    }

    #[test]
    fn reset_returns_to_unity() {
        let mut agc = Agc::new();
        for _ in 0..100 {
            agc.process(&mut hop(0.02));
        }
        agc.reset();
        let mut h = hop(0.3);
        agc.process(&mut h);
        assert!((rms(&h) - 0.3).abs() < 0.05, "not near unity: {}", rms(&h));
    }
}
