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

use axum::Router;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointId, SecretKey};

/// ALPN for the gateway protocol. Versioned separately from anything in
/// `protocol`: it names the transport contract, and a peer that speaks a
/// different one should be refused at the handshake rather than after it.
pub const GATEWAY_ALPN: &[u8] = b"dioxusfun/gateway/1";

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
    serve_on(bind_quic(secret).await?, router)
}

/// Bind the endpoint without serving anything on it yet.
///
/// Split from `serve_on` for the same reason `bind_with_fallback` is split from
/// `spawn_on`: a self-hosting client has to advertise its key and UDP port
/// *before* it has a router to serve — the port needs mapping and the address
/// goes out with the registration, both of which happen while the gateway is
/// still being assembled.
pub async fn bind_quic(secret: Option<SecretKey>) -> Result<Endpoint, String> {
    let mut builder = Endpoint::builder(presets::Minimal).alpns(vec![GATEWAY_ALPN.to_vec()]);
    if let Some(secret) = secret {
        builder = builder.secret_key(secret);
    }
    builder.bind().await.map_err(|e| format!("quic bind: {e}"))
}

/// Start accepting on an endpoint bound earlier.
pub fn serve_on(endpoint: Endpoint, router: Router) -> Result<QuicHandle, String> {
    let endpoint_id = endpoint.id();
    let sockets = endpoint.bound_sockets();
    tracing::info!(%endpoint_id, ?sockets, "gateway listening on QUIC");

    let accepting = endpoint.clone();
    let task = tokio::spawn(async move {
        while let Some(incoming) = accepting.accept().await {
            let router = router.clone();
            tokio::spawn(async move {
                if let Err(e) = serve_connection(incoming, router).await {
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
) -> Result<(), String> {
    let conn = incoming.await.map_err(|e| format!("handshake: {e}"))?;
    let remote = conn.remote_id();
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
