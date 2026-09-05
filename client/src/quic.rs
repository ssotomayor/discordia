use std::net::SocketAddr;

pub use dioxusfun_server::quic::Coordination;
use dioxusfun_server::quic::GATEWAY_ALPN;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, TransportAddr};

pub type GatewayIo = tokio::io::Join<iroh::endpoint::RecvStream, iroh::endpoint::SendStream>;

pub struct ConnectionGuard {
    _endpoint: Endpoint,
    conn: iroh::endpoint::Connection,
}

impl ConnectionGuard {
    /// True while the coordinator's relay carries the packets. It forwards
    /// ciphertext and sees only the two keys, so this is a matter of latency
    /// and of which party learns an address, not of who can read.
    pub fn relayed(&self) -> bool {
        self.conn
            .paths()
            .iter()
            .any(|p| p.is_selected() && p.is_relay())
    }
}

pub fn parse_endpoint_id(raw: &str) -> Result<EndpointId, String> {
    raw.parse::<EndpointId>()
        .map_err(|e| format!("bad endpoint key: {e}"))
}

pub fn parse_transport_addr(raw: &str) -> Option<TransportAddr> {
    if let Ok(sock) = raw.parse::<SocketAddr>() {
        return Some(TransportAddr::Ip(sock));
    }
    raw.parse::<iroh::RelayUrl>().ok().map(TransportAddr::Relay)
}

/// The relay named in a share string or a directory entry is the one the host
/// is connected to, so it is the one that can introduce us.
pub fn coordination_from(addrs: &[TransportAddr]) -> Coordination {
    addrs
        .iter()
        .find_map(|a| match a {
            TransportAddr::Relay(url) => Some(Coordination::Relay(url.to_string())),
            _ => None,
        })
        .unwrap_or(Coordination::None)
}

pub async fn dial(
    endpoint_id: EndpointId,
    addrs: &[TransportAddr],
    coordination: &Coordination,
) -> Result<(GatewayIo, ConnectionGuard), String> {
    if addrs.is_empty() && !coordination.is_coordinated() {
        return Err("the host published a key but no address to reach it at".into());
    }
    let endpoint = Endpoint::builder(presets::Minimal)
        .relay_mode(coordination.relay_mode()?)
        .bind()
        .await
        .map_err(|e| format!("quic bind: {e}"))?;

    let addr = EndpointAddr::new(endpoint_id).with_addrs(addrs.iter().cloned());
    let conn = endpoint
        .connect(addr, GATEWAY_ALPN)
        .await
        .map_err(|e| format!("quic connect: {e}"))?;

    let (send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| format!("quic stream: {e}"))?;

    Ok((
        tokio::io::join(recv, send),
        ConnectionGuard {
            _endpoint: endpoint,
            conn,
        },
    ))
}

pub fn secret_for(identity: &crate::identity::Identity) -> SecretKey {
    SecretKey::from_bytes(&identity.transport_seed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_keys_are_stable_and_distinct() {
        let a = crate::identity::Identity::restore_from_private_key("11".repeat(32), "a")
            .expect("identity a");
        let b = crate::identity::Identity::restore_from_private_key("22".repeat(32), "b")
            .expect("identity b");

        assert_eq!(secret_for(&a).public(), secret_for(&a).public());
        assert_ne!(secret_for(&a).public(), secret_for(&b).public());
    }

    #[test]
    fn the_transport_seed_is_not_the_identity_secret() {
        let id = crate::identity::Identity::restore_from_private_key("33".repeat(32), "c")
            .expect("identity");
        assert_ne!(id.transport_seed(), [0x33_u8; 32]);
    }

    #[test]
    fn a_relay_in_the_address_list_is_the_coordinator() {
        let addrs = vec![
            parse_transport_addr("192.168.1.5:4433").unwrap(),
            parse_transport_addr("https://relay.example/").unwrap(),
        ];
        assert_eq!(
            coordination_from(&addrs),
            Coordination::Relay("https://relay.example/".into())
        );
        assert_eq!(coordination_from(&addrs[..1]), Coordination::None);
    }
}
