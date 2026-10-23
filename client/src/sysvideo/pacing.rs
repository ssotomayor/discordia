use std::time::{Duration, Instant};

pub(crate) struct FramePacer {
    interval: Duration,
    next: Option<Instant>,
}

impl FramePacer {
    pub(crate) fn new(fps: u32) -> Self {
        Self {
            interval: Duration::from_secs_f64(1.0 / fps.max(1) as f64),
            next: None,
        }
    }

    pub(crate) fn accept(&mut self, now: Instant) -> bool {
        let Some(next) = self.next else {
            self.next = Some(now + self.interval / 2);
            return true;
        };
        if now < next {
            return false;
        }
        // A half-frame phase margin preserves near-target sources despite callback jitter.
        self.next = Some(if now.saturating_duration_since(next) >= self.interval {
            now + self.interval / 2
        } else {
            next + self.interval
        });
        true
    }
}
