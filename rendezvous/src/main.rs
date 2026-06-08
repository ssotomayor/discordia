use std::net::SocketAddr;
use std::sync::Arc;

use dioxusfun_rendezvous::{AppCtx, Config, registry::Registry, router};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // 7700 instead of 7000 because macOS reserves 7000 for AirPlay Receiver.
    let addr: SocketAddr = std::env::var("DIOXUSFUN_RENDEZVOUS_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:7700".into())
        .parse()
        .expect("DIOXUSFUN_RENDEZVOUS_ADDR must be host:port");

    let config = Config {
        livekit_url: std::env::var("LIVEKIT_URL").ok(),
    };
    tracing::info!(?config.livekit_url, "rendezvous configured");

    let ctx = AppCtx {
        registry: Arc::new(Registry::new()),
        config: Arc::new(config),
    };

    let app = router(ctx);
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%addr, %e, "failed to bind");
            std::process::exit(1);
        }
    };
    tracing::info!(%addr, "dioxusfun-rendezvous listening");
    axum::serve(listener, app).await.unwrap();
}
