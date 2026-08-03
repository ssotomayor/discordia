pub mod protocol;
pub mod registry;
pub mod relay;
pub mod shortcode;
pub mod verify;

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
    /// Credentials for the shared LiveKit, handed to registering hosts so the
    /// tokens they mint are accepted by it.
    pub livekit_api_key: Option<String>,
    pub livekit_api_secret: Option<String>,
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
        .route("/voice-token", axum::routing::post(voice_token))
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

#[derive(serde::Deserialize)]
struct VoiceTokenRequest {
    /// Grant issued to this host at registration (see `issue_voice_grant`).
    grant: String,
    /// Room the host wants a token for. We namespace it per host, so two
    /// hosts asking for the same name get different rooms.
    room: String,
    identity: String,
    #[serde(default)]
    name: String,
}

#[derive(serde::Serialize)]
struct VoiceTokenResponse {
    token: String,
}

/// Mint a LiveKit token on a host's behalf.
///
/// This exists so hosts never hold the shared SFU's signing secret. On a public
/// relay that would let any host mint tokens into any other host's rooms — and
/// tokens are the only thing standing between a stranger and your voice channel.
/// Instead the host presents its session grant, and we sign a token scoped to a
/// room namespaced with that host's shortcode.
async fn voice_token(
    State(ctx): State<AppCtx>,
    Json(req): Json<VoiceTokenRequest>,
) -> Result<Json<VoiceTokenResponse>, (axum::http::StatusCode, String)> {
    use axum::http::StatusCode;
    use livekit_api::access_token::{AccessToken, VideoGrants};

    let Some(shortcode) = ctx.registry.voice_grant_owner(&req.grant) else {
        return Err((StatusCode::UNAUTHORIZED, "unknown or expired grant".into()));
    };
    let (Some(key), Some(secret)) = (&ctx.config.livekit_api_key, &ctx.config.livekit_api_secret)
    else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "this rendezvous has no shared LiveKit".into(),
        ));
    };
    // Namespacing is the security boundary: a host can only ever reach rooms
    // under its own shortcode, whatever it asks for.
    let room = format!("{shortcode}--{}", req.room);
    let token = AccessToken::with_api_key(key, secret)
        .with_identity(&req.identity)
        .with_name(&req.name)
        .with_grants(VideoGrants {
            room_join: true,
            room,
            can_publish: true,
            can_subscribe: true,
            can_publish_data: true,
            ..Default::default()
        })
        .to_jwt()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("mint: {e}")))?;
    Ok(Json(VoiceTokenResponse { token }))
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
