pub mod archive;
pub mod auth;
pub mod gateway;
pub mod http;
pub mod livekit;
pub mod livekit_bundle;
pub mod media;
pub mod quic;
pub mod state;
pub mod store;

pub use dioxusfun_protocol as protocol;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use livekit::LiveKitConfig;

pub struct AppContext {
    pub state: Arc<state::AppState>,
    pub livekit: LiveKitConfig,
}

pub struct ServerConfig {
    pub livekit: LiveKitConfig,
    pub operators: std::collections::HashSet<String>,
    pub data_dir: PathBuf,
}

pub struct ServerHandle {
    pub addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    pub fn abort(self) {
        self.task.abort();
    }
}

async fn build_context(cfg: ServerConfig) -> std::io::Result<Arc<AppContext>> {
    let store = store::Store::open(&cfg.data_dir.join("discordia.db"))
        .await
        .map_err(|e| std::io::Error::other(format!("store: {e}")))?;
    let media = media::MediaStore::open(cfg.data_dir.join("media"))?;
    let state = state::AppState::load_or_seed(store, media, cfg.operators)
        .await
        .map_err(|e| std::io::Error::other(format!("state load: {e}")))?;
    let ctx = Arc::new(AppContext {
        state: Arc::new(state),
        livekit: cfg.livekit,
    });

    let sweep_state = ctx.state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let deleted = sweep_state.sweep_retention().await;
            if deleted > 0 {
                tracing::info!(deleted, "retention sweep removed expired messages");
            }
            let media = sweep_state.sweep_media().await;
            if media.deleted > 0 {
                tracing::info!(
                    deleted = media.deleted,
                    freed_bytes = media.freed_bytes,
                    kept = media.kept,
                    "media sweep reclaimed unreferenced blobs"
                );
            }
        }
    });

    Ok(ctx)
}

pub async fn serve(addr: SocketAddr, cfg: ServerConfig) -> std::io::Result<()> {
    let ctx = build_context(cfg).await?;
    let app = http::router(ctx);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr().unwrap_or(addr);
    tracing::info!(%bound, "dioxusfun-server listening");
    axum::serve(listener, app).await
}

pub async fn spawn(
    preferred: SocketAddr,
    max_attempts: u16,
    cfg: ServerConfig,
) -> std::io::Result<ServerHandle> {
    let listener = bind_with_fallback(preferred, max_attempts).await?;
    spawn_on(listener, cfg).await
}

pub async fn spawn_on(
    listener: tokio::net::TcpListener,
    cfg: ServerConfig,
) -> std::io::Result<ServerHandle> {
    let router = build_router(cfg).await?;
    Ok(serve_router(listener, router))
}

pub async fn build_router(cfg: ServerConfig) -> std::io::Result<axum::Router> {
    let ctx = build_context(cfg).await?;
    Ok(http::router(ctx))
}

pub fn serve_router(listener: tokio::net::TcpListener, router: axum::Router) -> ServerHandle {
    let addr = listener
        .local_addr()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
    tracing::info!(%addr, "dioxusfun-server listening");

    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "axum serve ended");
        }
    });

    ServerHandle { addr, task }
}

pub async fn bind_with_fallback(
    preferred: SocketAddr,
    max_attempts: u16,
) -> std::io::Result<tokio::net::TcpListener> {
    let mut last_err: Option<std::io::Error> = None;
    let base_port = preferred.port();
    for offset in 0..=max_attempts {
        let mut addr = preferred;
        addr.set_port(base_port.saturating_add(offset));
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => return Ok(l),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::other("no free port found")))
}
