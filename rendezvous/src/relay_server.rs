use std::net::SocketAddr;

use iroh_relay::server::{RelayConfig, Server, ServerConfig};

pub const DEFAULT_RELAY_PORT: u16 = 7701;

pub struct RelayHandle {
    pub url: String,
    _server: Server,
}

pub async fn spawn(bind: SocketAddr, public_url: Option<String>) -> Option<RelayHandle> {
    let url = public_url?;

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
            tracing::error!(error = %e, "could not start the iroh relay — hole punching will be unavailable");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::endpoint::presets;
    use iroh::{Endpoint, EndpointAddr, RelayMap, RelayMode, TransportAddr};

    async fn endpoint(url: &str) -> Endpoint {
        let url: iroh::RelayUrl = url.parse().unwrap();
        Endpoint::builder(presets::Minimal)
            .alpns(vec![b"discordia-relay-test".to_vec()])
            .relay_mode(RelayMode::Custom(RelayMap::from_iter([url])))
            .clear_ip_transports()
            .bind()
            .await
            .unwrap()
    }

    async fn exchange(conn: &iroh::endpoint::Connection, payload: &[u8]) {
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        send.write_all(payload).await.unwrap();
        send.finish().unwrap();
        assert_eq!(recv.read_to_end(1024).await.unwrap(), payload);
    }

    #[tokio::test]
    async fn relay_only_streams_resume_after_the_relay_restarts() {
        tokio::time::timeout(std::time::Duration::from_secs(45), async {
            let relay = spawn(
                "127.0.0.1:0".parse().unwrap(),
                Some("http://localhost/".into()),
            )
            .await
            .unwrap();
            let bind = relay._server.http_addr().unwrap();
            let url = format!("http://{bind}/");
            let server = endpoint(&url).await;
            let client = endpoint(&url).await;
            server.online().await;
            client.online().await;
            let accept = server.clone();
            let echo = tokio::spawn(async move {
                let conn = accept.accept().await.unwrap().await.unwrap();
                for _ in 0..2 {
                    let (mut send, mut recv) = conn.accept_bi().await.unwrap();
                    let payload = recv.read_to_end(1024).await.unwrap();
                    send.write_all(&payload).await.unwrap();
                    send.finish().unwrap();
                }
                conn.closed().await;
            });
            let addr = EndpointAddr::new(server.id())
                .with_addrs([TransportAddr::Relay(url.parse().unwrap())]);
            let conn = client.connect(addr, b"discordia-relay-test").await.unwrap();
            exchange(&conn, b"before restart").await;
            assert!(
                conn.paths()
                    .iter()
                    .any(|path| path.is_selected() && path.is_relay())
            );

            relay._server.shutdown().await.unwrap();
            let restarted = spawn(bind, Some(url)).await.unwrap();
            exchange(&conn, b"after restart").await;
            conn.close(0_u32.into(), b"test finished");
            echo.await.unwrap();
            client.close().await;
            server.close().await;
            restarted._server.shutdown().await.unwrap();
        })
        .await
        .expect("relay restart did not recover within 45 seconds");
    }
}
