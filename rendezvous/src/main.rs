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

    // Set to the client-dialable URL, not the bind address (which is usually
    // wildcard).
    // Unset means no relay coordination; users fall back to the WebSocket
    // proxy rather than a public relay.
    let relay_url = std::env::var("DIOXUSFUN_RENDEZVOUS_RELAY_URL").ok();
    let relay_bind: SocketAddr = std::env::var("DIOXUSFUN_RENDEZVOUS_RELAY_ADDR")
        .unwrap_or_else(|_| {
            format!(
                "0.0.0.0:{}",
                dioxusfun_rendezvous::relay_server::DEFAULT_RELAY_PORT
            )
        })
        .parse()
        .expect("DIOXUSFUN_RENDEZVOUS_RELAY_ADDR must be host:port");
    // Held for the process lifetime; dropping it stops the relay.
    let _relay = dioxusfun_rendezvous::relay_server::spawn(relay_bind, relay_url.clone()).await;

    let config = Config {
        relay_url,
        livekit_url: std::env::var("LIVEKIT_URL").ok(),
        // Required for hosts to mint voice tokens valid against the shared
        // SFU; without them, clients get "token signature is invalid".
        livekit_api_key: std::env::var("LIVEKIT_API_KEY").ok(),
        livekit_api_secret: std::env::var("LIVEKIT_API_SECRET").ok(),
        ..Config::default()
    };
    tracing::info!(
        ?config.relay_url,
        ?config.livekit_url,
        shared_credentials = config.livekit_api_secret.is_some(),
        "rendezvous configured"
    );

    let data_dir: std::path::PathBuf = std::env::var("DIOXUSFUN_RENDEZVOUS_DATA_DIR")
        .unwrap_or_else(|_| "./rendezvous-data".into())
        .into();
    let reservations_path = data_dir.join("reservations.json");
    tracing::info!(path = %reservations_path.display(), "reservations persistence");

    let ctx = AppCtx {
        registry: Arc::new(Registry::load(reservations_path)),
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
