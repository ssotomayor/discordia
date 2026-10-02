use crate::protocol::Id;

#[derive(serde::Deserialize)]
pub(super) struct ScrollSample {
    pub mode: String,
    pub channel: Option<Id>,
    pub sequence: u64,
    pub height: f64,
    pub top: f64,
    pub viewport: f64,
    #[serde(default)]
    pub pages: Vec<PageSample>,
    #[serde(default)]
    pub anchor_shift: Option<f64>,
}

#[derive(serde::Deserialize)]
pub(super) struct PageSample {
    pub id: Id,
    pub top: f64,
    pub height: f64,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct PageState {
    pub active: bool,
    pub height: f64,
}

pub(super) fn page_states(
    pages: &[PageSample],
    viewport: f64,
) -> std::collections::HashMap<Id, PageState> {
    if !viewport.is_finite() || viewport < 0.0 {
        return Default::default();
    }
    pages
        .iter()
        .filter(|p| p.top.is_finite() && p.height.is_finite() && p.height >= 0.0)
        .map(|p| {
            (
                p.id,
                PageState {
                    active: p.top <= viewport + 1200.0 && p.top + p.height >= -1200.0,
                    height: p.height,
                },
            )
        })
        .collect()
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
                    let growth = sample
                        .anchor_shift
                        .filter(|x| x.is_finite())
                        .unwrap_or(sample.height - self.height);
                    if growth > 0.0 || (sample.anchor_shift.is_some() && growth.abs() > 0.1) {
                        command.top = Some((sample.top + growth).max(0.0));
                    }
                }
                "append" | "resize" if self.following => command.top = Some(sample.height),
                "resize" => {
                    if let Some(shift) = sample
                        .anchor_shift
                        .filter(|x| x.is_finite() && x.abs() > 0.1)
                    {
                        command.top = Some((sample.top + shift).max(0.0));
                    }
                }
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
            pages: Vec::new(),
            anchor_shift: None,
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

    #[test]
    fn thousands_of_history_pages_only_mount_the_viewport_and_neighbors() {
        let pages: Vec<_> = (0..1000)
            .map(|i| PageSample {
                id: Id::new_v4(),
                top: (i as f64 - 500.0) * 4000.0,
                height: 4000.0,
            })
            .collect();
        let states = page_states(&pages, 800.0);
        assert_eq!(states.len(), 1000);
        assert_eq!(states.values().filter(|p| p.active).count(), 2);
        assert!(states[&pages[500].id].active);
        assert!(!states[&pages[0].id].active);
        assert!(page_states(&pages, f64::NAN).is_empty());
    }

    #[test]
    fn virtual_pages_and_images_preserve_the_reading_anchor() {
        let mut state = ScrollState::default();
        state.update(sample("channel", 5000.0, 0.0));
        state.update(sample("scroll", 5000.0, 1000.0));
        let mut resized = sample("resize", 6000.0, 1000.0);
        resized.anchor_shift = Some(120.0);
        assert_eq!(state.update(resized).top, Some(1120.0));
        let mut prepend = sample("prepend", 9000.0, 1120.0);
        prepend.anchor_shift = Some(2400.0);
        assert_eq!(state.update(prepend).top, Some(3520.0));
        let mut resized = sample("resize", 8000.0, 3520.0);
        resized.anchor_shift = Some(-1000.0);
        assert_eq!(state.update(resized).top, Some(2520.0));
    }
}
