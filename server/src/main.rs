use std::net::SocketAddr;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let addr: SocketAddr = std::env::var("DIOXUSFUN_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:9000".into())
        .parse()
        .expect("DIOXUSFUN_ADDR must be host:port");

    // Auto-spawn the bundled LiveKit unless the operator explicitly points
    // us at an external instance (e.g. LiveKit Cloud) via env.
    let want_autospawn = std::env::var("DIOXUSFUN_LIVEKIT_AUTOSPAWN")
        .map(|v| !matches!(v.as_str(), "0" | "false" | "no"))
        .unwrap_or(true);
    let livekit_present = std::env::var("LIVEKIT_URL").is_ok();

    let _livekit_handle = if want_autospawn && !livekit_present {
        match dioxusfun_server::livekit_bundle::spawn_livekit().await {
            Ok(child) => {
                tracing::info!("bundled livekit-server started on port 7880");
                Some(child)
            }
            Err(e) => {
                tracing::warn!(error = %e, "livekit subprocess not started — voice will be unavailable unless you set LIVEKIT_URL");
                None
            }
        }
    } else {
        if !want_autospawn {
            tracing::info!("DIOXUSFUN_LIVEKIT_AUTOSPAWN=0, not spawning bundled livekit");
        } else {
            tracing::info!("LIVEKIT_URL set, assuming external livekit instance");
        }
        None
    };

    let livekit_cfg = dioxusfun_server::livekit::LiveKitConfig::from_env();
    tracing::info!(
        explicit_url = ?livekit_cfg.explicit_url,
        port = livekit_cfg.port,
        "livekit configured (URLs handed to clients are derived per-connection unless explicit_url is set)"
    );

    let serve_fut = dioxusfun_server::serve(addr, livekit_cfg);
    tokio::select! {
        result = serve_fut => {
            if let Err(e) = result {
                tracing::error!(error = %e, "server exited with error");
                std::process::exit(1);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("ctrl+c — shutting down");
        }
    }
    // _livekit_handle drops here, killing the subprocess.
}
