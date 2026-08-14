//! The QUIC front door: the same gateway, over a transport that encrypts and
//! authenticates by public key.
//!
//! Everything the TCP listener serves is plaintext — see `TODO.md` under
//! Security. A self-hosted machine at a home address has no domain and no CA,
//! so ordinary TLS would mean a self-signed certificate pinned to the host's
//! key and a hand-written verifier, whose failure mode is the silent one where
//! accepting everything looks like working. A QUIC transport where the peer's
//! identity *is* its public key removes that verifier from our hands entirely,
//! which is the whole argument for it (`docs/NETWORKING.md`, Stage 2).
//!
//! **It serves the ordinary axum router, unchanged.** A QUIC bi-stream is a
//! byte stream, so `hyper` speaks HTTP/1.1 over it exactly as it does over TCP,
//! WebSocket upgrade included — `axum::serve` does the same thing to a
//! `TcpStream` a few layers down. That is deliberate: `handle_connection` is
//! two thousand lines with a hundred-odd references to its socket, and making
//! it generic over a transport would be a large diff through the most
//! load-bearing code in the repo to arrive at the same place. The WebSocket
//! stays as the framing, and this changes what carries it. `/media/{name}` and
//! the health route come along for free.
//!
//! One connection carries many streams, and each bi-stream is one HTTP
//! connection — so a client can open the gateway socket and fetch blobs over
//! the same QUIC connection without a second handshake.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointId, SecretKey};

/// ALPN for the gateway protocol. Versioned separately from anything in
/// `protocol`: it names the transport contract, and a peer that speaks a
/// different one should be refused at the handshake rather than after it.
pub const GATEWAY_ALPN: &[u8] = b"dioxusfun/gateway/1";

/// Whether a third party may help two peers find each other.
///
/// This is `docs/NETWORKING.md`'s tier 1 / tier 2 line, and it is a setting
/// rather than a default because contacting a coordinator silently is exactly
/// the surprise the design exists to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coordination {
    /// Nobody else. Peers reach each other at addresses they were told, or not
    /// at all.
    None,
    /// A relay may introduce the two ends so they can punch a hole — and then
    /// it is *required to step out*. See [`require_direct`].
    CoordinatorOnly,
}

/// How long to wait for hole punching to produce a direct path.
///
/// A relayed path comes up almost immediately and a direct one follows it, so
/// this is the window in which the punch either succeeds or has not. Long
/// enough for a round of candidates, short enough that a person is still
/// waiting rather than gone.
const PUNCH_WINDOW: Duration = Duration::from_secs(6);

/// What a connection's paths add up to, reduced to the two facts the policy
/// turns on.
///
/// A separate type because iroh's `PathList` borrows the connection and cannot
/// be constructed by hand — reducing first is what lets the decision below be
/// tested without a live relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathSummary {
    /// A direct IP path exists and is the one in use.
    pub direct_selected: bool,
    /// A direct path exists at all, selected or not.
    pub direct_available: bool,
}

impl PathSummary {
    fn of(conn: &iroh::endpoint::Connection) -> Self {
        let paths = conn.paths();
        Self {
            direct_selected: paths.iter().any(|p| p.is_ip() && p.is_selected()),
            direct_available: paths.iter().any(|p| p.is_ip()),
        }
    }
}

/// Whether this connection may carry a session, under `coordination`.
///
/// **This is the whole difference between tier 2 and tier 3.** A relay that
/// coordinates a hole punch will just as happily carry the data when the punch
/// fails, and the connection still works — which is precisely why it has to be
/// refused rather than assumed away. Honouring the setting means checking, and
/// then saying no.
pub fn verdict(summary: PathSummary, coordination: Coordination) -> Result<(), String> {
    match coordination {
        // Nothing to enforce: with no relay configured there is no other kind
        // of path this connection could be riding on.
        Coordination::None => Ok(()),
        Coordination::CoordinatorOnly if summary.direct_selected => Ok(()),
        Coordination::CoordinatorOnly if summary.direct_available => Err(
            "a direct path exists but the connection is not using it — refusing rather than \
             letting the relay carry the session"
                .into(),
        ),
        Coordination::CoordinatorOnly => Err(
            "hole punching did not produce a direct path, so this connection would be carried \
             by the relay. It was refused: the coordinator was allowed to introduce us, not to \
             read what we say."
                .into(),
        ),
    }
}

