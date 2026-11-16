use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct UplinkProtection {
    last_feedback: Option<Instant>,
    last_bad: Option<Instant>,
    good_since: Option<Instant>,
    constrained: bool,
}

impl UplinkProtection {
    pub fn sample(&mut self, now: Instant, loss: Option<f32>) -> bool {
        if self
            .last_feedback
            .is_some_and(|at| now.duration_since(at) >= Duration::from_secs(7))
        {
            *self = Self::default();
        }
        if let Some(loss) = loss.filter(|v| v.is_finite() && (0.0..=100.0).contains(v)) {
            self.last_feedback = Some(now);
            if loss >= 5.0 {
                self.constrained |= loss >= 20.0
                    || self.last_bad.is_some_and(|at| {
                        now.duration_since(at) >= Duration::from_secs(1)
                            && now.duration_since(at) < Duration::from_secs(7)
                    });
                self.last_bad = Some(now);
                self.good_since = None;
            } else if loss < 3.0 {
                self.last_bad = None;
                let since = *self.good_since.get_or_insert(now);
                if now.duration_since(since) >= Duration::from_secs(2) {
                    self.constrained = false;
                }
            } else {
                self.last_bad = None;
                self.good_since = None;
            }
        }
        self.constrained
    }
}

#[cfg(any(
    target_os = "android",
    target_os = "macos",
    target_os = "windows",
    test
))]
#[derive(Clone)]
pub(super) struct VideoBudget {
    pub limit: Option<u64>,
    floor: u64,
    ceiling: u64,
    baseline: u64,
    changed_at: Option<Instant>,
    constrained: bool,
}

#[cfg(any(
    target_os = "android",
    target_os = "macos",
    target_os = "windows",
    test
))]
impl VideoBudget {
    pub fn new(floor: u64, ceiling: u64) -> Self {
        let ceiling = ceiling.max(1);
        Self {
            limit: None,
            floor: floor.clamp(1, ceiling),
            ceiling,
            baseline: ceiling,
            changed_at: None,
            constrained: false,
        }
    }

    pub fn needs_allocation(&self, now: Instant, constrained: bool) -> bool {
        constrained
            && (self.limit.is_none()
                || self
                    .changed_at
                    .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(3)))
    }

    pub fn step(&mut self, now: Instant, constrained: bool, allocation: Option<u64>) {
        let allocation = allocation.filter(|v| *v > 0).map(|v| v.min(self.ceiling));
        let previous = self.limit;
        if constrained {
            if self.limit.is_none() {
                self.baseline = allocation.unwrap_or(self.ceiling);
            }
            if self.needs_allocation(now, true) {
                let current = self.limit.unwrap_or(self.baseline);
                let current = allocation.unwrap_or(current).min(current);
                self.limit = Some(
                    (current.saturating_mul(3) / 4)
                        .max(self.floor.min(current))
                        .min(self.ceiling),
                );
            }
        } else if let Some(current) = self.limit
            && (self.constrained
                || self
                    .changed_at
                    .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(2)))
        {
            // A ceiling permits probing; it must never force the congestion controller's bitrate.
            let next = current.saturating_add((current / 2).max(1));
            self.limit = (next < self.baseline).then_some(next);
        }
        if self.limit != previous {
            self.changed_at = Some(now);
        }
        self.constrained = constrained;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolated_loss_does_not_destroy_video_quality() {
        let now = Instant::now();
        let mut guard = UplinkProtection::default();
        assert!(!guard.sample(now, Some(7.03125)));
        assert!(!guard.sample(now + Duration::from_secs(1), Some(1.953125)));
        assert!(!guard.sample(now + Duration::from_secs(2), Some(7.0)));
        assert!(!guard.sample(now + Duration::from_secs(3), None));
        assert!(!guard.sample(now + Duration::from_secs(10), Some(7.0)));
    }

    #[test]
    fn sustained_or_severe_loss_protects_voice_and_fresh_recovery_releases_it() {
        let now = Instant::now();
        let mut guard = UplinkProtection::default();
        assert!(!guard.sample(now, Some(7.0)));
        assert!(guard.sample(now + Duration::from_secs(1), Some(8.0)));
        assert!(guard.sample(now + Duration::from_secs(2), Some(1.0)));
        assert!(guard.sample(now + Duration::from_secs(3), None));
        assert!(!guard.sample(now + Duration::from_secs(4), Some(1.0)));
        assert!(guard.sample(now + Duration::from_secs(5), Some(25.0)));
        assert!(!guard.sample(now + Duration::from_secs(12), None));
    }

    #[test]
    fn invalid_loss_and_middle_band_do_not_confirm_recovery() {
        let now = Instant::now();
        let mut guard = UplinkProtection::default();
        assert!(guard.sample(now, Some(25.0)));
        assert!(guard.sample(now + Duration::from_secs(1), Some(0.0)));
        assert!(guard.sample(now + Duration::from_secs(2), Some(4.0)));
        for (second, loss) in [(3, f32::NAN), (4, -1.0), (5, 101.0)] {
            assert!(guard.sample(now + Duration::from_secs(second), Some(loss)));
        }
        assert!(guard.sample(now + Duration::from_secs(6), Some(0.0)));
        assert!(!guard.sample(now + Duration::from_secs(8), Some(0.0)));
    }

    #[test]
    fn high_resolution_budget_reduces_and_recovers_without_a_fixed_low_cap() {
        let now = Instant::now();
        let mut budget = VideoBudget::new(300_000, 34_000_000);
        budget.step(now, true, Some(34_000_000));
        assert_eq!(budget.limit, Some(25_500_000));
        budget.step(now + Duration::from_secs(1), true, Some(20_000_000));
        assert_eq!(budget.limit, Some(25_500_000));
        budget.step(now + Duration::from_secs(3), true, Some(20_000_000));
        assert_eq!(budget.limit, Some(15_000_000));
        budget.step(now + Duration::from_secs(4), false, None);
        assert_eq!(budget.limit, Some(22_500_000));
        budget.step(now + Duration::from_secs(6), false, None);
        assert_eq!(budget.limit, Some(33_750_000));
        budget.step(now + Duration::from_secs(8), false, None);
        assert_eq!(budget.limit, None);
    }

    #[test]
    fn persistent_loss_remains_bounded_and_failed_applications_can_retry() {
        let now = Instant::now();
        let mut budget = VideoBudget::new(300_000, 34_000_000);
        let mut failed = budget.clone();
        failed.step(now, true, Some(4_000_000));
        assert_eq!(budget.limit, None);
        for second in (0..120).step_by(3) {
            budget.step(now + Duration::from_secs(second), true, Some(4_000_000));
        }
        assert_eq!(budget.limit, Some(300_000));
        for second in (120..160).step_by(2) {
            budget.step(now + Duration::from_secs(second), false, None);
        }
        assert_eq!(budget.limit, None);
        budget.step(now + Duration::from_secs(161), true, Some(100_000));
        assert_eq!(budget.limit, Some(100_000));
    }
}
