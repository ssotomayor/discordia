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

    pub fn current(&self) -> Settings {
        self.settings(self.level)
    }

    fn settings(&self, level: u8) -> Settings {
        let mut settings = self.original;
        let (scale, fps) = match level {
            0 => (1.0, 30),
            1 => (1.0, 20),
            2 => (0.75, 15),
            _ => (0.5, 15),
        };
        let even = |size: u32| ((size as f64 * scale) as u32 / 2 * 2).max(2);
        settings.width = even(settings.width);
        settings.height = even(settings.height);
        settings.fps = settings.fps.min(fps);
        settings
    }

    pub fn sample(&mut self, now: Instant, pressure: Pressure) -> Option<(u8, Settings)> {
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
        } else if pressure.thermal <= 1 {
            self.bad = 0;
            self.healthy_since.get_or_insert(now);
        } else {
            self.bad = 0;
            self.healthy_since = None;
        }
        if now.duration_since(self.changed_at) < Duration::from_secs(10) {
            return None;
        }
        let level = if pressure.low_memory || pressure.thermal >= 4 {
            (self.level + 1).clamp(2, 3)
        } else if self.bad >= 3 {
            (self.level + 1).min(3)
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
            (1, 1280, 720, 20)
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
            (2, 960, 540, 15)
        );
        b.applied(level, start + Duration::from_secs(12));
        assert!(
            b.sample(start + Duration::from_secs(15), Pressure::default())
                .is_none()
        );
        assert!(
            b.sample(start + Duration::from_secs(44), Pressure::default())
                .is_none()
        );
        let (level, next) = b
            .sample(start + Duration::from_secs(45), Pressure::default())
            .unwrap();
        assert_eq!((level, next.fps), (1, 20));
        assert_eq!(b.current().fps, 15);
    }
    #[test]
    fn lower_requested_fps_and_aspect_are_preserved() {
        let now = Instant::now();
        let mut settings = crate::features::screenshare::native_settings("crisp");
        settings.width = 576;
        settings.height = 1280;
        let b = MobileBudget::new(settings, now);
        let limited = b.settings(2);
        assert_eq!((limited.width, limited.height, limited.fps), (432, 960, 15));
        assert_eq!(limited.max_bitrate, settings.max_bitrate);
    }
}
