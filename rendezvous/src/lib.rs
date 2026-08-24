pub mod registry;
pub mod relay;
pub mod relay_server;
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
use dioxusfun_protocol::rendezvous::DiscoverEntry;
use tower_http::cors::{Any, CorsLayer};

use crate::registry::Registry;

#[derive(Clone)]
pub struct Config {
    pub livekit_url: Option<String>,
    pub livekit_api_key: Option<String>,
    pub livekit_api_secret: Option<String>,
    pub heartbeat_interval: std::time::Duration,
    pub host_timeout: std::time::Duration,
    pub relay_url: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            livekit_url: None,
            livekit_api_key: None,
            livekit_api_secret: None,
            relay_url: None,
            heartbeat_interval: std::time::Duration::from_secs(20),
            host_timeout: std::time::Duration::from_secs(60),
        }
    }
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
        .route("/resolve/:code", get(resolve))
        .route("/config", get(config))
        .route("/voice-token", axum::routing::post(voice_token))
        .route("/control", get(control))
        .route("/join/:code", get(join))
        .route("/proxy/:session", get(proxy))
        .with_state(ctx)
        .layer(cors)
}

async fn root() -> &'static str {
    "dioxusfun-rendezvous. Endpoints: /config, /discover, /resolve/:code, /control, /join/:code, /proxy/:session"
}

async fn config(State(ctx): State<AppCtx>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "relay_url": ctx.config.relay_url,
        "livekit_url": ctx.config.livekit_url,
    }))
}

async fn discover(State(ctx): State<AppCtx>) -> Json<Vec<DiscoverEntry>> {
    let mut entries = ctx.registry.discover();
    for e in &mut entries {
        e.relay_url = ctx.config.relay_url.clone();
    }
    Json(entries)
}

async fn resolve(
    State(ctx): State<AppCtx>,
    Path(code): Path<String>,
) -> Result<Json<DiscoverEntry>, axum::http::StatusCode> {
    ctx.registry
        .lookup(&code.to_lowercase())
        .map(|mut entry| {
            entry.relay_url = ctx.config.relay_url.clone();
            Json(entry)
        })
        .ok_or(axum::http::StatusCode::NOT_FOUND)
}

#[derive(serde::Deserialize)]
struct VoiceTokenRequest {
    grant: String,
    room: String,
    identity: String,
    #[serde(default)]
    name: String,
    #[serde(default = "publish_by_default")]
    can_publish: bool,
}

fn publish_by_default() -> bool {
    true
}

#[derive(serde::Serialize)]
struct VoiceTokenResponse {
    token: String,
}

async fn voice_token(
    State(ctx): State<AppCtx>,
    Json(req): Json<VoiceTokenRequest>,
) -> Result<Json<VoiceTokenResponse>, (axum::http::StatusCode, String)> {
    use axum::http::StatusCode;
    use livekit_api::access_token::AccessToken;

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
    let room = format!("{shortcode}--{}", req.room);
    let token = AccessToken::with_api_key(key, secret)
        .with_identity(&req.identity)
        .with_name(&req.name)
        .with_grants(grants_for(room, req.can_publish))
        .to_jwt()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("mint: {e}")))?;
    Ok(Json(VoiceTokenResponse { token }))
}

/// Minting here is what keeps hosts from ever holding the shared SFU's
/// signing secret.
fn grants_for(room: String, can_publish: bool) -> livekit_api::access_token::VideoGrants {
    livekit_api::access_token::VideoGrants {
        room_join: true,
        room,
        can_publish,
        can_subscribe: true,
        can_publish_data: can_publish,
        ..Default::default()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subscribe_only_request_gets_no_publish_rights() {
        let g = grants_for("code--screen-1".into(), false);
        assert!(!g.can_publish);
        assert!(!g.can_publish_data, "the data channel is publishing too");
        assert!(g.can_subscribe, "it still has to hear the stream");
        assert!(g.room_join);

        let g = grants_for("code--voice-1".into(), true);
        assert!(g.can_publish);
        assert!(g.can_publish_data);
    }

    #[test]
    fn a_request_without_the_field_still_gets_publish() {
        let req: VoiceTokenRequest = serde_json::from_str(
            r#"{"grant":"g","room":"voice-1","identity":"abc","name":"someone"}"#,
        )
        .expect("an older host's body must still parse");
        assert!(req.can_publish);
    }

    #[test]
    fn an_explicit_false_survives_deserialization() {
        let req: VoiceTokenRequest = serde_json::from_str(
            r#"{"grant":"g","room":"screen-1","identity":"abc#audio","name":"x","can_publish":false}"#,
        )
        .unwrap();
        assert!(!req.can_publish);
    }
}
