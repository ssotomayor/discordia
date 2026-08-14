//! Dialling a host over QUIC, and hosting one.
//!
//! The server half lives in `dioxusfun_server::quic`, which serves the ordinary
//! axum router over QUIC bi-streams — WebSocket upgrade included. So this side
//! is small: open a stream, run the same WebSocket client handshake we run over
//! TCP, and hand the socket back. Everything above it — Identify, the frame
//! loop in `net::run` — is unchanged and cannot tell the difference.
//!
//! What it buys is the difference between a *direct* connection and a *private*
//! one. `ws://` is plaintext to every hop on the path; this is encrypted, and
//! the peer is authenticated by its public key rather than by a certificate we
//! would otherwise have to verify ourselves. See `docs/NETWORKING.md`.

use std::net::SocketAddr;

use dioxusfun_server::quic::GATEWAY_ALPN;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey, TransportAddr};

/// The byte stream a gateway session runs on.
pub type GatewayIo = tokio::io::Join<iroh::endpoint::RecvStream, iroh::endpoint::SendStream>;

/// Keeps the connection under a [`GatewayIo`] alive.
///
/// Dropping the endpoint or the connection closes the stream, so the caller has
/// to hold this for as long as the session lasts. Handed back separately from
/// the stream because the stream gets moved into the WebSocket and the guard
/// cannot follow it there.
pub struct ConnectionGuard {
    _endpoint: Endpoint,
    _conn: iroh::endpoint::Connection,
}

/// Parse a host's advertised transport key.
pub fn parse_endpoint_id(raw: &str) -> Result<EndpointId, String> {
    raw.parse::<EndpointId>()
        .map_err(|e| format!("bad endpoint key: {e}"))
}

/// Open a gateway stream to `endpoint_id` at one of `addrs`.
///
/// **No relay and no discovery** (`presets::Minimal`), matching the host side:
/// the addresses are the ones the host published, and nothing else is contacted
/// to reach it. A host that has moved is simply unreachable here rather than
/// quietly found again through a third party — which is the distinction the
/// whole document is about, and it should not be made by a default.
pub async fn dial(
    endpoint_id: EndpointId,
    addrs: &[SocketAddr],
) -> Result<(GatewayIo, ConnectionGuard), String> {
    if addrs.is_empty() {
        return Err("the host published a key but no address to reach it at".into());
    }
    let endpoint = Endpoint::builder(presets::Minimal)
        .bind()
        .await
        .map_err(|e| format!("quic bind: {e}"))?;

    // The key is the destination; the addresses are only hints about where to
    // send the packets. A wrong address fails to connect, a wrong key fails to
    // authenticate, and neither can be mistaken for success.
    let addr =
        EndpointAddr::new(endpoint_id).with_addrs(addrs.iter().copied().map(TransportAddr::Ip));
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
            _conn: conn,
        },
    ))
}

/// This host's transport secret, derived from its Nostr identity.
///
/// See `identity::transport_seed` for why it is derived rather than stored.
pub fn secret_for(identity: &crate::identity::Identity) -> SecretKey {
    SecretKey::from_bytes(&identity.transport_seed())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transport key must be stable for an identity and different across
    /// identities — the first is what lets a friend keep reaching you after a
    /// restart, the second is what stops two hosts colliding.
    #[test]
    fn transport_keys_are_stable_and_distinct() {
        let a = crate::identity::Identity::restore_from_private_key("11".repeat(32), "a")
            .expect("identity a");
        let b = crate::identity::Identity::restore_from_private_key("22".repeat(32), "b")
            .expect("identity b");

        assert_eq!(secret_for(&a).public(), secret_for(&a).public());
        assert_ne!(secret_for(&a).public(), secret_for(&b).public());
    }

    /// And it must not be the Nostr key wearing a hat: the seed is a one-way
    /// hash, so the bytes on the wire are not the account secret.
    #[test]
    fn the_transport_seed_is_not_the_identity_secret() {
        let id = crate::identity::Identity::restore_from_private_key("33".repeat(32), "c")
            .expect("identity");
        assert_ne!(id.transport_seed(), [0x33_u8; 32]);
    }
}
