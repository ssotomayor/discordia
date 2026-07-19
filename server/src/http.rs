use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::Request;
use axum::middleware::{self, Next};
use axum::response::IntoResponse;
use axum::routing::get;
use tower_http::cors::{Any, CorsLayer};

use crate::AppContext;
use crate::gateway;

pub fn router(ctx: Arc<AppContext>) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    Router::new()
        .route("/", get(root))
        .route("/gateway", get(gateway_upgrade))
        .route("/media/{name}", get(serve_media))
        .with_state(ctx)
        .layer(middleware::from_fn(log_request))
        .layer(cors)
}

/// Serve a content-addressed media blob (`/media/<sha256>.<ext>`). Immutable
/// by construction, so cache hard.
async fn serve_media(
    axum::extract::Path(name): axum::extract::Path<String>,
    State(ctx): State<Arc<AppContext>>,
) -> axum::response::Response {
    use axum::http::{StatusCode, header};
    match ctx.state.media.read(&name) {
        Some((bytes, mime)) => (
            [
                (header::CONTENT_TYPE, mime.to_string()),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn log_request(req: Request<axum::body::Body>, next: Next) -> impl IntoResponse {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let upgrade = req
        .headers()
        .get("upgrade")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("-")
        .to_string();
    let response = next.run(req).await;
    tracing::info!(%method, %path, %upgrade, status = %response.status(), "request");
    response
}

async fn root() -> &'static str {
    "dioxusfun-server. Connect a WebSocket to /gateway."
}

async fn gateway_upgrade(
    ws: WebSocketUpgrade,
    State(ctx): State<Arc<AppContext>>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    // Capture the host the client used to reach us so we can hand back a
    // matching LiveKit URL when they JoinVoice.
    let client_host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    tracing::info!(?client_host, "gateway upgrade requested");
    ws.on_upgrade(move |socket| gateway::handle_connection(socket, ctx, client_host))
}
