//! A copy of the rendezvous's `limits::Limiter`, keyed by anything rather than
//! only an address: the relay also limits per handle and per key.

use std::collections::VecDeque;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

const SWEEP_EVERY: usize = 256;
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

pub struct Limiter<K: Eq + Hash = IpAddr> {
    burst: usize,
    window: Duration,
    hits: DashMap<K, VecDeque<Instant>>,
    calls: AtomicUsize,
}

impl<K: Eq + Hash + Clone> Limiter<K> {
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

    pub fn per_day(burst: usize) -> Self {
        Self::new(burst, DAY)
    }

    pub fn admit(&self, key: &K) -> bool {
        self.admit_at(key, Instant::now())
    }

    /// Refusals are not recorded, so a flood cannot extend its own lockout:
    /// the budget returns one window after the hits that spent it.
    pub fn admit_at(&self, key: &K, now: Instant) -> bool {
        if self.calls.fetch_add(1, Ordering::Relaxed) % SWEEP_EVERY == SWEEP_EVERY - 1 {
            self.sweep(now);
        }
        let mut hits = self.hits.entry(key.clone()).or_default();
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

    /// Whether `admit` would pass, spending nothing: so one of two budgets is
    /// not charged for a call the other refuses.
    pub fn has_room(&self, key: &K, now: Instant) -> bool {
        self.hits.get(key).is_none_or(|hits| {
            hits.iter()
                .filter(|t| now.duration_since(**t) < self.window)
                .count()
                < self.burst
        })
    }

    fn sweep(&self, now: Instant) {
        self.hits.retain(|_, hits| {
            hits.back()
                .is_some_and(|t| now.duration_since(*t) < self.window)
        });
    }

    pub fn tracked(&self) -> usize {
        self.hits.len()
    }
}

/// An IPv6 host is handed a /64, so one address in it is not one requester.
pub fn bucket(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let mut bits = v6.octets();
                bits[8..].fill(0);
                IpAddr::V6(bits.into())
            }
        },
        v4 => v4,
    }
}

/// The client behind `peer`: the right-most forwarded hop that is not itself a
/// trusted proxy, and only when `peer` is one. Anyone else's header is a claim.
pub fn client_ip(trusted: &[IpAddr], peer: IpAddr, forwarded_for: Option<&str>) -> IpAddr {
    if !trusted.contains(&peer) {
        return peer;
    }
    forwarded_for
        .into_iter()
        .flat_map(|v| v.rsplit(','))
        .filter_map(|hop| hop.trim().parse::<IpAddr>().ok())
        .find(|hop| !trusted.contains(hop))
        .unwrap_or(peer)
}

/// Defaults are production's; tests lower them instead of saturating anything.
pub struct Limits {
    /// Signed routes and `/`, per address.
    pub api: Limiter,
    /// `/v1/push`, per address. A guild server pushes for all its members
    /// from one address, so this is the wide one.
    pub push: Limiter,
    /// New device rows per address: the one route a stranger grows state through.
    pub registrations: Limiter,
    pub handle_minute: Limiter<String>,
    pub handle_day: Limiter<String>,
    /// DM pushes per recipient key.
    pub dm_per_key: Limiter<String>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            api: Limiter::per_minute(60),
            push: Limiter::per_minute(1200),
            registrations: Limiter::per_day(32),
            handle_minute: Limiter::per_minute(20),
            handle_day: Limiter::per_day(300),
            dm_per_key: Limiter::per_minute(30),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, last))
    }

    #[test]
    fn admits_the_burst_refuses_the_next_and_recovers_after_the_window() {
        let limiter = Limiter::new(3, Duration::from_secs(60));
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(limiter.admit_at(&ip(1), t0));
        }
        assert!(!limiter.admit_at(&ip(1), t0));
        assert!(!limiter.admit_at(&ip(1), t0 + Duration::from_secs(59)));
        assert!(limiter.admit_at(&ip(1), t0 + Duration::from_secs(60)));
    }

    #[test]
    fn keys_do_not_share_a_budget_and_room_spends_nothing() {
        let limiter = Limiter::<String>::new(1, Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(limiter.has_room(&"a".into(), t0));
        assert!(limiter.has_room(&"a".into(), t0));
        assert!(limiter.admit_at(&"a".into(), t0));
        assert!(!limiter.has_room(&"a".into(), t0));
        assert!(limiter.admit_at(&"b".into(), t0));
    }

    #[test]
    fn idle_keys_are_swept() {
        let limiter = Limiter::new(1, Duration::from_secs(1));
        let t0 = Instant::now();
        for last in 0..=255u8 {
            limiter.admit_at(&ip(last), t0);
        }
        assert_eq!(limiter.tracked(), 256);
        let later = t0 + Duration::from_secs(5);
        for _ in 0..SWEEP_EVERY {
            limiter.admit_at(&ip(7), later);
        }
        assert_eq!(limiter.tracked(), 1);
    }

    #[test]
    fn an_ipv6_host_is_its_slash_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
        assert_eq!(bucket(a), bucket(b));
        assert_eq!(
            bucket(a),
            IpAddr::V6("2001:db8:1:2::".parse::<Ipv6Addr>().unwrap())
        );
        let mapped: IpAddr = "::ffff:203.0.113.5".parse().unwrap();
        assert_eq!(bucket(mapped), ip(5));
    }

    #[test]
    fn forwarded_for_counts_only_from_a_trusted_proxy() {
        let proxy: IpAddr = "127.0.0.1".parse().unwrap();
        let header = Some("198.51.100.7, 203.0.113.9");
        assert_eq!(client_ip(&[], proxy, header), proxy);
        assert_eq!(client_ip(&[proxy], ip(1), header), ip(1));
        assert_eq!(client_ip(&[proxy], proxy, header), ip(9));
        assert_eq!(client_ip(&[proxy], proxy, Some("junk")), proxy);
    }
}
