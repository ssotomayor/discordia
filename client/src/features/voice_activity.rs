use super::*;
use crate::audio_queue::Frame;
use crate::mic_profile::Config;
use std::collections::VecDeque;

const LEAD_IN: usize = 3;

#[derive(Default)]
pub(super) struct VoiceGate {
    attack: u32,
    remaining: u32,
    open: bool,
}

impl VoiceGate {
    pub fn silence(&mut self) {
        *self = Self::default();
    }

    pub fn step(&mut self, probability: f32, peak: i32, threshold: i32) -> GateAction {
        let above = peak
            >= if self.open {
                (threshold / 2).max(1)
            } else {
                threshold.max(1)
            };
        let confident = probability >= if self.open { 0.35 } else { 0.6 };
        if above && confident {
            self.attack += 1;
            if self.open || self.attack >= 2 {
                self.remaining = GATE_HANGOVER_FRAMES;
            }
        } else {
            self.attack = 0;
            self.remaining = self.remaining.saturating_sub(1);
        }
        let open = self.remaining > 0;
        let action = match (self.open, open) {
            (false, false) => GateAction::Drop,
            (false, true) => GateAction::RampIn,
            (true, true) => GateAction::Pass,
            (true, false) => GateAction::RampOut,
        };
        self.open = open;
        action
    }
}

pub(super) struct LeadIn {
    pending: VecDeque<Frame<f32>>,
    config: Option<Config>,
}

impl LeadIn {
    pub fn new() -> Self {
        Self {
            pending: VecDeque::with_capacity(LEAD_IN),
            config: None,
        }
    }

    pub fn clear(&mut self) {
        self.pending.clear();
    }

    pub fn push_clocked(
        &mut self,
        frame: Frame<f32>,
        action: GateAction,
        config: Config,
        muted: bool,
        output: &mut Vec<Frame<f32>>,
    ) {
        let retained =
            config.processing.gate && config.processing.voice_detection && config.volume > 0;
        let silence_source = if retained && action == GateAction::Drop {
            self.pending.front().filter(|old| {
                self.pending.len() == LEAD_IN
                    && self.config == Some(config)
                    && old.position.map(|p| p.capture_id) == frame.position.map(|p| p.capture_id)
            })
        } else {
            Some(&frame)
        };
        let silence = silence_source.map(|frame| Frame {
            samples: [0.0; FRAME_SAMPLES],
            captured: frame.captured,
            position: frame.position,
        });
        self.push(frame, action, config, muted, output);
        // Silence follows the VAD lead-in so reopening cannot replay older capture positions.
        if output.is_empty()
            && !muted
            && let Some(silence) = silence
        {
            output.push(silence);
        }
    }

