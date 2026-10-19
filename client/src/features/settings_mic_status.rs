use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Status {
    #[default]
    Idle,
    Listening,
    Good,
    InputHigh,
    OutputHigh,
    Muted,
    Deafened,
    ZeroVolume,
    Unavailable,
}

impl Status {
    pub fn title(self) -> &'static str {
        match self {
            Self::Idle => "Check your voice",
            Self::Listening => "Speak to check your microphone",
            Self::Good => "Good level",
            Self::InputHigh | Self::OutputHigh => "Level too high",
            Self::Muted => "Microphone muted",
            Self::Deafened => "Call audio disabled",
            Self::ZeroVolume => "Microphone volume is zero",
            Self::Unavailable => "Audio device unavailable",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Idle => "Start a local test to check your microphone level.",
            Self::Listening => "Silence is normal. Speak a sentence to see your level.",
            Self::Good => "Your microphone signal has a comfortable level.",
            Self::InputHigh => "Move away from the microphone or lower the device's input gain.",
            Self::OutputHigh => "Lower your microphone volume to leave room for louder words.",
            Self::Muted => "Your voice is muted in the call.",
            Self::Deafened => "You are neither hearing nor transmitting call audio.",
            Self::ZeroVolume => "Raise the microphone volume so your voice can be heard.",
            Self::Unavailable => "Check your audio devices and try again.",
        }
    }

    pub fn action(self) -> Option<&'static str> {
        match self {
            Self::InputHigh => Some("Choose microphone"),
            Self::Unavailable => Some("Check devices"),
            Self::OutputHigh | Self::ZeroVolume => Some("Adjust volume"),
            Self::Muted => Some("Activate microphone"),
            Self::Deafened => Some("Activate audio"),
            _ => None,
        }
    }

    pub fn tone(self) -> &'static str {
        match self {
            Self::Good => "good",
            Self::InputHigh | Self::OutputHigh | Self::ZeroVolume | Self::Unavailable => "warning",
            _ => "neutral",
        }
    }
}

#[derive(Default)]
pub(crate) struct Levels {
    samples: VecDeque<(Instant, u32, u32)>,
    last_voice: Option<Instant>,
}

impl Levels {
    pub fn sample(&mut self, now: Instant, input: u32, output: u32) -> Status {
        self.samples.push_back((now, input, output));
        while self
            .samples
            .front()
            .is_some_and(|s| now.duration_since(s.0) > Duration::from_secs(2))
        {
            self.samples.pop_front();
        }
        if output >= 10 {
            self.last_voice = Some(now);
        }
        if self
            .samples
            .front()
            .is_some_and(|s| now.duration_since(s.0) >= Duration::from_secs(1))
        {
            let high = |raw: bool| {
                self.samples
                    .iter()
                    .filter(|s| if raw { s.1 >= 990 } else { s.2 >= 970 })
                    .count()
                    * 4
                    >= self.samples.len()
            };
            if high(true) {
                return Status::InputHigh;
            }
            if high(false) {
                return Status::OutputHigh;
            }
        }
        if self
            .last_voice
            .is_some_and(|at| now.duration_since(at) < Duration::from_secs(3))
        {
            Status::Good
        } else {
            Status::Listening
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_an_invitation_to_speak_and_a_single_peak_is_not_a_warning() {
        let mut levels = Levels::default();
        let now = Instant::now();
        for tick in 0..40 {
            assert_eq!(
                levels.sample(now + Duration::from_millis(tick * 100), 0, 0),
                Status::Listening
            );
        }
        assert_eq!(
            levels.sample(now + Duration::from_secs(4), 1000, 980),
            Status::Good
        );
        for tick in 41..60 {
            assert_eq!(
                levels.sample(now + Duration::from_millis(tick * 100), 100, 100),
                Status::Good
            );
        }
    }

    #[test]
    fn persistent_high_levels_distinguish_input_and_manual_gain() {
        let now = Instant::now();
        for (input, output, expected) in [
            (1000, 980, Status::InputHigh),
            (500, 980, Status::OutputHigh),
        ] {
            let mut levels = Levels::default();
            for tick in 0..20 {
                levels.sample(now + Duration::from_millis(tick * 100), input, output);
            }
            assert!(levels.sample(now + Duration::from_secs(2), input, output) == expected);
            for tick in 21..61 {
                levels.sample(now + Duration::from_millis(tick * 100), 100, 100);
            }
            assert!(levels.sample(now + Duration::from_secs(7), 100, 100) == Status::Good);
        }
    }
}
