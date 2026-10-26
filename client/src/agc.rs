//! Automatic gain control for the microphone: libwebrtc's AGC2, run on its own
//! after noise suppression. Issue #170 has why the APM on the pushed source never runs.

use cxx::UniquePtr;
use webrtc_sys::apm::ffi;

use crate::denoise::HOP;

const HEADROOM_DB: f32 = 5.0;
const MAX_GAIN_DB: f32 = 30.0;
const INITIAL_GAIN_DB: f32 = 0.0;
const MAX_GAIN_CHANGE_DB_PER_SEC: f32 = 6.0;
const MAX_OUTPUT_NOISE_DBFS: f32 = -50.0;
const CEILING: f32 = 0.98;

/// Below this a hop says nothing about the gain applied to it.
const GAIN_FLOOR_RMS: f32 = 0.001;

pub struct Agc {
    apm: UniquePtr<ffi::AudioProcessingModule>,
    gain_db: f32,
}

impl Agc {
    pub fn new() -> Self {
        Self {
            apm: ffi::create_gain_controller(
                HEADROOM_DB,
                MAX_GAIN_DB,
                INITIAL_GAIN_DB,
                MAX_GAIN_CHANGE_DB_PER_SEC,
                MAX_OUTPUT_NOISE_DBFS,
            ),
            gain_db: INITIAL_GAIN_DB,
        }
    }

    /// What the last speech hop was raised by, limiter included. AGC2 keeps its
    /// own gain to itself, so this is measured, for the call recording only.
    pub fn gain_db(&self) -> f32 {
        self.gain_db
    }

