//! Who a connection counts against (trap 4). One principal must not be able to
//! fill a cap alone, so every held connection is charged to where it came from:
//! the client's address, the proxy's client for proxied traffic, or the relaying
//! peer's iroh key for relayed QUIC, which has no address at all (trap 15).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Origin {
    Ip(IpAddr),
    /// A relayed QUIC peer's endpoint id.
    Relayed(String),
}

impl Origin {
    /// An IPv6 host is handed a /64, so one address in it is not one machine.
    pub fn ip(ip: IpAddr) -> Self {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
            v4 => v4,
        };
        match ip {
            IpAddr::V6(v6) => {
                let mut bits = v6.octets();
                bits[8..].fill(0);
                Origin::Ip(IpAddr::V6(bits.into()))
            }
            v4 => Origin::Ip(v4),
        }
    }
}

/// What the operator says about the transport, and the caps it is held to.
#[derive(Debug, Clone, Default)]
pub struct Admission {
    /// Peers whose `X-Forwarded-For` names the real client
    /// (`DISCORDIA_TRUSTED_PROXIES`). Nobody's, by default.
    pub trusted_proxies: Vec<IpAddr>,
    pub limits: Limits,
}

impl Admission {
    pub fn trusts(&self, peer: IpAddr) -> bool {
        self.trusted_proxies.contains(&peer)
    }

    /// The client behind `peer`: the right-most forwarded hop that is not itself
    /// a trusted proxy, and only when `peer` is one. Anyone else's header is a claim.
    pub fn client_ip(&self, peer: IpAddr, forwarded_for: Option<&str>) -> IpAddr {
        if !self.trusts(peer) {
            return peer;
        }
        forwarded_for
            .into_iter()
            .flat_map(|v| v.rsplit(','))
            .filter_map(|hop| hop.trim().parse::<IpAddr>().ok())
            .find(|hop| !self.trusts(*hop))
            .unwrap_or(peer)
    }
}

/// On a relayed QUIC connection's router, naming the peer for the gateway.
#[derive(Debug, Clone)]
pub struct RelayPeer(pub String);

/// The forwarded address counts only when `peer` is a trusted proxy.
pub fn origin_of(
    admission: &Admission,
    peer: Option<IpAddr>,
    forwarded_for: Option<&str>,
    relay: Option<&RelayPeer>,
) -> Option<Origin> {
    match (peer, relay) {
        (Some(ip), _) => Some(Origin::ip(admission.client_ip(ip, forwarded_for))),
        (None, Some(RelayPeer(id))) => Some(Origin::Relayed(id.clone())),
        (None, None) => None,
    }
}

/// Defaults are production's; tests lower them instead of saturating anything.
#[derive(Debug, Clone)]
pub struct Limits {
    pub sockets_per_origin: usize,
    /// Relayed QUIC peers together: fresh iroh keys are free, so their share is
    /// bounded as a whole as well as per key.
    pub relayed_sockets: usize,
    /// TCP connections that have not finished a request, per address and in all.
    pub pending_per_origin: usize,
    pub pending: usize,
    pub header_read: Duration,
    pub quic_connections_per_origin: usize,
    pub tunnels_per_key: usize,
    pub tunnels_per_origin: usize,
}

impl Default for Limits {
    fn default() -> Self {
        // One address can be a whole office or carrier NAT, so every per-origin
        // share is the socket share, not what one machine needs.
        let per_address = crate::state::MAX_CONNECTIONS_PER_IP as usize;
        Self {
            sockets_per_origin: per_address,
            relayed_sockets: crate::state::MAX_CONNECTIONS / 4,
            pending_per_origin: per_address,
            pending: 1024,
            header_read: Duration::from_secs(10),
            quic_connections_per_origin: per_address,
            // One person's call: the voice room and up to four screen-room
            // identities (trap 10), with room for a reconnect in flight.
            tunnels_per_key: 8,
            tunnels_per_origin: per_address,
        }
    }
}

/// Slots held per key and in all; a slot is given back when its guard drops,
/// so the count follows whatever task holds it.
#[derive(Debug)]
pub struct Budget<K> {
    per_key: usize,
    total: usize,
    held: Mutex<(usize, HashMap<K, usize>)>,
}

#[derive(Debug)]
pub struct Slot<K: std::hash::Hash + Eq> {
    budget: Arc<Budget<K>>,
    key: Option<K>,
}

impl<K: std::hash::Hash + Eq + Clone> Budget<K> {
    pub fn new(per_key: usize, total: usize) -> Arc<Self> {
        Arc::new(Self {
            per_key,
            total,
            held: Mutex::new((0, HashMap::new())),
        })
    }

    /// `None` charges the total only.
    pub fn take(self: &Arc<Self>, key: Option<K>) -> Option<Slot<K>> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if held.0 >= self.total {
            return None;
        }
        if let Some(k) = &key {
            let n = held.1.entry(k.clone()).or_default();
            if *n >= self.per_key {
                return None;
            }
            *n += 1;
        }
        held.0 += 1;
        Some(Slot {
            budget: self.clone(),
            key,
        })
    }
}

impl<K: std::hash::Hash + Eq> Drop for Slot<K> {
    fn drop(&mut self) {
        let mut held = self.budget.held.lock().unwrap_or_else(|e| e.into_inner());
        held.0 = held.0.saturating_sub(1);
        if let Some(k) = &self.key
            && let Some(n) = held.1.get_mut(k)
        {
            *n = n.saturating_sub(1);
            if *n == 0 {
                held.1.remove(k);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forwarded_address_counts_only_from_a_trusted_peer() {
        let proxy: IpAddr = "127.0.0.1".parse().unwrap();
        let admission = Admission {
            trusted_proxies: vec![proxy],
            ..Default::default()
        };
        let client: IpAddr = "198.51.100.4".parse().unwrap();
        let header = Some("203.0.113.9, 198.51.100.4");
        assert_eq!(
            admission.client_ip(proxy, header),
            client,
            "right-most untrusted hop"
        );
        let stranger: IpAddr = "192.0.2.1".parse().unwrap();
        assert_eq!(
            admission.client_ip(stranger, header),
            stranger,
            "nobody else's header"
        );
        assert_eq!(
            Admission::default().client_ip(proxy, header),
            proxy,
            "trusts nothing by default"
        );
        assert_eq!(admission.client_ip(proxy, Some("garbage")), proxy);
    }

    #[test]
    fn an_ipv6_host_is_one_origin_across_its_prefix() {
        let a = Origin::ip("2001:db8::1".parse().unwrap());
        let b = Origin::ip("2001:db8::ffff:2".parse().unwrap());
        assert_eq!(a, b);
        assert_ne!(a, Origin::ip("2001:db8:0:1::1".parse().unwrap()));
        assert_eq!(
            Origin::ip("::ffff:192.0.2.1".parse().unwrap()),
            Origin::ip("192.0.2.1".parse().unwrap())
        );
    }

    #[test]
    fn a_slot_comes_back_when_dropped() {
        let budget = Budget::new(2, 3);
        let a = budget.take(Some("a")).unwrap();
        let _a2 = budget.take(Some("a")).unwrap();
        assert!(budget.take(Some("a")).is_none(), "per key");
        let _b = budget.take(Some("b")).unwrap();
        assert!(budget.take(Some("c")).is_none(), "in all");
        drop(a);
        assert!(budget.take(Some("a")).is_some());
    }
}
