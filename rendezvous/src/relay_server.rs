//! An iroh relay, run by this rendezvous.
//!
//! Hole punching needs a third party — two machines behind NAT cannot learn
//! each other's public addresses unaided (`docs/NETWORKING.md`). The question
//! has only ever been *which* third party. Until now the answer was n0's public
//! relays, which meant an app talking to a company nobody chose, on first run,
//! under someone else's rate limits and terms.
//!
//! A rendezvous is already the third party its users picked. It already knows
//! who is registered and already carries their traffic when nothing better
//! works. Coordinating a punch is strictly *less* than it does today: a relayed
//! iroh connection is encrypted end to end, so this cannot read what it
//! forwards, while the plaintext WebSocket proxy next door can.
//!
//! So it runs here, in-process. One service to deploy, one address for a user
//! to configure, and the rendezvous knows its own relay's URL without being
//! told — which is what lets it advertise it to hosts and joiners as part of
//! registration instead of demanding more configuration.
//!
//! **No TLS here.** The relay serves plain HTTP and expects to sit behind
//! whatever terminates TLS for the rest of the deployment, or to be reached
//! directly on a testing box. What crosses it is already encrypted by the
//! endpoints; TLS on this hop would hide metadata from the network, not content
//! from us, and we already see the metadata by being the relay.

use std::net::SocketAddr;

use iroh_relay::server::{RelayConfig, Server, ServerConfig};

/// Default port for the relay's HTTP service.
///
/// Not 80: this shares a box with the gateway, the SFU and the rendezvous
/// itself, and a privileged port would mean running as root for no gain.
pub const DEFAULT_RELAY_PORT: u16 = 7701;

/// A running relay, and the URL to hand to clients.
pub struct RelayHandle {
    /// What a client should be told to use. Derived from the configured public
    /// host, not from the bind address, which is usually a wildcard and would
    /// tell a client to dial itself.
    pub url: String,
    _server: Server,
}

/// Start a relay on `bind`, advertising itself as `public_url`.
///
/// Returns `None` when no public URL is configured: a relay nobody can be told
/// about is a port held open for nothing. That is the ordinary case for a
/// rendezvous that has not opted in, and its users simply get no coordination —
/// they are relayed by the WebSocket proxy exactly as they were before this
/// existed, rather than being quietly handed to somebody else's servers.
pub async fn spawn(bind: SocketAddr, public_url: Option<String>) -> Option<RelayHandle> {
    let url = public_url?;

    // Both config structs are `#[non_exhaustive]`, so they are built by their
    // constructors and adjusted, not by struct literal.
    //
    // Defaults are right here: no TLS (see the module docs), default limits, and
    // access open to anyone who can reach this rendezvous. Narrowing access to
    // registered hosts would not help — the joiners being introduced are not
    // registered — and the relay cannot read what it carries either way.
    let mut config = ServerConfig::default();
    config.relay = Some(RelayConfig::new(bind));

    match Server::spawn(config).await {
        Ok(server) => {
            tracing::info!(%url, %bind, "iroh relay listening — this rendezvous coordinates its own hole punching");
            Some(RelayHandle {
                url,
                _server: server,
            })
        }
        Err(e) => {
            // Not fatal. Everything else the rendezvous does still works, and
            // its users fall back to the proxy.
            tracing::error!(error = %e, "could not start the iroh relay — hole punching will be unavailable");
            None
        }
    }
}
