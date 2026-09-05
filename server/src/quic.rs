use std::net::SocketAddr;
use std::path::Path;

use axum::Router;
use axum::extract::ConnectInfo;
use iroh::endpoint::{IncomingAddr, presets};
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

/// A relayed connection is accepted: the coordinator's relay forwards QUIC
/// ciphertext and sees only the two keys, so it can carry what it cannot read.
/// A direct peer's address becomes `ConnectInfo`, which is what the per-address
/// cap and the LiveKit URL choice read; a relayed peer has none.
async fn serve_connection(
    incoming: iroh::endpoint::Incoming,
    router: Router,
    _coordination: &Coordination,
) -> Result<(), String> {
    let arrived = incoming.remote_addr();
    let conn = incoming.await.map_err(|e| format!("handshake: {e}"))?;
    let remote = conn.remote_id();
    let peer = match arrived {
        IncomingAddr::Ip(addr) => Some(addr),
        IncomingAddr::Relay { url, .. } => {
            tracing::info!(%remote, %url, "quic peer connected through the relay");
            None
        }
        _ => None,
    };
    let router = match peer {
        Some(addr) => {
            tracing::info!(%remote, %addr, "quic peer connected");
            router.layer(axum::Extension(ConnectInfo(addr)))
        }
        None => router,
    };

    // One QUIC peer is one machine; each stream it opens is a whole HTTP
    // connection on our side, so the count is bounded here, not in the router.
    let streams = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_STREAMS_PER_PEER));
    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(pair) => pair,
            Err(e) => {
                tracing::debug!(%remote, error = %e, "no more streams");
                return Ok(());
            }
        };
        let Ok(permit) = streams.clone().try_acquire_owned() else {
            tracing::warn!(%remote, "quic peer opened too many streams — dropping one");
            continue;
        };
        let router = router.clone();
        tokio::spawn(async move {
            let _permit = permit;
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

const MAX_STREAMS_PER_PEER: usize = 8;

const SECRET_FILE: &str = "quic-secret";

/// The key friends dial and pin, so it has to outlive the process; a fresh one
/// per start would make every share string a one-time string.
pub fn persistent_secret(data_dir: &Path) -> std::io::Result<SecretKey> {
    let path = data_dir.join(SECRET_FILE);
    if let Ok(text) = std::fs::read_to_string(&path) {
        // Never minted over: a fresh key here would silently break every
        // share string, so a file that cannot be read stays and is reported.
        return hex::decode(text.trim())
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
            .map(|arr| SecretKey::from_bytes(&arr))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "{} is not a 32-byte hex key; restore it from a backup, or delete it \
                         to mint a new one (every share string will change)",
                        path.display()
                    ),
                )
            });
    }
    let secret = SecretKey::generate();
    std::fs::create_dir_all(data_dir)?;
    let tmp = data_dir.join(format!("{SECRET_FILE}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    crate::livekit_bundle::write_private(&tmp, format!("{}\n", hex::encode(secret.to_bytes())))?;
    std::fs::rename(&tmp, &path)?;
    Ok(secret)
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
            livekit: crate::livekit::LiveKitConfig::from_env(&dir),
            operators: Default::default(),
            identities: Default::default(),
            media_max_bytes: crate::media::DEFAULT_MAX_BYTES,
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

    #[test]
    fn the_quic_secret_survives_a_restart_and_is_private_to_its_directory() {
        let dir = std::env::temp_dir().join(format!(
            "dioxusfun-quic-secret-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let first = persistent_secret(&dir).expect("first");
        let second = persistent_secret(&dir).expect("second");
        assert_eq!(
            first.public(),
            second.public(),
            "the same file, the same key"
        );

        let other = std::env::temp_dir().join(format!("{}-other", dir.display()));
        let elsewhere = persistent_secret(&other).expect("other dir");
        assert_ne!(first.public(), elsewhere.public());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(SECRET_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "the secret is readable by its owner only"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn an_unreadable_quic_secret_is_reported_not_replaced() {
        let dir = std::env::temp_dir().join(format!(
            "dioxusfun-quic-secret-corrupt-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SECRET_FILE);
        std::fs::write(&path, "not hex at all\n").unwrap();

        let err = persistent_secret(&dir).expect_err("garbage must not become a key");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "not hex at all\n",
            "the file the operator can still recover from must be left alone"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
