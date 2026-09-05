pub mod archive;
pub mod auth;
pub mod gateway;
pub mod http;
pub mod livekit;
pub mod livekit_bundle;
pub mod media;
pub mod quic;
pub mod sanitize;
pub mod state;
pub mod store;

pub use dioxusfun_protocol as protocol;

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use livekit::LiveKitConfig;

pub struct AppContext {
    pub state: Arc<state::AppState>,
    pub livekit: LiveKitConfig,
    /// Every address a client may have dialed to reach this gateway, as
    /// `protocol::dial_origin` spells them. An Identify naming any other
    /// address is refused before its signature is even checked.
    pub identities: HashSet<String>,
    /// Flipped once, by `GatewayShutdown::close_all`. Every connection task
    /// subscribes. Owned here rather than by the handle so that dropping the
    /// handle — which `build_router` does — closes nothing.
    pub shutdown: Arc<tokio::sync::watch::Sender<bool>>,
}

/// The switch that closes every open gateway socket at once.
///
/// Dropping the listener is not enough: axum spawns a task per connection and
/// `on_upgrade` spawns another, both detached from it, so a stopped host would
/// otherwise go on serving everyone already connected.
pub struct GatewayShutdown(Arc<tokio::sync::watch::Sender<bool>>);

impl GatewayShutdown {
    /// `send_replace`, not `send`: the value has to stick even with nobody
    /// watching yet, or a socket accepted a moment later reads a channel that
    /// still says the server is running.
    pub fn close_all(&self) {
        self.0.send_replace(true);
    }
}

pub struct ServerConfig {
    pub livekit: LiveKitConfig,
    pub operators: HashSet<String>,
    pub data_dir: PathBuf,
    /// Names beyond the loopback and interface addresses, which `spawn_on`
    /// and `serve` add for the port they bind.
    pub identities: HashSet<String>,
    /// Ceiling on the blob directory; `DIOXUSFUN_MEDIA_MAX_BYTES` sets it.
    pub media_max_bytes: u64,
}

/// The addresses this machine answers on for `port`: loopback by every name,
/// and each interface it has. What a LAN friend or a local client dials.
pub fn local_identities(port: u16) -> HashSet<String> {
    let mut out: HashSet<String> = ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .map(|h| format!("{h}:{port}"))
        .collect();
    if let Ok(interfaces) = local_ip_address::list_afinet_netifas() {
        for (_, ip) in interfaces {
            out.insert(match ip {
                std::net::IpAddr::V4(v4) => format!("{v4}:{port}"),
                std::net::IpAddr::V6(v6) => format!("[{v6}]:{port}"),
            });
        }
    }
    out
}

/// `DIOXUSFUN_PUBLIC_HOSTS`: comma-separated `host`, `host:port` or URLs a
/// reverse proxy or DNS name presents this gateway as.
pub fn declared_identities(list: &str, default_port: u16) -> HashSet<String> {
    list.split(',')
        .filter(|s| !s.trim().is_empty())
        .filter_map(|s| protocol::host_origin(s, default_port))
        .collect()
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

async fn build_context(cfg: ServerConfig) -> std::io::Result<(Arc<AppContext>, GatewayShutdown)> {
    let store = store::Store::open_in(&cfg.data_dir)
        .await
        .map_err(|e| std::io::Error::other(format!("store: {e}")))?;
    let media = media::MediaStore::open(cfg.data_dir.join("media"), cfg.media_max_bytes)?;
    let state = state::AppState::load_or_seed(store, media, cfg.operators)
        .await
        .map_err(|e| std::io::Error::other(format!("state load: {e}")))?;
    let shutdown = Arc::new(tokio::sync::watch::Sender::new(false));
    let ctx = Arc::new(AppContext {
        state: Arc::new(state),
        livekit: cfg.livekit,
        identities: cfg.identities,
        shutdown: shutdown.clone(),
    });

    // Its own tick: the retention sweep runs hourly and a voice minute is a
    // minute. Both are cheap when nobody is in a call.
    let voice_xp_state = ctx.state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        tick.tick().await;
        loop {
            tick.tick().await;
            for (guild_id, member) in voice_xp_state.award_voice_minute().await {
                let targets = voice_xp_state.guild_member_pubkeys(guild_id);
                voice_xp_state.deliver(
                    targets,
                    crate::protocol::ServerMessage::MemberUpdate(member),
                );
            }
        }
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

    Ok((ctx, GatewayShutdown(shutdown)))
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
    mut cfg: ServerConfig,
) -> std::io::Result<ServerHandle> {
    if let Ok(addr) = listener.local_addr() {
        cfg.identities.extend(local_identities(addr.port()));
    }
    let router = build_router(cfg).await?;
    Ok(serve_router(listener, router))
}

pub async fn build_router(cfg: ServerConfig) -> std::io::Result<axum::Router> {
    Ok(build_gateway(cfg).await?.0)
}

/// The router and the switch that closes the sockets it went on to serve.
///
/// Anything that can be stopped while people are still connected — the
/// in-process host, above all — wants both halves.
pub async fn build_gateway(cfg: ServerConfig) -> std::io::Result<(axum::Router, GatewayShutdown)> {
    let (ctx, shutdown) = build_context(cfg).await?;
    Ok((http::router(ctx), shutdown))
}

pub fn serve_router(listener: tokio::net::TcpListener, router: axum::Router) -> ServerHandle {
    let addr = listener
        .local_addr()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
    tracing::info!(%addr, "dioxusfun-server listening");

    let task = tokio::spawn(async move {
        let service = router.into_make_service_with_connect_info::<SocketAddr>();
        if let Err(e) = axum::serve(listener, service).await {
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
