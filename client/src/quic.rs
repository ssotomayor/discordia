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

pub use dioxusfun_server::quic::Coordination;
use dioxusfun_server::quic::{GATEWAY_ALPN, require_direct, watch_for_relay_fallback};
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
    relay_refusal: dioxusfun_server::quic::RelayRefusal,
}

impl ConnectionGuard {
    /// Whether this session ended because the path fell back to the relay.
    ///
    /// The session sees its socket close and nothing else; the reason lives in
    /// the watcher task. Asked once the session is over, so what the user is
    /// told is the decision that ended it rather than the symptom.
    pub fn relay_refused(&self) -> bool {
        self.relay_refusal.refused()
    }
}

/// Parse a host's advertised transport key.
pub fn parse_endpoint_id(raw: &str) -> Result<EndpointId, String> {
    raw.parse::<EndpointId>()
        .map_err(|e| format!("bad endpoint key: {e}"))
}

/// Parse one advertised address.
///
/// Two shapes travel in the same list because they are the same kind of hint:
/// `1.2.3.4:41234` is somewhere to send packets, and `https://relay.example/`
/// is somewhere to be introduced. Anything else is dropped rather than
/// rejected — one unusable entry should not cost a host its other addresses.
pub fn parse_transport_addr(raw: &str) -> Option<TransportAddr> {
    if let Ok(sock) = raw.parse::<SocketAddr>() {
        return Some(TransportAddr::Ip(sock));
    }
    raw.parse::<iroh::RelayUrl>().ok().map(TransportAddr::Relay)
}

/// Open a gateway stream to `endpoint_id` at one of `addrs`.
///
/// `coordination` decides whether anyone else is involved, and *who*. `None`
/// contacts nothing: the addresses are the ones the host published, and a host
/// that has moved is simply unreachable rather than quietly found again through
/// a third party. `Relay(url)` lets that named relay — in practice the one the
/// user's own rendezvous runs — introduce the two ends so they can punch a
/// hole, and then *insists* the result is direct, refusing the connection if the
/// relay is still carrying it. That refusal is the entire difference between
/// tier 2 and tier 3 (`docs/NETWORKING.md`).
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

    // The key is the destination; the addresses are only hints about where to
    // send the packets. A wrong address fails to connect, a wrong key fails to
    // authenticate, and neither can be mistaken for success.
    let addr = EndpointAddr::new(endpoint_id).with_addrs(addrs.iter().cloned());
    let conn = endpoint
        .connect(addr, GATEWAY_ALPN)
        .await
        .map_err(|e| format!("quic connect: {e}"))?;

    // A coordinator was allowed to introduce us. Whether it is now carrying the
    // conversation is a different question, and this is where it gets asked —
    // before a single frame crosses.
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
