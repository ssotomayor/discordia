use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use igd_next::PortMappingProtocol;

const LEASE: Duration = Duration::from_secs(3600);

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

const HAIRPIN_TIMEOUT: Duration = Duration::from_secs(3);

/// No TCP gateway port: off this machine the gateway speaks QUIC only, so the
/// only thing to open for chat is the UDP port the QUIC endpoint bound.
#[derive(Debug, Clone, Copy)]
pub struct Ports {
    pub media_tcp: u16,
    pub media_tcp_ice: u16,
    pub media_udp: u16,
    pub quic_udp: u16,
}

#[derive(Debug, Clone)]
pub struct Mapped {
    pub method: &'static str,
    pub public_ip: IpAddr,
    pub media: bool,
    pub quic: bool,
    pub hairpin: bool,
}

pub struct MappingGuard {
    _shutdown: tokio::sync::oneshot::Sender<()>,
}

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

async fn finish(
    mut router: Router,
    local_ip: Ipv4Addr,
    ports: Ports,
) -> Result<(Mapped, MappingGuard), String> {
    let public_ip = router.public_ip().await?;
    if is_private(public_ip) {
        return Err(format!(
            "your router's own address ({public_ip}) is private, so it is behind \
             another NAT — usually carrier-grade NAT at your ISP. Nothing this \
             machine can do opens a path in; you need the relay, or a provider \
             that gives you a public address."
        ));
    }

    let quic_ok = match router
        .add(PortMappingProtocol::UDP, local_ip, ports.quic_udp)
        .await
    {
        Ok(external) if external == ports.quic_udp => true,
        Ok(external) => {
            tracing::warn!(
                wanted = ports.quic_udp,
                granted = external,
                "router renumbered the QUIC port — friends cannot be told a port we do not hold"
            );
            false
        }
        Err(e) => return Err(format!("the router refused to forward the QUIC port: {e}")),
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

    // Hairpin matters for the SFU, which replaces its LAN candidate with the
    // advertised address, so the SFU's own TCP port is what gets probed.
    let hairpin = media && probe_hairpin(SocketAddr::new(public_ip, ports.media_tcp)).await;

    let mapped = Mapped {
        method: router.method(),
        public_ip,
        media,
        quic: quic_ok,
        hairpin,
    };
    Ok((mapped, keep_alive(router, local_ip, ports)))
}

fn keep_alive(router: Router, local_ip: Ipv4Addr, ports: Ports) -> MappingGuard {
    let all = [
        (PortMappingProtocol::UDP, ports.quic_udp, ports.quic_udp),
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
        for (proto, internal, external) in all {
            let _ = router.remove(proto, internal, external).await;
        }
    });
    MappingGuard { _shutdown: tx }
}

async fn probe_hairpin(public: SocketAddr) -> bool {
    matches!(
        tokio::time::timeout(HAIRPIN_TIMEOUT, tokio::net::TcpStream::connect(public)).await,
        Ok(Ok(_))
    )
}

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
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

    async fn add(
        &self,
        protocol: PortMappingProtocol,
        local_ip: Ipv4Addr,
        port: u16,
    ) -> Result<u16, String> {
        match self {
            Router::Igd(g) => g
                .add_port(
                    protocol,
                    port,
                    SocketAddr::new(IpAddr::V4(local_ip), port),
                    LEASE.as_secs() as u32,
                    "Discordia",
                )
                .await
                .map(|()| port)
                .map_err(|e| e.to_string()),
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
