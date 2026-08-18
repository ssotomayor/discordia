//! Asking the router for a way in.
//!
//! A self-hosted machine at home has no address the internet can dial, which is
//! why every remote friend is relayed today. A port mapping is the one answer to
//! that involving nobody else at all: the router already stands between us and
//! the internet, and both UPnP-IGD and NAT-PMP exist to let a host on the inside
//! ask it for a forward. See `docs/NETWORKING.md` — this is tier 1, and the only
//! tier with no third party in it.
//!
//! **Failure is the normal case, not an error.** UPnP is off by default on some
//! routers, and behind carrier-grade NAT there is no public address to hand out
//! however willing the router is. Every entry point here returns a reason
//! instead of an address, and the caller keeps hosting either way — the host is
//! simply relay-only, which is what it already was.
//!
//! Two protocols because routers implement one or the other and rarely both:
//! UPnP-IGD is the common one, NAT-PMP is what Apple's base stations and a few
//! others speak. IGD is tried first only because it is far more widespread.
//!
//! On macOS both discovery paths are local-network traffic (SSDP multicast to
//! 239.255.255.250, and a UDP request to the default gateway), so they fall
//! under the same per-app grant `NSLocalNetworkUsageDescription` covers. That
//! key is already declared in `client/Info.plist`; this is the first code that
//! actually exercises it.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use igd_next::PortMappingProtocol;

/// How long a mapping is asked for, and — at half of it — how often it is
/// renewed.
///
/// Deliberately not "infinite" (`0` for IGD): a lease that outlives the process
/// leaves a hole in someone's router pointing at a port nothing is listening on
/// any more, and plenty of routers refuse infinite leases outright. An hour is
/// long enough that renewal is rare and short enough that a crash cleans itself
/// up.
const LEASE: Duration = Duration::from_secs(3600);

/// Discovery has to finish while someone is watching a "starting…" spinner, and
/// a router that is not going to answer never says so. Both protocols are given
/// the same short window.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

/// How long to wait for the router to loop a connection to our own public
/// address back to us. A router that does this at all does it immediately.
const HAIRPIN_TIMEOUT: Duration = Duration::from_secs(3);

/// The ports a self-hosting machine needs open, and what each one carries.
#[derive(Debug, Clone, Copy)]
pub struct Ports {
    /// The gateway WebSocket — chat, presence, everything in `protocol`.
    pub gateway_tcp: u16,
    /// LiveKit's signalling WebSocket.
    pub media_tcp: u16,
    /// LiveKit's ICE/TCP fallback, for peers whose network blocks UDP.
    pub media_tcp_ice: u16,
    /// LiveKit's single-port UDP mux — the actual audio and video.
    pub media_udp: u16,
    /// The QUIC transport's UDP port, when one is bound.
    ///
    /// Mapped here rather than left to iroh's own port mapper so that one
    /// module owns every hole we ask for, and so a renumbering is noticed the
    /// same way the media ports' is — an advertised address whose port the
    /// router quietly changed is unreachable in exactly the way that looks like
    /// the host being offline.
    pub quic_udp: Option<u16>,
}

/// What the router agreed to.
#[derive(Debug, Clone)]
pub struct Mapped {
    /// Which protocol got us here, for the UI to name.
    pub method: &'static str,
    /// Our address as the internet sees it.
    pub public_ip: IpAddr,
    /// External port for the gateway. Usually the same as the internal one; a
    /// router that had it taken may hand back a different one, which is fine
    /// here because we advertise this endpoint explicitly.
    pub gateway_port: u16,
    /// Whether LiveKit's three ports were mapped **at the same port numbers**.
    ///
    /// Media is the one part that cannot absorb a renumbering: LiveKit puts its
    /// own port into the ICE candidates it advertises, and we hand out its URL
    /// by substituting a host into `ws://{host}:{port}`. A router that grants
    /// `7882 → 41234` produces a config that looks fine and drops every packet,
    /// so a renumbered media port is reported as "not mapped" instead.
    pub media: bool,
    /// Whether the QUIC transport's UDP port was mapped at its own number, so
    /// the key-authenticated path is reachable from outside and not only on
    /// this network.
    pub quic: bool,
    /// Whether this machine can reach its own public address — "hairpin NAT".
    ///
    /// Not a curiosity: LiveKit *replaces* its host ICE candidate with the
    /// advertised address rather than adding to it, so advertising one costs
    /// the LAN path unless the router loops traffic back. Measured rather than
    /// assumed, because it decides whether we advertise at all.
    pub hairpin: bool,
}

