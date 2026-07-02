//! Library entry point for embedding the gateway in another process
//! (i.e. the Dioxus client's self-host mode).
//!
//! The thin `bin/dioxusfun-server` shim in `src/main.rs` is just a wrapper
//! that wires logging + env config and calls [`serve`].

pub mod auth;
pub mod gateway;
pub mod http;
pub mod livekit;
pub mod livekit_bundle;
pub mod state;

/// Wire protocol — re-exported from the shared `dioxusfun-protocol` crate so
/// `crate::protocol::…` paths throughout the server keep working unchanged.
pub use dioxusfun_protocol as protocol;

use std::net::SocketAddr;
use std::sync::Arc;

use livekit::LiveKitConfig;

pub struct AppContext {
    pub state: Arc<state::AppState>,
    pub livekit: LiveKitConfig,
}

pub struct ServerHandle {
    pub addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    pub fn abort(self) {
        self.task.abort();
    }

    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

/// Bind + serve in the foreground until the underlying axum server exits.
pub async fn serve(addr: SocketAddr, livekit_cfg: LiveKitConfig) -> std::io::Result<()> {
    let ctx = Arc::new(AppContext {
        state: Arc::new(state::AppState::seeded()),
        livekit: livekit_cfg,
    });
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
    livekit_cfg: LiveKitConfig,
) -> std::io::Result<ServerHandle> {
    let ctx = Arc::new(AppContext {
        state: Arc::new(state::AppState::seeded()),
        livekit: livekit_cfg,
    });

    let listener = bind_with_fallback(preferred, max_attempts).await?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, "dioxusfun-server listening");
    let app = http::router(ctx);

    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "axum serve ended");
        }
    });

    Ok(ServerHandle { addr, task })
}

async fn bind_with_fallback(
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
