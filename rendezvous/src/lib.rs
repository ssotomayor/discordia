pub mod protocol;
pub mod registry;
pub mod relay;
pub mod shortcode;

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::response::IntoResponse;
use axum::routing::get;
use tower_http::cors::{Any, CorsLayer};

use crate::protocol::DiscoverEntry;
use crate::registry::Registry;

#[derive(Clone)]
pub struct Config {
    pub livekit_url: Option<String>,
}

#[derive(Clone)]
pub struct AppCtx {
    pub registry: Arc<Registry>,
    pub config: Arc<Config>,
}

pub fn router(ctx: AppCtx) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);
    Router::new()
        .route("/", get(root))
        .route("/discover", get(discover))
        .route("/control", get(control))
        .route("/join/:code", get(join))
        .route("/proxy/:session", get(proxy))
        .with_state(ctx)
        .layer(cors)
}

async fn root() -> &'static str {
    "dioxusfun-rendezvous. Endpoints: /discover, /control, /join/:code, /proxy/:session"
}

async fn discover(State(ctx): State<AppCtx>) -> Json<Vec<DiscoverEntry>> {
    Json(ctx.registry.discover())
}

async fn control(ws: WebSocketUpgrade, State(ctx): State<AppCtx>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| relay::handle_host_control(socket, ctx.registry, ctx.config))
}

async fn join(
    ws: WebSocketUpgrade,
    Path(code): Path<String>,
    State(ctx): State<AppCtx>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| relay::handle_friend_join(socket, ctx.registry, code))
}

async fn proxy(
    ws: WebSocketUpgrade,
    Path(session): Path<String>,
    State(ctx): State<AppCtx>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| relay::handle_host_proxy(socket, ctx.registry, session))
}