impl Mapped {
    /// The gateway URL to advertise to a rendezvous.
    pub fn endpoint(&self) -> String {
        match self.public_ip {
            IpAddr::V6(ip) => format!("ws://[{ip}]:{}", self.gateway_port),
            IpAddr::V4(ip) => format!("ws://{ip}:{}", self.gateway_port),
        }
    }
}

/// A live set of mappings. Dropping it asks the router to take them away.
pub struct MappingGuard {
    /// Dropping the sender is the shutdown signal; the task cannot outlive us.
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

/// Ask the router to forward `ports` to `local_ip`, and report what it did.
///
/// Returns the mapping plus a guard that renews it and removes it on drop. The
/// error side is a sentence fit to show a person, because that is where it goes:
/// a host that cannot be reached has to be told why (`docs/NETWORKING.md`).
pub async fn request(local_ip: Ipv4Addr, ports: Ports) -> Result<(Mapped, MappingGuard), String> {
    let igd_err = match igd_router(local_ip).await {
        Ok(router) => return finish(router, local_ip, ports).await,
        Err(e) => e,
    };
    let natpmp_err = match natpmp_router().await {
        Ok(router) => return finish(router, local_ip, ports).await,
        Err(e) => e,
    };
    Err(format!(
        "no router accepted a port mapping — UPnP-IGD: {igd_err}; NAT-PMP: {natpmp_err}. \
         Either both are disabled on your router, or your ISP puts you behind \
         carrier-grade NAT, where no public address exists to map."
    ))
}

/// Add every port, then keep them alive until the guard is dropped.
async fn finish(
    mut router: Router,
    local_ip: Ipv4Addr,
    ports: Ports,
) -> Result<(Mapped, MappingGuard), String> {
    let public_ip = router.public_ip().await?;
    if is_private(public_ip) {
        // A private router address implies double-NAT/CGNAT; mapping ports
        // would only be reachable from the ISP's network, which is worse than
        // failing fast.
        return Err(format!(
            "your router's own address ({public_ip}) is private, so it is behind \
             another NAT — usually carrier-grade NAT at your ISP. Nothing this \
             machine can do opens a path in; you need the relay, or a provider \
             that gives you a public address."
        ));
    }

    let gateway_port = router
        .add(PortMappingProtocol::TCP, local_ip, ports.gateway_tcp)
        .await
        .map_err(|e| format!("the router refused to forward the gateway port: {e}"))?;

    let quic_ok = match ports.quic_udp {
        Some(port) => matches!(
            router.add(PortMappingProtocol::UDP, local_ip, port).await,
            Ok(external) if external == port
        ),
        None => false,
    };

    let media = {
        let mut all_ok = true;
        for (proto, port) in [
            (PortMappingProtocol::TCP, ports.media_tcp),
            (PortMappingProtocol::TCP, ports.media_tcp_ice),
            (PortMappingProtocol::UDP, ports.media_udp),
        ] {
            match router.add(proto, local_ip, port).await {
                Ok(external) if external == port => {}
                Ok(external) => {
                    tracing::warn!(
                        %proto, wanted = port, granted = external,
                        "router renumbered a media port — voice cannot use it"
                    );
                    all_ok = false;
                }
                Err(e) => {
                    tracing::warn!(%proto, port, error = %e, "media port not mapped");
                    all_ok = false;
                }
            }
        }
        all_ok
    };

    let hairpin = probe_hairpin(SocketAddr::new(public_ip, gateway_port)).await;

    let mapped = Mapped {
        method: router.method(),
        public_ip,
        gateway_port,
        media,
        quic: quic_ok,
        hairpin,
    };
    Ok((mapped, keep_alive(router, local_ip, ports, gateway_port)))
}

/// Renew the lease at half its lifetime, and give the ports back on shutdown.
///
/// Renewal is not optional bookkeeping: `LEASE` is an hour, and a session
/// outliving it would keep advertising an endpoint the router has since
/// forgotten — the failure that looks exactly like the host being offline.
fn keep_alive(
    router: Router,
    local_ip: Ipv4Addr,
    ports: Ports,
    gateway_external: u16,
) -> MappingGuard {
    // Both numbers, per port: the two protocols identify a mapping differently
    // — IGD deletes by the *external* port, NAT-PMP by the *internal* one (its
    // delete is a zero-lifetime request naming the private port). They are the
    // same number for everything but the gateway, which is exactly why keeping
    // only one of them looked fine.
    let all = [
        (
            PortMappingProtocol::TCP,
            ports.gateway_tcp,
            gateway_external,
        ),
        (PortMappingProtocol::TCP, ports.media_tcp, ports.media_tcp),
        (
            PortMappingProtocol::TCP,
            ports.media_tcp_ice,
            ports.media_tcp_ice,
        ),
        (PortMappingProtocol::UDP, ports.media_udp, ports.media_udp),
    ];
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut renew = tokio::time::interval(LEASE / 2);
        renew.tick().await; // fires immediately; the mapping was just made
        tokio::pin!(rx);
        loop {
            tokio::select! {
                _ = renew.tick() => {
                    for &(proto, internal, external) in &all {
                        match router.add(proto, local_ip, internal).await {
                            // A renewal on a different external port means the
                            // advertised endpoint is stale.
                            Ok(granted) if granted != external => tracing::warn!(
                                %proto, internal, was = external, now = granted,
                                "renewal moved the mapping — the advertised endpoint is stale"
                            ),
                            Ok(_) => {}
                            Err(e) => tracing::warn!(
                                %proto, internal, error = %e, "port mapping renewal failed"
                            ),
                        }
                    }
                }
                _ = &mut rx => break,
            }
        }
        // Best effort: we are exiting, and unwithdrawn leases expire within
        // the hour.
        for (proto, internal, external) in all {
            let _ = router.remove(proto, internal, external).await;
        }
    });
    MappingGuard { _shutdown: tx }
}

