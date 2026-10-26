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

/// One frame out per frame in. The encoder's RTP clock advances only with
/// what is pushed, so a shut gate sends zeros rather than nothing (trap 38).
pub(super) struct Outgoing {
    pub frame: Frame<f32>,
    pub silent: bool,
    pub lead_in: bool,
}

struct Held {
    frame: Frame<f32>,
    voiced: bool,
    lead_in: bool,
}

/// With voice detection the stream runs `LEAD_IN` frames behind capture, so
/// the frames before an opening go out as audio instead of the zeros they
/// would otherwise have been. Positions and stamps stay the capture's.
pub(super) struct LeadIn {
    pending: VecDeque<Held>,
    config: Option<Config>,
}

impl LeadIn {
    pub fn new() -> Self {
        Self {
            pending: VecDeque::with_capacity(LEAD_IN + 1),
            config: None,
        }
    }

    pub fn clear(&mut self) {
        self.pending.clear();
    }

    pub fn push(
        &mut self,
        mut frame: Frame<f32>,
        action: GateAction,
        config: Config,
        muted: bool,
        output: &mut Vec<Outgoing>,
    ) {
        output.clear();
        let changed_capture = self.pending.back().is_some_and(|old| {
            old.frame.position.map(|p| p.capture_id) != frame.position.map(|p| p.capture_id)
        });
        if self.config != Some(config) || changed_capture || muted || config.volume == 0 {
            self.clear();
            self.config = Some(config);
        }
        let silent = muted || config.volume == 0 || action == GateAction::Drop;
        if muted
            || config.volume == 0
            || !config.processing.gate
            || !config.processing.voice_detection
        {
            if silent {
                frame.samples = [0.0; FRAME_SAMPLES];
            }
            output.push(Outgoing {
                frame,
                silent,
                lead_in: false,
            });
            return;
        }
        if action == GateAction::RampIn {
            let mut first = true;
            for held in &mut self.pending {
                if !held.voiced {
                    held.voiced = true;
                    held.lead_in = true;
                    if first {
                        ramp(&mut held.frame.samples, true);
                    }
                }
                first = false;
            }
            if first {
                ramp(&mut frame.samples, true);
            }
        }
        self.pending.push_back(Held {
            frame,
            voiced: action != GateAction::Drop,
            lead_in: false,
        });
        if self.pending.len() > LEAD_IN {
            let mut held = self.pending.pop_front().expect("just pushed");
            if !held.voiced {
                held.frame.samples = [0.0; FRAME_SAMPLES];
            }
            output.push(Outgoing {
                frame: held.frame,
                silent: !held.voiced,
                lead_in: held.lead_in,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_queue::CapturePosition;

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

    fn drain(
        lead: &mut LeadIn,
        hops: impl IntoIterator<Item = (u64, u64, GateAction)>,
        config: Config,
    ) -> Vec<Outgoing> {
        let mut out = Vec::new();
        let mut step = Vec::with_capacity(1);
        for (id, hop, action) in hops {
            lead.push(frame(id, hop), action, config, false, &mut step);
            out.append(&mut step);
        }
        out
    }

    #[test]
    fn lead_in_releases_the_last_three_hops_as_audio_and_every_hop_before_as_silence() {
        let mut lead = LeadIn::new();
        let hops = (0..5).map(|hop| (1, hop, GateAction::Drop));
        let out = drain(&mut lead, hops, config());
        assert_eq!(out.len(), 2, "a 30 ms delay line");
        assert!(out.iter().all(|o| o.silent && o.frame.samples[0] == 0.0));
        let out = drain(&mut lead, [(1, 5, GateAction::RampIn)], config());
        assert_eq!(out.len(), 1);
        assert!(out[0].lead_in && !out[0].silent);
        assert_eq!(
            out[0].frame.samples[0], 0.0,
            "the first released hop ramps in"
        );
        assert_eq!(
            out[0].frame.position.unwrap().first_sample,
            2 * FRAME_SAMPLES as u64
        );
        let out = drain(
            &mut lead,
            (6..9).map(|hop| (1, hop, GateAction::Pass)),
            config(),
        );
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].frame.samples[0], 0.3);
        assert!(out[0].lead_in && out[1].lead_in && !out[2].lead_in);
        assert_eq!(out[2].frame.samples[0], 0.5);
        for (index, packet) in out.iter().enumerate() {
            assert_eq!(
                packet.frame.position.unwrap().first_sample,
                (index as u64 + 3) * FRAME_SAMPLES as u64
            );
        }
    }

    #[test]
    fn every_hop_goes_out_once_in_capture_order_whatever_the_gate_does() {
        let mut lead = LeadIn::new();
        let mut rng = 7u32;
        let hops = (0..400).map(|hop| {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            let action = match rng % 4 {
                0 => GateAction::Drop,
                1 => GateAction::RampIn,
                2 => GateAction::Pass,
                _ => GateAction::RampOut,
            };
            (1, hop, action)
        });
        let out = drain(&mut lead, hops, config());
        assert_eq!(out.len(), 400 - LEAD_IN);
        for (index, packet) in out.iter().enumerate() {
            assert_eq!(
                packet.frame.position.unwrap().first_sample,
                index as u64 * FRAME_SAMPLES as u64
            );
            assert_eq!(
                packet.silent,
                packet.frame.samples.iter().all(|s| *s == 0.0) && index > 0
            );
        }
        let mut plain = config();
        plain.processing.voice_detection = false;
        let mut lead = LeadIn::new();
        let out = drain(
            &mut lead,
            (0..10).map(|hop| (1, hop, GateAction::Drop)),
            plain,
        );
        assert_eq!(out.len(), 10, "without voice detection nothing is delayed");
        assert!(out.iter().all(|o| o.silent));
    }

    #[test]
    fn mute_zero_gain_config_changes_and_capture_reopens_discard_retained_audio() {
        for reset in 0..4 {
            let mut lead = LeadIn::new();
            let mut output = Vec::with_capacity(4);
            let mut config = config();
            lead.push(frame(1, 1), GateAction::Drop, config, false, &mut output);
            assert!(output.is_empty());
            match reset {
                0 => {
                    lead.push(frame(1, 2), GateAction::Drop, config, true, &mut output);
                    assert!(
                        output.len() == 1 && output[0].silent,
                        "muted hops still go out"
                    );
                }
                1 => {
                    config.volume = 0;
                    lead.push(frame(1, 2), GateAction::Drop, config, false, &mut output);
                    assert!(output.len() == 1 && output[0].silent);
                    config.volume = 100;
                }
                2 => {
                    config.threshold += 1;
                }
                _ => {}
            }
            let id = if reset == 3 { 2 } else { 1 };
            let mut out = Vec::new();
            for hop in 3..7 {
                lead.push(
                    frame(id, hop),
                    GateAction::RampIn,
                    config,
                    false,
                    &mut output,
                );
                out.append(&mut output);
            }
            assert_eq!(out.len(), 1, "retained audio survived reset {reset}");
            assert_eq!(
                out[0].frame.position.unwrap().first_sample,
                3 * FRAME_SAMPLES as u64
            );
            assert!(!out[0].silent && !out[0].lead_in);
        }
    }
}