/// Wait for hole punching to produce a direct path, then insist on it.
///
/// Returns once a direct path is selected, or an error once the window closes.
/// The connection is left open either way — closing it is the caller's decision,
/// because the caller knows whether it has anything better to fall back to.
pub async fn require_direct(
    conn: &iroh::endpoint::Connection,
    coordination: Coordination,
) -> Result<(), String> {
    if coordination == Coordination::None {
        return Ok(());
    }
    let deadline = tokio::time::Instant::now() + PUNCH_WINDOW;
    loop {
        let summary = PathSummary::of(conn);
        if summary.direct_selected {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return verdict(summary, coordination);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Kill the connection if it ever falls back onto the relay.
///
/// Checking once at the start is not enough and the difference is not
/// theoretical: iroh will move traffic back to the relay when a direct path
/// degrades, so a session that began direct can quietly become a relayed one
/// while the UI still says otherwise. Spawned for the life of the connection.
pub fn watch_for_relay_fallback(conn: iroh::endpoint::Connection, coordination: Coordination) {
    if coordination == Coordination::None {
        return;
    }
    tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut paths = conn.paths_stream();
        while let Some(list) = paths.next().await {
            let direct_selected = list.iter().any(|p| p.is_ip() && p.is_selected());
            if !direct_selected {
                tracing::warn!(
                    "connection fell back to the relay — closing, because a coordinator was \
                     allowed to introduce us and not to carry us"
                );
                conn.close(1u32.into(), b"relay fallback refused");
                return;
            }
        }
    });
}

/// A bound QUIC endpoint serving the gateway.
pub struct QuicHandle {
    /// The public half of this host's transport key. A joiner dials *this*, not
    /// an address — which is what makes the address itself changeable.
    pub endpoint_id: EndpointId,
    /// The UDP sockets it is actually listening on, for advertising.
    pub sockets: Vec<SocketAddr>,
    endpoint: Endpoint,
    task: tokio::task::JoinHandle<()>,
}

impl QuicHandle {
    /// Stop accepting and close the endpoint.
    pub async fn shutdown(self) {
        self.task.abort();
        self.endpoint.close().await;
    }
}

/// Bind a QUIC endpoint and serve `router` on it.
///
/// `secret` is the host's transport identity. Pass the same one across restarts
/// and friends keep reaching you at the same key; pass `None` and a fresh one is
/// generated, which is right for a throwaway and wrong for a host anyone has
/// saved.
///
/// **No relay and no discovery service** (`presets::Minimal`). That is the
/// point of this stage: a peer reaches us because it was told an address, and
/// no third party is contacted to arrange it — `docs/NETWORKING.md` tier 1.
/// Relay-assisted connections are a later stage precisely because they involve
/// someone else, and that has to be a choice rather than a default.
pub async fn serve_quic(router: Router, secret: Option<SecretKey>) -> Result<QuicHandle, String> {
    serve_on(bind_quic(secret, Coordination::None).await?, router)
}

/// Bind the endpoint without serving anything on it yet.
///
/// Split from `serve_on` for the same reason `bind_with_fallback` is split from
/// `spawn_on`: a self-hosting client has to advertise its key and UDP port
/// *before* it has a router to serve — the port needs mapping and the address
/// goes out with the registration, both of which happen while the gateway is
/// still being assembled.
pub async fn bind_quic(
    secret: Option<SecretKey>,
    coordination: Coordination,
) -> Result<Endpoint, String> {
    // `Minimal` contacts nothing; `N0` brings relays and address lookup, which
    // is what makes a hole punch possible and is also the third party the
    // setting exists to gate.
    let mut builder = match coordination {
        Coordination::None => Endpoint::builder(presets::Minimal),
        Coordination::CoordinatorOnly => Endpoint::builder(presets::N0),
    }
    .alpns(vec![GATEWAY_ALPN.to_vec()]);
    if let Some(secret) = secret {
        builder = builder.secret_key(secret);
    }
    builder.bind().await.map_err(|e| format!("quic bind: {e}"))
}

/// Start accepting on an endpoint bound earlier.
pub fn serve_on(endpoint: Endpoint, router: Router) -> Result<QuicHandle, String> {
    serve_on_with(endpoint, router, Coordination::None)
}

/// As [`serve_on`], enforcing `coordination` on every connection accepted.
pub fn serve_on_with(
    endpoint: Endpoint,
    router: Router,
    coordination: Coordination,
) -> Result<QuicHandle, String> {
    let endpoint_id = endpoint.id();
    let sockets = endpoint.bound_sockets();
    tracing::info!(%endpoint_id, ?sockets, "gateway listening on QUIC");

    let accepting = endpoint.clone();
    let task = tokio::spawn(async move {
        while let Some(incoming) = accepting.accept().await {
            let router = router.clone();
            tokio::spawn(async move {
                if let Err(e) = serve_connection(incoming, router, coordination).await {
                    tracing::debug!(error = %e, "quic connection ended");
                }
            });
        }
    });

    Ok(QuicHandle {
        endpoint_id,
        sockets,
        endpoint,
        task,
    })
}

/// A dialable address for this endpoint, with loopback substituted for the
/// wildcard.
///
/// `bound_sockets` reports what the socket is bound to, which is usually
/// `0.0.0.0:port` — a valid bind and a meaningless destination. Anything
/// handing these addresses to a peer has to make that substitution somewhere,
/// so it lives here rather than at each caller.
pub fn dialable_addrs(sockets: &[SocketAddr]) -> Vec<SocketAddr> {
    sockets
        .iter()
        .map(|s| {
            let mut s = *s;
            if s.ip().is_unspecified() {
                s.set_ip(match s.ip() {
                    std::net::IpAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
                    std::net::IpAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
                });
            }
            s
        })
        .collect()
}

/// One peer: accept bi-streams until they stop coming, serving HTTP on each.
async fn serve_connection(
    incoming: iroh::endpoint::Incoming,
    router: Router,
    coordination: Coordination,
) -> Result<(), String> {
    let conn = incoming.await.map_err(|e| format!("handshake: {e}"))?;
    let remote = conn.remote_id();

    // Enforced on this side too, not only the dialling one. Otherwise the
    // guarantee is only as good as the other end's build, and the relay would
    // still end up carrying a session somebody asked it not to.
    if let Err(e) = require_direct(&conn, coordination).await {
        tracing::warn!(%remote, error = %e, "refusing a relayed connection");
        conn.close(1u32.into(), b"relayed connection refused");
        return Err(e);
    }
    watch_for_relay_fallback(conn.clone(), coordination);
    tracing::info!(%remote, "quic peer connected");

    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(pair) => pair,
            // The ordinary end of a connection, not a fault.
            Err(e) => {
                tracing::debug!(%remote, error = %e, "no more streams");
                return Ok(());
            }
        };
        let router = router.clone();
        tokio::spawn(async move {
            let io = hyper_util::rt::TokioIo::new(tokio::io::join(recv, send));
            let service = hyper_util::service::TowerToHyperService::new(router);
            // `with_upgrades` is not optional here — it is what lets the
            // WebSocket upgrade at `/gateway` take the stream over, which is
            // the entire reason this exists.
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                tracing::debug!(error = %e, "quic http stream ended");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use iroh::{EndpointAddr, TransportAddr};
    use tokio_tungstenite::tungstenite::Message;

    /// The real gateway router over a temp data dir.
    async fn gateway_router() -> Router {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "dioxusfun-quic-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let ctx = crate::build_context(crate::ServerConfig {
            livekit: crate::livekit::LiveKitConfig::from_env(),
            operators: Default::default(),
            data_dir: dir,
        })
        .await
        .expect("build context");
        crate::http::router(ctx)
    }

    /// Dial the endpoint and open one HTTP-over-QUIC stream to it.
    ///
    /// The endpoint and connection come back with the stream because dropping
    /// either closes it — they have to stay alive for as long as the caller is
    /// reading, which is what the `_`-bound guards in each test are doing.
    async fn open_stream(
        handle: &QuicHandle,
    ) -> (
        Endpoint,
        iroh::endpoint::Connection,
        impl tokio::io::AsyncRead + tokio::io::AsyncWrite + use<>,
    ) {
        let client = Endpoint::builder(presets::Minimal)
            .bind()
            .await
            .expect("client endpoint");
        // Address *and* key: the address says where to send packets, the key is
        // who must be at the other end. A wrong key fails the handshake — which
        // is the property this transport exists for.
        let addr = EndpointAddr::new(handle.endpoint_id).with_addrs(
            dialable_addrs(&handle.sockets)
                .into_iter()
                .map(TransportAddr::Ip),
        );
        let conn = client
            .connect(addr, GATEWAY_ALPN)
            .await
            .expect("quic connect");
        let (send, recv) = conn.open_bi().await.expect("open bi");
        (client, conn, tokio::io::join(recv, send))
    }

    /// The whole premise in one test: an ordinary axum route, served over QUIC.
    ///
    /// If this passes, `hyper` is speaking HTTP/1.1 over a bi-stream and the
    /// existing router needed no changes to be reachable that way.
    #[tokio::test]
    async fn http_routes_answer_over_quic() {
        let handle = serve_quic(gateway_router().await, None)
            .await
            .expect("serve quic");
        let (_ep, _conn, io) = open_stream(&handle).await;

        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(Box::pin(io)))
                .await
                .expect("http handshake");
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let req = hyper::Request::builder()
            .uri("/")
            .header(hyper::header::HOST, "gateway.invalid")
            .body(String::new())
            .unwrap();
        let res = sender.send_request(req).await.expect("request");
        assert_eq!(res.status(), 200);

        handle.shutdown().await;
    }

    /// And the part that matters: the WebSocket upgrade survives the trip, so
    /// the gateway protocol runs unchanged over an encrypted, key-authenticated
    /// transport. `Hello` is the server's first frame, so receiving it proves
    /// the upgrade completed *and* that a real connection handler is on the
    /// other end rather than a socket that merely opened.
    #[tokio::test]
    async fn the_gateway_speaks_websocket_over_quic() {
        let handle = serve_quic(gateway_router().await, None)
            .await
            .expect("serve quic");
        let (_ep, _conn, io) = open_stream(&handle).await;

        let (mut ws, response) =
            tokio_tungstenite::client_async("ws://gateway.invalid/gateway", Box::pin(io))
                .await
                .expect("websocket upgrade over quic");
        assert_eq!(response.status(), 101);

        let frame = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for Hello")
            .expect("stream ended")
            .expect("frame");
        let text = match frame {
            Message::Text(t) => t,
            other => panic!("expected text, got {other:?}"),
        };
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["op"], "hello", "got {text}");
        assert!(parsed["d"]["nonce"].as_str().is_some_and(|n| !n.is_empty()));

        handle.shutdown().await;
    }

    /// The tier-2 promise, stated as a table.
    ///
    /// A relay that arranges a hole punch will carry the data just as happily
    /// when the punch fails, and the connection *works* either way — which is
    /// exactly why "coordinator, never carrier" has to be a refusal and not an
    /// assumption. These are the four cases that decide it.
    #[test]
    fn a_coordinator_may_introduce_but_not_carry() {
        let direct = PathSummary {
            direct_selected: true,
            direct_available: true,
        };
        let relayed = PathSummary {
            direct_selected: false,
            direct_available: false,
        };
        let punched_but_unused = PathSummary {
            direct_selected: false,
            direct_available: true,
        };

        // With no coordinator there is no relay to fall back to, so nothing is
        // refused — a connection that exists at all is direct by construction.
        assert!(verdict(relayed, Coordination::None).is_ok());
        assert!(verdict(direct, Coordination::None).is_ok());

        // With one, only a selected direct path passes.
        assert!(verdict(direct, Coordination::CoordinatorOnly).is_ok());

        let refused = verdict(relayed, Coordination::CoordinatorOnly).unwrap_err();
        assert!(refused.contains("refused"), "{refused}");
        // The message has to say what happened and why, because this is a
        // working connection being turned away — a bare failure would look like
        // the host being offline.
        assert!(refused.contains("relay"), "{refused}");

        // And the subtle one: a direct path existing is not the same as it
        // being used. Traffic on the relay is traffic the relay can read,
        // whatever else was negotiated alongside it.
        assert!(verdict(punched_but_unused, Coordination::CoordinatorOnly).is_err());
    }

    /// A loopback connection has a direct path and no relay, so enforcement is
    /// a no-op — the check must not refuse the case it is meant to allow.
    #[tokio::test]
    async fn enforcement_passes_a_genuinely_direct_connection() {
        let handle = serve_quic(gateway_router().await, None)
            .await
            .expect("serve quic");
        let (_ep, conn, _io) = open_stream(&handle).await;

        assert!(
            require_direct(&conn, Coordination::CoordinatorOnly)
                .await
                .is_ok()
        );
        assert!(PathSummary::of(&conn).direct_selected);

        handle.shutdown().await;
    }

    /// Dialling the right address with the wrong key must fail. This is the
    /// difference between "encrypted" and "authenticated", and the reason this
    /// transport needs no certificate verifier of our own.
    #[tokio::test]
    async fn a_wrong_key_cannot_connect() {
        let handle = serve_quic(gateway_router().await, None)
            .await
            .expect("serve quic");
        let client = Endpoint::builder(presets::Minimal)
            .bind()
            .await
            .expect("client endpoint");

        // Somebody else's key, at the right address.
        let impostor = SecretKey::generate().public();
        let addr = EndpointAddr::new(impostor).with_addrs(
            dialable_addrs(&handle.sockets)
                .into_iter()
                .map(TransportAddr::Ip),
        );

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.connect(addr, GATEWAY_ALPN),
        )
        .await;
        // Either a refusal or no answer at all is correct; the point is that no
        // session is established.
        if let Ok(Ok(_)) = result {
            panic!("connected to a host whose key we got wrong");
        }

        handle.shutdown().await;
    }
}