/// Can this machine reach itself at its public address?
///
/// The gateway's listener is already bound when this runs, so a TCP connect
/// that completes means the router looped the packet back through the mapping
/// we just made. It proves both at once — which is why the connection is opened
/// and dropped without saying anything: the handshake *is* the answer.
///
/// A `false` here does not mean the mapping failed. It means we cannot see it
/// from the inside, and a friend on the outside may well still get through.
async fn probe_hairpin(public: SocketAddr) -> bool {
    matches!(
        tokio::time::timeout(HAIRPIN_TIMEOUT, tokio::net::TcpStream::connect(public)).await,
        Ok(Ok(_))
    )
}

/// Addresses that mean "still inside somebody's network" — RFC 1918, the
/// CGNAT range from RFC 6598, and link-local.
fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                // 100.64.0.0/10 — what an ISP hands you under carrier-grade NAT.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

enum Router {
    Igd(Box<igd_next::aio::Gateway<igd_next::aio::tokio::Tokio>>),
    NatPmp(natpmp::NatpmpAsync<tokio::net::UdpSocket>),
}

async fn igd_router(local_ip: Ipv4Addr) -> Result<Router, String> {
    let options = igd_next::SearchOptions {
        // Bind to the forwarding interface; a wildcard bind may find a router
        // that cannot route to `local_ip`.
        bind_addr: SocketAddr::new(IpAddr::V4(local_ip), 0),
        timeout: Some(DISCOVERY_TIMEOUT),
        ..Default::default()
    };
    igd_next::aio::tokio::search_gateway(options)
        .await
        .map(|g| Router::Igd(Box::new(g)))
        .map_err(|e| e.to_string())
}

async fn natpmp_router() -> Result<Router, String> {
    natpmp::new_tokio_natpmp()
        .await
        .map(Router::NatPmp)
        .map_err(|e| e.to_string())
}

