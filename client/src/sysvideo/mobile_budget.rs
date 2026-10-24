use std::time::{Duration, Instant};

use super::Settings;

#[derive(Default)]
pub(crate) struct Pressure {
    pub active_frames: bool,
    pub thermal: u8,
    pub low_memory: bool,
    pub cpu_limited: bool,
    pub processing_ms: Option<f64>,
    pub encode_ms: Option<f64>,
}

pub(crate) struct MobileBudget {
    original: Settings,
    level: u8,
    bad: u8,
    healthy_since: Option<Instant>,
    changed_at: Instant,
}

impl MobileBudget {
    pub fn new(original: Settings, now: Instant) -> Self {
        Self {
            original,
            level: 0,
            bad: 0,
            healthy_since: None,
            changed_at: now,
        }
    }

    pub fn warm_start(original: Settings, now: Instant) -> Self {
        let mut budget = Self::new(original, now);
        budget.level = 2;
        budget
    }

    pub fn current(&self) -> Settings {
        self.settings(self.level)
    }

    fn settings(&self, level: u8) -> Settings {
        let mut settings = self.original;
        let (scale, fps) = match level {
            0 => (1.0, 30),
            1 => (0.75, 30),
            2 => (0.5, 30),
            3 => (0.5, 20),
            _ => (0.5, 15),
        };
        let even = |size: u32| ((size as f64 * scale) as u32 / 2 * 2).max(2);
        settings.width = even(settings.width);
        settings.height = even(settings.height);
        settings.fps = settings.fps.min(fps);
        settings
    }

    pub fn sample(&mut self, now: Instant, pressure: Pressure) -> Option<(u8, Settings)> {
        if now.duration_since(self.changed_at) < Duration::from_secs(10) {
            return None;
        }
        let frame_ms = 1000.0 / self.current().fps.max(1) as f64;
        let expensive = pressure.active_frames
            && [pressure.processing_ms, pressure.encode_ms]
                .into_iter()
                .flatten()
                .any(|ms| ms.is_finite() && ms > frame_ms * 0.9);
        let overloaded =
            pressure.thermal >= 3 || pressure.low_memory || pressure.cpu_limited || expensive;
        if overloaded {
            self.healthy_since = None;
            self.bad = self.bad.saturating_add(1);
        } else if pressure.thermal <= 1 && self.can_recover(&pressure) {
            self.bad = 0;
            self.healthy_since.get_or_insert(now);
        } else {
            self.bad = 0;
            self.healthy_since = None;
        }
        let level = if pressure.low_memory || pressure.thermal >= 4 {
            (self.level + 1).clamp(3, 4)
        } else if self.bad >= 3 {
            (self.level + 1).min(4)
        } else if self
            .healthy_since
            .is_some_and(|since| now.duration_since(since) >= Duration::from_secs(30))
        {
            self.level.saturating_sub(1)
        } else {
            self.level
        };
        (level != self.level).then(|| (level, self.settings(level)))
    }

    fn can_recover(&self, pressure: &Pressure) -> bool {
        let current = self.current();
        let next = self.settings(self.level.saturating_sub(1));
        let pixels = |settings: Settings| f64::from(settings.width) * f64::from(settings.height);
        let growth = pixels(next) / pixels(current);
        let available_ms = 1000.0 / next.fps.max(1) as f64 * 0.8;
        [pressure.processing_ms, pressure.encode_ms]
            .into_iter()
            .flatten()
            .all(|ms| ms.is_finite() && ms >= 0.0 && ms * growth <= available_ms)
    }