    pub fn push(
        &mut self,
        mut frame: Frame<f32>,
        action: GateAction,
        config: Config,
        muted: bool,
        output: &mut Vec<Frame<f32>>,
    ) {
        output.clear();
        let changed_capture = self.pending.back().is_some_and(|old| {
            old.position.map(|p| p.capture_id) != frame.position.map(|p| p.capture_id)
        });
        if self.config != Some(config) || changed_capture || muted || config.volume == 0 {
            self.clear();
            self.config = Some(config);
        }
        if muted || config.volume == 0 {
            return;
        }
        if !config.processing.gate || !config.processing.voice_detection {
            self.clear();
            if action != GateAction::Drop {
                output.push(frame);
            }
            return;
        }
        if action == GateAction::Drop {
            if self.pending.len() == LEAD_IN {
                let _ = self.pending.pop_front();
            }
            self.pending.push_back(frame);
            return;
        }
        if action == GateAction::RampIn {
            if let Some(first) = self.pending.front_mut() {
                ramp(&mut first.samples, true);
            } else {
                ramp(&mut frame.samples, true);
            }
            output.extend(self.pending.drain(..));
        } else {
            self.clear();
        }
        output.push(frame);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_queue::CapturePosition;

    #[test]
    fn a_closed_gate_keeps_the_live_clock_without_sending_captured_sound() {
        for detection in [false, true] {
            let mut lead = LeadIn::new();
            let mut output = Vec::new();
            let mut config = config();
            config.processing.voice_detection = detection;
            for hop in 1..100 {
                lead.push_clocked(frame(1, hop), GateAction::Drop, config, false, &mut output);
                if detection && hop <= LEAD_IN as u64 {
                    assert!(output.is_empty());
                    continue;
                }
                assert_eq!(output.len(), 1);
                assert!(output[0].samples.iter().all(|s| *s == 0.0));
                assert_eq!(
                    output[0].position.unwrap().first_sample,
                    (hop - if detection { LEAD_IN as u64 } else { 0 }) * FRAME_SAMPLES as u64
                );
            }
            lead.push_clocked(frame(1, 100), GateAction::Drop, config, true, &mut output);
            assert!(output.is_empty());
            lead.push_clocked(
                frame(1, 101),
                GateAction::RampIn,
                config,
                false,
                &mut output,
            );
            assert_eq!(
                output.len(),
                1,
                "muting must discard retained microphone samples"
            );
            config.volume = 0;
            lead.push_clocked(frame(1, 102), GateAction::Drop, config, false, &mut output);
            assert_eq!(output.len(), 1);
            assert!(output[0].samples.iter().all(|s| *s == 0.0));
        }
    }

    #[test]
    fn loud_non_speech_and_single_confidence_spikes_do_not_open_transmission() {
        let mut gate = VoiceGate::default();
        for _ in 0..100 {
            assert_eq!(gate.step(0.1, 900, 10), GateAction::Drop);
        }
        assert_eq!(gate.step(0.9, 900, 10), GateAction::Drop);
        assert_eq!(gate.step(0.1, 900, 10), GateAction::Drop);
        for _ in 0..100 {
            assert_eq!(gate.step(0.95, 0, 10), GateAction::Drop);
        }
    }

    #[test]
    fn voice_has_separate_open_hold_and_release_conditions() {
        let mut gate = VoiceGate::default();
        assert_eq!(gate.step(0.7, 20, 10), GateAction::Drop);
        assert_eq!(gate.step(0.7, 20, 10), GateAction::RampIn);
        for _ in 0..100 {
            assert_eq!(gate.step(0.4, 6, 10), GateAction::Pass);
        }
        for _ in 1..GATE_HANGOVER_FRAMES {
            assert_eq!(gate.step(0.1, 100, 10), GateAction::Pass);
        }
        assert_eq!(gate.step(0.1, 100, 10), GateAction::RampOut);
        assert_eq!(gate.step(0.1, 100, 10), GateAction::Drop);
        gate.silence();
        assert_eq!(gate.step(0.7, 20, 10), GateAction::Drop);
    }

    fn config() -> Config {
        let mut config = Config::from_state(&AppState::empty());
        config.processing.voice_detection = true;
        config.processing.gate = true;
        config
    }

    fn frame(capture_id: u64, hop: u64) -> Frame<f32> {
        let mut frame = Frame::new([hop as f32 / 10.0; FRAME_SAMPLES]);
        frame.position = Some(CapturePosition {
            capture_id,
            first_sample: hop * FRAME_SAMPLES as u64,
        });
        frame
    }

    #[test]
    fn lead_in_preserves_only_the_last_three_hops_with_their_original_capture_positions() {
        let mut lead = LeadIn::new();
        let mut output = Vec::with_capacity(4);
        for hop in 0..5 {
            lead.push(
                frame(1, hop),
                GateAction::Drop,
                config(),
                false,
                &mut output,
            );
            assert!(output.is_empty());
        }
        let current = frame(1, 5);
        let captured = current.captured;
        lead.push(current, GateAction::RampIn, config(), false, &mut output);
        assert_eq!(output.len(), 4);
        assert_eq!(output[0].samples[0], 0.0);
        assert_eq!(output[1].samples[0], 0.3);
        assert_eq!(output[3].samples[0], 0.5);
        assert_eq!(output[3].captured, captured);
        for (index, packet) in output.iter().enumerate() {
            assert_eq!(
                packet.position.unwrap().first_sample,
                (index as u64 + 2) * FRAME_SAMPLES as u64
            );
        }
    }

    #[test]
    fn mute_zero_gain_config_changes_and_capture_reopens_discard_retained_audio() {
        for reset in 0..4 {
            let mut lead = LeadIn::new();
            let mut output = Vec::with_capacity(4);
            let mut config = config();
            lead.push(frame(1, 1), GateAction::Drop, config, false, &mut output);
            match reset {
                0 => lead.push(frame(1, 2), GateAction::Drop, config, true, &mut output),
                1 => {
                    config.volume = 0;
                    lead.push(frame(1, 2), GateAction::Drop, config, false, &mut output);
                    config.volume = 100;
                }
                2 => {
                    config.threshold += 1;
                }
                _ => {}
            }
            let id = if reset == 3 { 2 } else { 1 };
            lead.push(frame(id, 3), GateAction::RampIn, config, false, &mut output);
            assert_eq!(output.len(), 1, "retained audio survived reset {reset}");
            assert_eq!(
                output[0].position.unwrap().first_sample,
                3 * FRAME_SAMPLES as u64
            );
        }
    }
}
