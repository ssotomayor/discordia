//! A loopback door to the host's SFU signaling, carried by the gateway
//! connection. The LiveKit SDK dials `ws://127.0.0.1:<port>/sfu`; every TCP
//! connection it opens becomes one stream to the gateway, which serves
//! `/sfu/*` by piping to its bundled SFU. Only signaling: media is ICE's.

use std::net::Ipv4Addr;
use std::sync::Arc;

use futures_util::future::BoxFuture;

pub trait Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> Stream for T {}

/// Opens one fresh byte stream to the gateway per accepted connection.
pub type Opener =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Box<dyn Stream>, String>> + Send + Sync>;

pub struct SfuTunnel {
    url: String,
    accept: Option<tokio::task::JoinHandle<()>>,
}

impl SfuTunnel {
    pub async fn open(opener: Opener) -> Result<Self, String> {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|e| format!("tunnel listener: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("tunnel listener address: {e}"))?
            .port();
        let accept = tokio::spawn(async move {
            // Children live in the set so that aborting this task takes them down too.
            let mut links = tokio::task::JoinSet::new();
            loop {
                let Ok((mut near, _)) = listener.accept().await else {
                    break;
                };
                while links.try_join_next().is_some() {}
                let opener = opener.clone();
                links.spawn(async move {
                    match opener().await {
                        Ok(mut far) => {
                            let _ = tokio::io::copy_bidirectional(&mut near, &mut far).await;
                        }
                        Err(error) => {
                            tracing::warn!(%error, "voice route: tunnel stream to the gateway failed");
                        }
                    }
                });
            }
        });
        Ok(Self {
            url: format!("ws://127.0.0.1:{port}/sfu"),
            accept: Some(accept),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Dropping only asks; the port is free once the aborted task has run its drop.
    pub async fn close(mut self) {
        if let Some(accept) = self.accept.take() {
            accept.abort();
            let _ = accept.await;
        }
    }
}

impl Drop for SfuTunnel {
    fn drop(&mut self) {
        if let Some(accept) = &self.accept {
            accept.abort();
        }
    }
}

pub fn quic_opener(conn: iroh::endpoint::Connection) -> Opener {
    Arc::new(move || {
        let conn = conn.clone();
        Box::pin(async move {
            let (send, recv) = conn
                .open_bi()
                .await
                .map_err(|e| format!("quic stream: {e}"))?;
            Ok(Box::new(tokio::io::join(recv, send)) as Box<dyn Stream>)
        })
    })
}

/// Over plain WebSocket the gateway is a URL, so its `/sfu` is one too.
pub fn direct_url(gateway_url: &str) -> String {
    let base = gateway_url.trim_end_matches('/');
    let base = base.strip_suffix("/gateway").unwrap_or(base);
    format!("{base}/sfu")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn echo_opener() -> Opener {
        Arc::new(|| {
            Box::pin(async {
                let (near, mut far) = tokio::io::duplex(4096);
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = far.read(&mut buf).await {
                        if n == 0 || far.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
                Ok(Box::new(near) as Box<dyn Stream>)
            })
        })
    }

    #[tokio::test]
    async fn each_connection_gets_its_own_stream_and_bytes_cross_both_ways() {
        let tunnel = SfuTunnel::open(echo_opener()).await.unwrap();
        let port: u16 = tunnel
            .url()
            .trim_start_matches("ws://127.0.0.1:")
            .trim_end_matches("/sfu")
            .parse()
            .unwrap();
        for payload in [
            b"GET /sfu/rtc HTTP/1.1\r\n".as_slice(),
            b"second".as_slice(),
        ] {
            let mut tcp = tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            tcp.write_all(payload).await.unwrap();
            let mut back = vec![0u8; payload.len()];
            tcp.read_exact(&mut back).await.unwrap();
            assert_eq!(back, payload);
        }
    }

    #[tokio::test]
    async fn dropping_the_tunnel_closes_the_door() {
        let tunnel = SfuTunnel::open(echo_opener()).await.unwrap();
        let port: u16 = tunnel
            .url()
            .trim_start_matches("ws://127.0.0.1:")
            .trim_end_matches("/sfu")
            .parse()
            .unwrap();
        tunnel.close().await;
        let refused = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)),
        )
        .await;
        assert!(matches!(refused, Ok(Err(_))), "{refused:?}");
    }

    #[test]
    fn the_direct_url_replaces_the_gateway_path() {
        assert_eq!(
            direct_url("ws://127.0.0.1:9000/gateway"),
            "ws://127.0.0.1:9000/sfu"
        );
        assert_eq!(direct_url("wss://app.example/"), "wss://app.example/sfu");
    }
}