    pub fn process(&mut self, hop: &mut [f32; HOP]) -> Result<(), String> {
        let before = rms(hop);
        let mut pcm = hop.map(|sample| (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
        let status = unsafe {
            self.apm.pin_mut().process_stream(
                pcm.as_ptr(),
                pcm.len(),
                pcm.as_mut_ptr(),
                pcm.len(),
                48_000,
                1,
            )
        };
        if status != 0 {
            return Err(format!("gain control failed: {status}"));
        }
        for (sample, value) in hop.iter_mut().zip(pcm) {
            *sample = value as f32 / i16::MAX as f32;
        }
        if before > GAIN_FLOOR_RMS {
            self.gain_db = 20.0 * (rms(hop).max(f32::MIN_POSITIVE) / before).log10();
        }
        Ok(())
    }
}

fn rms(hop: &[f32]) -> f32 {
    (hop.iter().map(|s| s * s).sum::<f32>() / hop.len().max(1) as f32).sqrt()
}

const RELEASE_SECS: f32 = 0.05;

/// Peak ceiling after the manual volume: the gain drops the instant a sample
/// would pass it and recovers over `RELEASE_SECS`, carried across hops.
pub struct Limiter {
    gain: f32,
    release: f32,
}

impl Default for Limiter {
    fn default() -> Self {
        Self {
            gain: 1.0,
            release: 1.0 - (-1.0 / (RELEASE_SECS * 48_000.0)).exp(),
        }
    }
}

impl Limiter {
    pub fn process(&mut self, hop: &mut [f32]) {
        for sample in hop {
            let target = CEILING / sample.abs().max(CEILING);
            self.gain = (self.gain + (1.0 - self.gain) * self.release).min(target);
            *sample *= self.gain;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    struct Rng(u32);

    impl Rng {
        fn next(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0 as f32 / u32::MAX as f32
        }

        fn between(&mut self, low: f32, high: f32) -> f32 {
            low + (high - low) * self.next()
        }
    }

    struct Resonator {
        a1: f32,
        a2: f32,
        gain: f32,
        y: [f32; 2],
    }

    impl Resonator {
        fn tune(&mut self, hz: f32, bandwidth: f32) {
            let r = (-std::f32::consts::PI * bandwidth / 48_000.0).exp();
            self.a1 = -2.0 * r * (std::f32::consts::TAU * hz / 48_000.0).cos();
            self.a2 = r * r;
            self.gain = 1.0 - r;
        }

        fn step(&mut self, x: f32) -> f32 {
            let y = self.gain * x - self.a1 * self.y[0] - self.a2 * self.y[1];
            self.y = [y, self.y[0]];
            y
        }
    }

    /// Phrases of voiced syllables with pauses between: a pulse train at a
    /// wandering pitch through vowel formants, each syllable stressed apart.
    pub(crate) fn speech(seconds: f32, level_dbfs: f32, seed: u32) -> Vec<f32> {
        const VOWELS: [[f32; 3]; 5] = [
            [730.0, 1090.0, 2440.0],
            [270.0, 2290.0, 3010.0],
            [300.0, 870.0, 2240.0],
            [530.0, 1840.0, 2480.0],
            [570.0, 840.0, 2410.0],
        ];
        let len = (seconds * 48_000.0) as usize;
        let mut rng = Rng(seed | 1);
        let mut out = vec![0.0f32; len];
        let mut voiced = vec![false; len];
        let mut formants: Vec<_> = (0..3)
            .map(|_| Resonator {
                a1: 0.0,
                a2: 0.0,
                gain: 0.0,
                y: [0.0; 2],
            })
            .collect();
        let mut phase = 0.0f32;
        let mut at = (0.3 * 48_000.0) as usize;
        while at < len {
            for _ in 0..(5 + (rng.next() * 5.0) as usize) {
                let length = (rng.between(0.16, 0.26) * 48_000.0) as usize;
                let vowel = VOWELS[(rng.next() * VOWELS.len() as f32) as usize % VOWELS.len()];
                for (resonator, (hz, bandwidth)) in formants
                    .iter_mut()
                    .zip(vowel.iter().zip([80.0, 90.0, 120.0]))
                {
                    resonator.tune(*hz, bandwidth);
                }
                let stress = 10f32.powf(rng.between(-6.0, 3.0) / 20.0);
                let pitch = rng.between(100.0, 150.0);
                let ramp = 0.03 * 48_000.0;
                for i in 0..length.min(len.saturating_sub(at)) {
                    let t = i as f32;
                    let envelope = (t / ramp).min(1.0).min((length as f32 - t) / ramp).max(0.0);
                    let hz = pitch * (1.0 + 0.08 * (t / length as f32 - 0.5));
                    phase += hz / 48_000.0;
                    let mut x = (rng.next() - 0.5) * 0.05;
                    if phase >= 1.0 {
                        phase -= 1.0;
                        x += 1.0;
                    }
                    let y = formants.iter_mut().fold(x, |x, f| f.step(x));
                    out[at + i] = y * envelope * stress;
                    voiced[at + i] = true;
                }
                at += length + (rng.between(0.04, 0.08) * 48_000.0) as usize;
            }
            at += (rng.between(0.4, 0.8) * 48_000.0) as usize;
        }
        let power = out
            .iter()
            .zip(&voiced)
            .filter(|(_, v)| **v)
            .map(|(s, _)| s * s)
            .sum::<f32>()
            / voiced.iter().filter(|v| **v).count().max(1) as f32;
        let scale = 10f32.powf(level_dbfs / 20.0) / power.sqrt();
        for sample in &mut out {
            *sample *= scale;
        }
        out
    }

    fn dbfs(samples: &[f32]) -> f32 {
        20.0 * rms(samples).max(1e-9).log10()
    }

    /// The output, and the gain on every hop that carried speech.
    fn run(input: &[f32], speech_rms: f32) -> (Vec<f32>, Vec<f32>) {
        let mut agc = Agc::new();
        let mut out = Vec::with_capacity(input.len());
        let mut gains = Vec::new();
        for hop in input.as_chunks::<HOP>().0 {
            let mut processed = *hop;
            agc.process(&mut processed).unwrap();
            gains.push(if rms(hop) > speech_rms * 0.3 {
                dbfs(&processed) - dbfs(hop)
            } else {
                f32::NAN
            });
            out.extend_from_slice(&processed);
        }
        (out, gains)
    }

    fn spread(gains: &[f32]) -> (f32, f32, f32) {
        let mut speech: Vec<_> = gains.iter().copied().filter(|g| !g.is_nan()).collect();
        speech.sort_by(f32::total_cmp);
        let at = |q: usize| speech[speech.len() * q / 100];
        (at(5), at(50), at(95))
    }

    const SETTLED: usize = 8 * 100;

    /// The gain is a property of the speaker, not of the syllable: a per-hop
    /// gain swung 11 dB across one phrase, louder and quieter by the word.
    #[test]
    fn speech_keeps_one_gain_from_word_to_word() {
        let input = speech(20.0, -30.0, 7);
        let (_, gains) = run(&input, 10f32.powf(-30.0 / 20.0));
        let (low, _, high) = spread(&gains[SETTLED..]);
        assert!(high - low < 7.0, "gain ranged {low:.1}..{high:.1} dB");
    }

    #[test]
    fn quiet_speech_is_raised_and_loud_speech_is_left_alone() {
        for level in [-40.0, -30.0, -20.0] {
            let input = speech(20.0, level, 11);
            let (out, _) = run(&input, 10f32.powf(level / 20.0));
            let landed = dbfs(&out[SETTLED * HOP..]) - dbfs(&input[SETTLED * HOP..]) + level;
            assert!(
                (-21.0..-12.0).contains(&landed),
                "{level} dBFS speech landed at {landed:.1}"
            );
        }
        let input = speech(20.0, -10.0, 11);
        let (_, gains) = run(&input, 10f32.powf(-10.0 / 20.0));
        let (_, median, high) = spread(&gains[SETTLED..]);
        assert!(
            median < 0.5 && high < 1.0,
            "loud speech raised {median:.1}/{high:.1} dB"
        );
    }

    /// Room tone between phrases must not walk the gain up, or the next word
    /// arrives shouting.
    #[test]
    fn a_pause_in_room_tone_keeps_the_gain() {
        let level = -30.0;
        let mut input = speech(12.0, level, 3);
        let mut rng = Rng(5);
        let noise = 10f32.powf(-55.0 / 20.0) * 3f32.sqrt() * 2.0;
        input.extend((0..6 * 48_000).map(|_| (rng.next() - 0.5) * noise));
        input.extend(speech(4.0, level, 9));
        let (out, gains) = run(&input, 10f32.powf(level / 20.0));
        let (_, before, _) = spread(&gains[1000..1200]);
        let (_, after, _) = spread(&gains[1800..1900]);
        assert!(
            (after - before).abs() < 3.0,
            "gain {before:.1} before the pause, {after:.1} after"
        );
        let tone = 1300 * HOP..1700 * HOP;
        let lifted = dbfs(&out[tone.clone()]) - dbfs(&input[tone]);
        assert!(
            lifted < before + 1.0,
            "room tone raised {lifted:.1} dB, speech {before:.1}"
        );
    }

    #[test]
    fn manual_boost_is_limited_without_flattening_the_waveform() {
        let mut samples = [1.6, -0.8, 0.4];
        Limiter::default().process(&mut samples);
        assert!((samples[0] - 0.98).abs() < 1e-6);
        assert!((samples[1] + 0.49).abs() < 1e-3);
        assert!((samples[2] - 0.245).abs() < 1e-3);
    }

    /// One gain per 10 ms hop stepped a hundred times a second on loud speech,
    /// which is a buzz. The gain now moves per sample, so a hop boundary is no
    /// different from any other pair of samples.
    #[test]
    fn a_loud_sine_at_double_volume_sees_no_gain_step_at_hop_boundaries() {
        let mut limiter = Limiter::default();
        let input: Vec<f32> = (0..HOP * 100)
            .map(|n| 0.9 * 2.0 * (std::f32::consts::TAU * 1000.0 * n as f32 / 48_000.0 + 0.3).sin())
            .collect();
        let mut output = input.clone();
        for hop in output.chunks_mut(HOP) {
            limiter.process(hop);
        }
        assert!(output.iter().all(|s| s.abs() <= CEILING + 1e-6));
        let gain = |n: usize| output[n] / input[n];
        for hop in 2..100 {
            let at = hop * HOP;
            let step = (gain(at) - gain(at - 1)).abs();
            assert!(step < 1e-3, "hop {hop}: gain stepped {step}");
        }
        assert!(gain(HOP * 50) < 0.6, "double volume must be held down");
    }

    #[test]
    fn the_gain_recovers_after_a_peak_instead_of_staying_down() {
        let mut limiter = Limiter::default();
        let mut peak = [0.1; HOP];
        peak[0] = 1.8;
        limiter.process(&mut peak);
        assert!((peak[0] - CEILING).abs() < 1e-6);
        assert!(peak[1] < 0.06);
        let mut after = Vec::new();
        for _ in 0..30 {
            let mut hop = [0.1; HOP];
            limiter.process(&mut hop);
            after.extend_from_slice(&hop);
        }
        assert!(
            after.windows(2).all(|w| w[1] >= w[0] - 1e-7),
            "recovery is monotonic"
        );
        assert!(
            (after[after.len() - 1] - 0.1).abs() < 1e-3,
            "back to unity within 300 ms"
        );
        let at_50ms = after[HOP * 5];
        assert!(
            at_50ms > 0.08 && at_50ms < 0.1,
            "a 50 ms release: {at_50ms}"
        );
    }
}
