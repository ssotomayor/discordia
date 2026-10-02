use crate::protocol::Id;

#[derive(serde::Deserialize)]
pub(super) struct ScrollSample {
    pub mode: String,
    pub channel: Option<Id>,
    pub sequence: u64,
    pub height: f64,
    pub top: f64,
    pub viewport: f64,
}

#[derive(serde::Serialize)]
pub(super) struct ScrollCommand {
    pub channel: Option<Id>,
    pub sequence: u64,
    pub top: Option<f64>,
}

pub(super) struct ScrollState {
    channel: Option<Id>,
    height: f64,
    following: bool,
}

impl Default for ScrollState {
    fn default() -> Self {
        Self {
            channel: None,
            height: 0.0,
            following: true,
        }
    }
}

impl ScrollState {
    pub fn update(&mut self, sample: ScrollSample) -> ScrollCommand {
        let mut command = ScrollCommand {
            channel: sample.channel,
            sequence: sample.sequence,
            top: None,
        };
        if !sample.height.is_finite()
            || !sample.top.is_finite()
            || !sample.viewport.is_finite()
            || sample.height < 0.0
            || sample.viewport < 0.0
        {
            return command;
        }
        if self.channel != sample.channel || sample.mode == "channel" {
            self.channel = sample.channel;
            self.following = true;
            command.top = Some(sample.height);
        } else {
            match sample.mode.as_str() {
                "scroll" => self.following = sample.height - sample.top - sample.viewport <= 40.0,
                "prepend" => {
                    let growth = sample.height - self.height;
                    if growth > 0.0 {
                        command.top = Some(sample.top + growth);
                    }
                }
                "append" if self.following => command.top = Some(sample.height),
                _ => {}
            }
        }
        if sample.mode != "scroll" {
            self.height = sample.height;
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(mode: &str, height: f64, top: f64) -> ScrollSample {
        ScrollSample {
            mode: mode.into(),
            channel: None,
            sequence: 1,
            height,
            top,
            viewport: 200.0,
        }
    }
    #[test]
    fn history_preserves_position_and_new_messages_respect_manual_scroll() {
        let mut state = ScrollState::default();
        assert_eq!(
            state.update(sample("channel", 1000.0, 0.0)).top,
            Some(1000.0)
        );
        state.update(sample("scroll", 1000.0, 300.0));
        assert_eq!(state.update(sample("append", 1100.0, 300.0)).top, None);
        assert_eq!(
            state.update(sample("prepend", 1500.0, 300.0)).top,
            Some(700.0)
        );
        state.update(sample("scroll", 1500.0, 1280.0));
        assert_eq!(
            state.update(sample("append", 1600.0, 1280.0)).top,
            Some(1600.0)
        );
        assert_eq!(state.update(sample("channel", 100.0, 0.0)).top, Some(100.0));
    }
    #[test]
    fn invalid_measurements_do_not_corrupt_the_anchor() {
        let mut state = ScrollState::default();
        state.update(sample("channel", 1000.0, 0.0));
        assert_eq!(state.update(sample("append", f64::NAN, 0.0)).top, None);
        assert_eq!(
            state.update(sample("prepend", 1200.0, 10.0)).top,
            Some(210.0)
        );
    }
}
