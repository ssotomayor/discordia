//! Library entry point for embedding the gateway in another process
//! (i.e. the Dioxus client's self-host mode).
//!
//! The thin `bin/dioxusfun-server` shim in `src/main.rs` is just a wrapper
//! that wires logging + env config and calls [`serve`].

pub mod archive;
pub mod auth;
pub mod gateway;
pub mod http;
pub mod livekit;
pub mod livekit_bundle;
pub mod media;
pub mod state;
pub mod store;

/// Wire protocol — re-exported from the shared `dioxusfun-protocol` crate so
/// `crate::protocol::…` paths throughout the server keep working unchanged.
pub use dioxusfun_protocol as protocol;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use livekit::LiveKitConfig;

pub struct AppContext {
    pub state: Arc<state::AppState>,
    pub livekit: LiveKitConfig,
}

/// Everything a gateway instance needs besides its bind address. One struct so
/// embedded self-host, the standalone binary, and tests all configure the same
/// way (see docs/ROADMAP.md "run-modes").
pub struct ServerConfig {
    pub livekit: LiveKitConfig,
    /// Pubkeys treated as owners of system guilds (the seeded Lobby).
    pub operators: std::collections::HashSet<String>,
    /// Root for durable data: `<data_dir>/discordia.db` + `<data_dir>/media/`.
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

/// Build the shared context: open the store + media dir, rehydrate (or seed)
/// state, and start the hourly retention sweep.
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

    // Hourly retention sweep (runs once shortly after boot, too).
    let sweep_state = ctx.state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            let deleted = sweep_state.sweep_retention().await;
            if deleted > 0 {
                tracing::info!(deleted, "retention sweep removed expired messages");
            }
        }
    });

    Ok(ctx)
}

/// Bind + serve in the foreground until the underlying axum server exits.
pub async fn serve(addr: SocketAddr, cfg: ServerConfig) -> std::io::Result<()> {
    let ctx = build_context(cfg).await?;
    let app = http::router(ctx);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr().unwrap_or(addr);
    tracing::info!(%bound, "dioxusfun-server listening");
    axum::serve(listener, app).await
}

/// Bind on the first free port in `preferred..=preferred+max_attempts` and
/// spawn the server as a background task. Returns the bound address so the
/// caller knows which port we actually got.
pub async fn spawn(
    preferred: SocketAddr,
    max_attempts: u16,
    cfg: ServerConfig,
) -> std::io::Result<ServerHandle> {
    let listener = bind_with_fallback(preferred, max_attempts).await?;
    spawn_on(listener, cfg).await
}

/// Serve on a listener the caller already bound.
///
/// Self-host needs the port before the gateway starts: it advertises a
/// port-mapped address to the rendezvous at registration time, and a mapping
/// has to name the port the gateway actually got — which, with the fallback
/// above, is not necessarily the one asked for. Binding first and serving
/// second is what makes that answerable in the right order.
pub async fn spawn_on(
    listener: tokio::net::TcpListener,
    cfg: ServerConfig,
) -> std::io::Result<ServerHandle> {
    let addr = listener.local_addr()?;
    let ctx = build_context(cfg).await?;
    tracing::info!(%addr, "dioxusfun-server listening");
    let app = http::router(ctx);

    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "axum serve ended");
        }
    });

    Ok(ServerHandle { addr, task })
}

/// Bind the first free port in `preferred..=preferred+max_attempts`.
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
