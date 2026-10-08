use std::net::SocketAddr;
use std::sync::Arc;

use dioxusfun_rendezvous::{AppCtx, Config, registry::Registry, router, turn_relay};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let addr: SocketAddr = std::env::var("DIOXUSFUN_RENDEZVOUS_ADDR")
        // 7700, not 7000: macOS reserves 7000 for AirPlay Receiver.
        .unwrap_or_else(|_| "0.0.0.0:7700".into())
        .parse()
        .expect("DIOXUSFUN_RENDEZVOUS_ADDR must be host:port");

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
    let _relay = dioxusfun_rendezvous::relay_server::spawn(relay_bind, relay_url.clone()).await;

    let (turn, _turn_server) = match std::env::var("DIOXUSFUN_RENDEZVOUS_TURN_URL").ok() {
        Some(url) => {
            let bind: SocketAddr = std::env::var("DIOXUSFUN_RENDEZVOUS_TURN_ADDR")
                .unwrap_or_else(|_| format!("0.0.0.0:{}", turn_relay::DEFAULT_PORT))
                .parse()
                .expect("DIOXUSFUN_RENDEZVOUS_TURN_ADDR must be host:port");
            let relay_ip = match std::env::var("DIOXUSFUN_RENDEZVOUS_TURN_RELAY_IP").ok() {
                Some(ip) => ip
                    .parse()
                    .expect("DIOXUSFUN_RENDEZVOUS_TURN_RELAY_IP must be an IP address"),
                None => turn_relay::resolve_relay_ip(&url).await.expect(
                    "DIOXUSFUN_RENDEZVOUS_TURN_URL must name this machine's public address",
                ),
            };
            // Credentials are minted per registration and renewed over the control
            // stream, so a secret that changes with the process costs nothing.
            let secret = std::env::var("DIOXUSFUN_RENDEZVOUS_TURN_SECRET")
                .unwrap_or_else(|_| hex::encode(rand::random::<[u8; 32]>()));
            let ports = match std::env::var("DIOXUSFUN_RENDEZVOUS_TURN_PORTS") {
                Ok(spec) => {
                    turn_relay::parse_ports(&spec).expect("DIOXUSFUN_RENDEZVOUS_TURN_PORTS")
                }
                Err(_) => turn_relay::DEFAULT_PORTS,
            };
            let issuer = turn_relay::Issuer::new(vec![url], secret);
            match turn_relay::spawn(bind, relay_ip, ports, &issuer).await {
                Ok(server) => (Some(issuer), Some(server)),
                Err(e) => {
                    tracing::error!(%e, "turn relay not started; hosts behind NAT fall back to the shared SFU");
                    (None, None)
                }
            }
        }
        None => (None, None),
    };

    let config = Config {
        relay_url,
        livekit_url: std::env::var("LIVEKIT_URL").ok(),
        livekit_api_key: std::env::var("LIVEKIT_API_KEY").ok(),
        livekit_api_secret: std::env::var("LIVEKIT_API_SECRET").ok(),
        turn,
        ..Config::default()
    };
    tracing::info!(
        ?config.relay_url,
        ?config.livekit_url,
        shared_credentials = config.livekit_api_secret.is_some(),
        turn = config.turn.as_ref().map(|t| t.urls().to_vec()).unwrap_or_default().join(","),
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
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}
