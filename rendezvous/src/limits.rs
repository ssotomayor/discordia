use std::collections::VecDeque;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// A router served without connect info (the server's tests) has no peer
/// address; everyone then shares this one bucket rather than being refused.
const UNKNOWN_PEER: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

const SWEEP_EVERY: usize = 256;

pub struct Limiter {
    burst: usize,
    window: Duration,
    hits: DashMap<IpAddr, VecDeque<Instant>>,
    calls: AtomicUsize,
}

impl Limiter {
    pub fn new(burst: usize, window: Duration) -> Self {
        Self {
            burst,
            window,
            hits: DashMap::new(),
            calls: AtomicUsize::new(0),
        }
    }

    pub fn per_minute(burst: usize) -> Self {
        Self::new(burst, Duration::from_secs(60))
    }

    pub fn admit(&self, peer: Option<IpAddr>) -> bool {
        self.admit_at(peer, Instant::now())
    }

    /// Refusals are not recorded, so a flood cannot extend its own lockout:
    /// the budget returns one window after the hits that spent it.
    pub fn admit_at(&self, peer: Option<IpAddr>, now: Instant) -> bool {
        if self.calls.fetch_add(1, Ordering::Relaxed) % SWEEP_EVERY == SWEEP_EVERY - 1 {
            self.sweep(now);
        }
        let mut hits = self.hits.entry(peer.unwrap_or(UNKNOWN_PEER)).or_default();
        while hits
            .front()
            .is_some_and(|t| now.duration_since(*t) >= self.window)
        {
            hits.pop_front();
        }
        if hits.len() >= self.burst {
            return false;
        }
        hits.push_back(now);
        true
    }

    fn sweep(&self, now: Instant) {
        self.hits.retain(|_, hits| {
            hits.back()
                .is_some_and(|t| now.duration_since(*t) < self.window)
        });
    }

    pub fn tracked_peers(&self) -> usize {
        self.hits.len()
    }
}

pub struct Limits {
    pub resolve: Limiter,
    pub control: Limiter,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            resolve: Limiter::per_minute(30),
            control: Limiter::per_minute(10),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> Option<IpAddr> {
        Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, last)))
    }

    #[test]
    fn admits_the_burst_refuses_the_next_and_recovers_after_the_window() {
        let limiter = Limiter::new(3, Duration::from_secs(60));
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(limiter.admit_at(ip(1), t0));
        }
        assert!(!limiter.admit_at(ip(1), t0));
        assert!(
            !limiter.admit_at(ip(1), t0 + Duration::from_secs(59)),
            "still inside the window"
        );
        assert!(limiter.admit_at(ip(1), t0 + Duration::from_secs(60)));
    }

    #[test]
    fn peers_do_not_share_a_budget() {
        let limiter = Limiter::new(1, Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(limiter.admit_at(ip(1), t0));
        assert!(!limiter.admit_at(ip(1), t0));
        assert!(limiter.admit_at(ip(2), t0));
    }

    #[test]
    fn unknown_peers_share_one_bucket() {
        let limiter = Limiter::new(1, Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(limiter.admit_at(None, t0));
        assert!(!limiter.admit_at(None, t0));
    }

    #[test]
    fn refusals_do_not_extend_the_lockout() {
        let limiter = Limiter::new(1, Duration::from_secs(10));
        let t0 = Instant::now();
        assert!(limiter.admit_at(ip(1), t0));
        for s in 1..10 {
            assert!(!limiter.admit_at(ip(1), t0 + Duration::from_secs(s)));
        }
        assert!(limiter.admit_at(ip(1), t0 + Duration::from_secs(10)));
    }

    #[test]
    fn idle_peers_are_swept() {
        let limiter = Limiter::new(1, Duration::from_secs(1));
        let t0 = Instant::now();
        for last in 0..=255u8 {
            limiter.admit_at(ip(last), t0);
        }
        assert_eq!(limiter.tracked_peers(), 256);
        let later = t0 + Duration::from_secs(5);
        for _ in 0..SWEEP_EVERY {
            limiter.admit_at(ip(7), later);
        }
        assert_eq!(limiter.tracked_peers(), 1, "only the peer still talking");
    }
}
