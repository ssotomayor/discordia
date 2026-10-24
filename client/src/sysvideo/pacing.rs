use std::time::{Duration, Instant};

pub(crate) struct FramePacer {
    interval: Duration,
    next: Option<Instant>,
}

impl FramePacer {
    #[cfg(any(target_os = "android", test))]
    pub(crate) fn set_fps(&mut self, fps: u32) {
        *self = Self::new(fps);
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pacing_limits_fast_callbacks_without_losing_near_target_frames() {
        let start = Instant::now();
        let mut pacer = FramePacer::new(30);
        let accepted = (0..600)
            .filter(|n| pacer.accept(start + Duration::from_secs_f64(*n as f64 / 60.0)))
            .count();
        assert!((299..=301).contains(&accepted), "{accepted}");
        pacer.set_fps(15);
        let accepted = (0..600)
            .filter(|n| {
                pacer.accept(
                    start + Duration::from_secs(10) + Duration::from_secs_f64(*n as f64 / 60.0),
                )
            })
            .count();
        assert!((149..=151).contains(&accepted), "{accepted}");
        let mut slow = FramePacer::new(30);
        assert!((0..10).all(|n| slow.accept(start + Duration::from_secs(n))));
    }
}
