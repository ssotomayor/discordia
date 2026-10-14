use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GatewayRoute {
    Ipv4,
    Ipv6,
    Relay,
    Local,
    Unknown,
}

impl GatewayRoute {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ipv4 => "Direct · IPv4",
            Self::Ipv6 => "Direct · IPv6",
            Self::Relay => "Relay",
            Self::Local => "Local / proxy",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Clone)]
pub(crate) enum RouteSource {
    Socket(Option<IpAddr>),
    Quic(iroh::endpoint::Connection),
}

impl RouteSource {
    pub(crate) fn current(&self) -> GatewayRoute {
        match self {
            Self::Socket(Some(ip)) if ip.is_loopback() => GatewayRoute::Local,
            Self::Socket(Some(_)) => GatewayRoute::Unknown,
            Self::Socket(None) => GatewayRoute::Unknown,
            Self::Quic(conn) => conn.paths().iter().find(|path| path.is_selected()).map_or(
                GatewayRoute::Unknown,
                |path| match path.remote_addr() {
                    iroh::TransportAddr::Ip(addr) if addr.ip().to_canonical().is_ipv4() => {
                        GatewayRoute::Ipv4
                    }
                    iroh::TransportAddr::Ip(_) => GatewayRoute::Ipv6,
                    iroh::TransportAddr::Relay(_) => GatewayRoute::Relay,
                    _ => GatewayRoute::Unknown,
                },
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostConnection {
    pub id: u64,
    pub pubkey: String,
    pub name: String,
    pub route: GatewayRoute,
}