impl Router {
    fn method(&self) -> &'static str {
        match self {
            Router::Igd(_) => "UPnP-IGD",
            Router::NatPmp(_) => "NAT-PMP",
        }
    }

    async fn public_ip(&mut self) -> Result<IpAddr, String> {
        match self {
            Router::Igd(g) => g.get_external_ip().await.map_err(|e| e.to_string()),
            Router::NatPmp(n) => {
                n.send_public_address_request()
                    .await
                    .map_err(|e| e.to_string())?;
                match natpmp_response(n).await? {
                    natpmp::Response::Gateway(g) => Ok(IpAddr::V4(*g.public_address())),
                    other => Err(format!("unexpected NAT-PMP reply: {other:?}")),
                }
            }
        }
    }

    /// Map `port` and return the external port actually granted.
    async fn add(
        &self,
        protocol: PortMappingProtocol,
        local_ip: Ipv4Addr,
        port: u16,
    ) -> Result<u16, String> {
        match self {
            Router::Igd(g) => {
                // Re-adding an identical mapping is how IGD renewal works
                // (idempotent update).
                g.add_port(
                    protocol,
                    port,
                    SocketAddr::new(IpAddr::V4(local_ip), port),
                    LEASE.as_secs() as u32,
                    "Discordia",
                )
                .await
                .map(|()| port)
                .map_err(|e| e.to_string())
            }
            Router::NatPmp(n) => {
                n.send_port_mapping_request(
                    natpmp_proto(protocol),
                    port,
                    port,
                    LEASE.as_secs() as u32,
                )
                .await
                .map_err(|e| e.to_string())?;
                match natpmp_response(n).await? {
                    natpmp::Response::TCP(m) | natpmp::Response::UDP(m) => Ok(m.public_port()),
                    other => Err(format!("unexpected NAT-PMP reply: {other:?}")),
                }
            }
        }
    }

    /// Withdraw a mapping. Each protocol is given the port number *it* keys on
    /// — see the comment in `keep_alive`.
    async fn remove(
        &self,
        protocol: PortMappingProtocol,
        internal_port: u16,
        external_port: u16,
    ) -> Result<(), String> {
        match self {
            Router::Igd(g) => g
                .remove_port(protocol, external_port)
                .await
                .map_err(|e| e.to_string()),
            Router::NatPmp(n) => {
                // NAT-PMP has no delete verb: lifetime 0 is the delete, and
                // public port 0 means "whichever you gave me".
                n.send_port_mapping_request(natpmp_proto(protocol), internal_port, 0, 0)
                    .await
                    .map_err(|e| e.to_string())?;
                natpmp_response(n).await.map(|_| ())
            }
        }
    }
}

fn natpmp_proto(protocol: PortMappingProtocol) -> natpmp::Protocol {
    match protocol {
        PortMappingProtocol::TCP => natpmp::Protocol::TCP,
        PortMappingProtocol::UDP => natpmp::Protocol::UDP,
    }
}

/// Read one NAT-PMP reply, under our own clock.
///
/// `read_response_or_retry` only retries on a socket *error*; against a router
/// that is simply not listening it awaits a datagram that never comes, and the
/// whole self-host start-up waits with it.
async fn natpmp_response(
    n: &natpmp::NatpmpAsync<tokio::net::UdpSocket>,
) -> Result<natpmp::Response, String> {
    match tokio::time::timeout(DISCOVERY_TIMEOUT, n.read_response_or_retry()).await {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(_) => Err("no reply from the router".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_is_a_dialable_url() {
        let mapped = |ip: IpAddr| Mapped {
            method: "UPnP-IGD",
            public_ip: ip,
            gateway_port: 9000,
            media: true,
            quic: true,
            hairpin: true,
        };
        assert_eq!(
            mapped("203.0.113.5".parse().unwrap()).endpoint(),
            "ws://203.0.113.5:9000"
        );
        // v6 needs the brackets, or the port reads as part of the address.
        assert_eq!(
            mapped("2001:db8::1".parse().unwrap()).endpoint(),
            "ws://[2001:db8::1]:9000"
        );
    }

    /// The addresses that mean a mapping would be worthless — chiefly the
    /// CGNAT range, which is the case a home user cannot fix and must be told
    /// about rather than left to debug.
    #[test]
    fn private_addresses_are_not_public() {
        for ip in [
            "192.168.1.1",
            "10.0.0.1",
            "172.16.0.1",
            "100.64.0.1",
            "100.127.255.255",
            "127.0.0.1",
            "169.254.1.1",
        ] {
            assert!(is_private(ip.parse().unwrap()), "{ip} should be private");
        }
        for ip in ["203.0.113.5", "8.8.8.8", "100.128.0.1", "99.255.255.255"] {
            assert!(!is_private(ip.parse().unwrap()), "{ip} should be public");
        }
    }
}
