use std::net::SocketAddr;

pub use dioxusfun_server::quic::Coordination;
use dioxusfun_server::quic::{GATEWAY_ALPN, require_direct, watch_for_relay_fallback};
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, TransportAddr};

pub type GatewayIo = tokio::io::Join<iroh::endpoint::RecvStream, iroh::endpoint::SendStream>;

pub struct ConnectionGuard {
    _endpoint: Endpoint,
    _conn: iroh::endpoint::Connection,
    relay_refusal: dioxusfun_server::quic::RelayRefusal,
}

impl ConnectionGuard {
    pub fn relay_refused(&self) -> bool {
        self.relay_refusal.refused()
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

    require_direct(&conn, coordination).await.inspect_err(|_| {
        conn.close(1u32.into(), b"relayed connection refused");
    })?;
    let relay_refusal = watch_for_relay_fallback(conn.clone(), coordination);

    let (send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| format!("quic stream: {e}"))?;

    Ok((
        tokio::io::join(recv, send),
        ConnectionGuard {
            _endpoint: endpoint,
            _conn: conn,
            relay_refusal,
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
}
