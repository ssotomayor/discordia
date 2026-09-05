use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, State};
use axum::http::{Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::AppContext;
use crate::gateway;

/// No `/media` route on purpose: blobs travel over the identified socket
/// (`FetchEmoji`), so the server is not an anonymous file host by URL. No CORS
/// layer either: nothing here is for a browser, and `gateway_upgrade` says so.
pub fn router(ctx: Arc<AppContext>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/gateway", get(gateway_upgrade))
        .with_state(ctx)
        .layer(middleware::from_fn(log_request))
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
    tracing::info!(?method, ?path, ?upgrade, status = %response.status(), "request");
    response
}

async fn root() -> &'static str {
    "dioxusfun-server. Connect a WebSocket to /gateway."
}

/// A browser always sends `Origin` and the native client never does, so the
/// header alone marks a web page reaching for a gateway on someone's machine
/// or LAN — which CORS would not stop, since it does not cover WebSockets.
///
/// `peer` is absent over QUIC, where the stream has no TCP address; the
/// per-address cap then does not apply and the global one still does.
async fn gateway_upgrade(
    ws: WebSocketUpgrade,
    State(ctx): State<Arc<AppContext>>,
    headers: axum::http::HeaderMap,
    peer: Option<ConnectInfo<SocketAddr>>,
) -> Response {
    if headers.contains_key(header::ORIGIN) {
        return (
            StatusCode::FORBIDDEN,
            "this gateway does not accept connections from web pages",
        )
            .into_response();
    }
    let client_host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let peer_ip = peer.map(|ConnectInfo(addr)| addr.ip());
    tracing::info!(?client_host, ?peer_ip, "gateway upgrade requested");
    ws.max_message_size(gateway::MAX_FRAME_BYTES)
        .max_frame_size(gateway::MAX_FRAME_BYTES)
        .on_upgrade(move |socket| gateway::handle_connection(socket, ctx, client_host, peer_ip))
        .into_response()
}
