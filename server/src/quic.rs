use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointId, SecretKey};

pub const GATEWAY_ALPN: &[u8] = b"dioxusfun/gateway/1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Coordination {
    None,
    Relay(String),
}

impl Coordination {
    pub fn is_coordinated(&self) -> bool {
        matches!(self, Coordination::Relay(_))
    }

    pub fn relay_mode(&self) -> Result<iroh::RelayMode, String> {
        match self {
            Coordination::None => Ok(iroh::RelayMode::Disabled),
            Coordination::Relay(url) => {
                let url: iroh::RelayUrl = url
                    .parse()
                    .map_err(|e| format!("bad relay url {url}: {e}"))?;
                Ok(iroh::RelayMode::Custom(iroh::RelayMap::from_iter([url])))
            }
        }
    }
}

const PUNCH_WINDOW: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathSummary {
    pub direct_selected: bool,
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

pub fn verdict(summary: PathSummary, coordination: &Coordination) -> Result<(), String> {
    match coordination {
        Coordination::None => Ok(()),
        Coordination::Relay(_) if summary.direct_selected => Ok(()),
        Coordination::Relay(_) if summary.direct_available => Err(
            "a direct path exists but the connection is not using it — refusing rather than \
             letting the relay carry the session"
                .into(),
        ),
        Coordination::Relay(_) => Err(
            "hole punching did not produce a direct path, so this connection would be carried \
             by the relay. It was refused: the coordinator was allowed to introduce us, not to \
             read what we say."
                .into(),
        ),
    }
}

pub async fn require_direct(
    conn: &iroh::endpoint::Connection,
    coordination: &Coordination,
) -> Result<(), String> {
    if !coordination.is_coordinated() {
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

#[derive(Clone, Default)]
pub struct RelayRefusal(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl RelayRefusal {
    pub fn refused(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn set(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

pub fn watch_for_relay_fallback(
    conn: iroh::endpoint::Connection,
    coordination: &Coordination,
) -> RelayRefusal {
    let refusal = RelayRefusal::default();
    if !coordination.is_coordinated() {
        return refusal;
    }
    let flag = refusal.clone();
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
                flag.set();
                conn.close(1u32.into(), b"relay fallback refused");
                return;
            }
        }
    });
    refusal
}

pub struct QuicHandle {
    pub endpoint_id: EndpointId,
    pub sockets: Vec<SocketAddr>,
    endpoint: Endpoint,
    task: tokio::task::JoinHandle<()>,
}

impl QuicHandle {
    pub async fn shutdown(self) {
        self.task.abort();
        self.endpoint.close().await;
    }
}

pub async fn serve_quic(router: Router, secret: Option<SecretKey>) -> Result<QuicHandle, String> {
    serve_on(bind_quic(secret, &Coordination::None).await?, router)
}

pub async fn bind_quic(
    secret: Option<SecretKey>,
    coordination: &Coordination,
) -> Result<Endpoint, String> {
    let mut builder = Endpoint::builder(presets::Minimal)
        .alpns(vec![GATEWAY_ALPN.to_vec()])
        .relay_mode(coordination.relay_mode()?);
    if let Some(secret) = secret {
        builder = builder.secret_key(secret);
    }
    builder.bind().await.map_err(|e| format!("quic bind: {e}"))
}

pub fn serve_on(endpoint: Endpoint, router: Router) -> Result<QuicHandle, String> {
    serve_on_with(endpoint, router, Coordination::None)
}

pub fn serve_on_with(
    endpoint: Endpoint,
    router: Router,
    coordination: Coordination,
) -> Result<QuicHandle, String> {
    let coordination = std::sync::Arc::new(coordination);
    let endpoint_id = endpoint.id();
    let sockets = endpoint.bound_sockets();
    tracing::info!(%endpoint_id, ?sockets, "gateway listening on QUIC");

    let accepting = endpoint.clone();
    let task = tokio::spawn(async move {
        while let Some(incoming) = accepting.accept().await {
            let router = router.clone();
            let coordination = coordination.clone();
            tokio::spawn(async move {
                if let Err(e) = serve_connection(incoming, router, &coordination).await {
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

async fn serve_connection(
    incoming: iroh::endpoint::Incoming,
    router: Router,
    coordination: &Coordination,
) -> Result<(), String> {
    let conn = incoming.await.map_err(|e| format!("handshake: {e}"))?;
    let remote = conn.remote_id();

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
            Err(e) => {
                tracing::debug!(%remote, error = %e, "no more streams");
                return Ok(());
            }
        };
        let router = router.clone();
        tokio::spawn(async move {
            let io = hyper_util::rt::TokioIo::new(tokio::io::join(recv, send));
            let service = hyper_util::service::TowerToHyperService::new(router);
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

        let coordinated = Coordination::Relay("https://relay.example/".into());

        assert!(verdict(relayed, &Coordination::None).is_ok());
        assert!(verdict(direct, &Coordination::None).is_ok());

        assert!(verdict(direct, &coordinated).is_ok());

        let refused = verdict(relayed, &coordinated).unwrap_err();
        assert!(refused.contains("refused"), "{refused}");
        assert!(refused.contains("relay"), "{refused}");

        assert!(verdict(punched_but_unused, &coordinated).is_err());
    }

    #[tokio::test]
    async fn enforcement_passes_a_genuinely_direct_connection() {
        let handle = serve_quic(gateway_router().await, None)
            .await
            .expect("serve quic");
        let (_ep, conn, _io) = open_stream(&handle).await;

        assert!(
            require_direct(&conn, &Coordination::Relay("https://relay.example/".into()))
                .await
                .is_ok()
        );
        assert!(PathSummary::of(&conn).direct_selected);

        handle.shutdown().await;
    }

    #[tokio::test]
    async fn a_wrong_key_cannot_connect() {
        let handle = serve_quic(gateway_router().await, None)
            .await
            .expect("serve quic");
        let client = Endpoint::builder(presets::Minimal)
            .bind()
            .await
            .expect("client endpoint");

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
        if let Ok(Ok(_)) = result {
            panic!("connected to a host whose key we got wrong");
        }

        handle.shutdown().await;
    }
}