    pub fn applied(&mut self, level: u8, now: Instant) {
        self.level = level;
        self.bad = 0;
        self.healthy_since = None;
        self.changed_at = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn budget(now: Instant) -> MobileBudget {
        MobileBudget::new(crate::features::screenshare::native_settings("720"), now)
    }
    #[test]
    fn warm_start_prioritizes_fps_without_exceeding_the_requested_rate() {
        let now = Instant::now();
        let original = crate::features::screenshare::native_settings("720");
        let warm = MobileBudget::warm_start(original, now).current();
        assert_eq!(
            (warm.width, warm.height, warm.max_bitrate),
            (
                original.width / 2,
                original.height / 2,
                original.max_bitrate
            )
        );
        assert_eq!(warm.fps, 30);
        let mut slower = original;
        slower.fps = 15;
        assert_eq!(MobileBudget::warm_start(slower, now).current().fps, 15);
    }

    #[test]
    fn recovery_requires_capacity_for_the_next_budget() {
        let now = Instant::now();
        let mut b =
            MobileBudget::warm_start(crate::features::screenshare::native_settings("720"), now);
        b.applied(3, now);
        for seconds in [3, 15, 33, 60] {
            assert!(
                b.sample(
                    now + Duration::from_secs(seconds),
                    Pressure {
                        active_frames: true,
                        encode_ms: Some(34.0),
                        ..Default::default()
                    }
                )
                .is_none()
            );
        }
        assert!(
            b.sample(
                now + Duration::from_secs(63),
                Pressure {
                    active_frames: true,
                    encode_ms: Some(20.0),
                    ..Default::default()
                }
            )
            .is_none()
        );
        assert_eq!(
            b.sample(
                now + Duration::from_secs(93),
                Pressure {
                    active_frames: true,
                    encode_ms: Some(20.0),
                    ..Default::default()
                }
            )
            .unwrap()
            .1
            .fps,
            30
        );
        b.applied(2, now);
        assert!(!b.can_recover(&Pressure {
            encode_ms: Some(30.0),
            ..Default::default()
        }));
        assert!(b.can_recover(&Pressure {
            encode_ms: Some(10.0),
            ..Default::default()
        }));
    }
    #[test]
    fn sustained_cost_steps_down_but_a_single_spike_and_idle_do_not() {
        let start = Instant::now();
        let mut b = budget(start);
        assert!(
            b.sample(
                start + Duration::from_secs(12),
                Pressure {
                    active_frames: true,
                    encode_ms: Some(45.0),
                    ..Default::default()
                }
            )
            .is_none()
        );
        assert!(
            b.sample(start + Duration::from_secs(15), Pressure::default())
                .is_none()
        );
        for seconds in [18, 21] {
            assert!(
                b.sample(
                    start + Duration::from_secs(seconds),
                    Pressure {
                        active_frames: true,
                        encode_ms: Some(45.0),
                        ..Default::default()
                    }
                )
                .is_none()
            );
        }
        let (level, next) = b
            .sample(
                start + Duration::from_secs(24),
                Pressure {
                    active_frames: true,
                    encode_ms: Some(45.0),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            (level, next.width, next.height, next.fps),
            (1, 960, 540, 30)
        );
        b.applied(level, start + Duration::from_secs(24));
        assert!(
            b.sample(
                start + Duration::from_secs(27),
                Pressure {
                    low_memory: true,
                    ..Default::default()
                }
            )
            .is_none()
        );
    }
    #[test]
    fn memory_pressure_reduces_pixels_and_recovery_is_slow_and_bounded() {
        let start = Instant::now();
        let mut b = budget(start);
        let (level, next) = b
            .sample(
                start + Duration::from_secs(12),
                Pressure {
                    low_memory: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            (level, next.width, next.height, next.fps),
            (3, 640, 360, 20)
        );
        b.applied(level, start + Duration::from_secs(12));
        assert!(
            b.sample(start + Duration::from_secs(15), Pressure::default())
                .is_none()
        );
        assert!(
            b.sample(start + Duration::from_secs(22), Pressure::default())
                .is_none()
        );
        assert!(
            b.sample(start + Duration::from_secs(44), Pressure::default())
                .is_none()
        );
        let (level, next) = b
            .sample(start + Duration::from_secs(52), Pressure::default())
            .unwrap();
        assert_eq!((level, next.fps), (2, 30));
        assert_eq!(b.current().fps, 20);
    }
    #[test]
    fn lower_requested_fps_and_aspect_are_preserved() {
        let now = Instant::now();
        let mut settings = crate::features::screenshare::native_settings("crisp");
        settings.width = 576;
        settings.height = 1280;
        let b = MobileBudget::new(settings, now);
        let limited = b.settings(2);
        assert_eq!((limited.width, limited.height, limited.fps), (288, 640, 15));
        assert_eq!(limited.max_bitrate, settings.max_bitrate);
    }
}
