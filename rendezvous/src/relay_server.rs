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
