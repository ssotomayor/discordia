//! The bundled SFU's signaling, served by the gateway under `/sfu`.
//!
//! A caller who can reach the gateway — directly, or through the rendezvous's
//! relay — can then reach the SFU's control WebSocket even when none of the
//! host's addresses are routable from where they stand (an IPv4-only caller
//! and an IPv6-only host, say). Media still travels by ICE, relayed by TURN
//! when it must; only the signaling rides here, and the SFU still checks the
//! token in it, so the proxy adds no trust.

use std::sync::Arc;

use axum::extract::ws::{
    CloseFrame as AxumClose, Message as AxumMessage, WebSocket, WebSocketUpgrade,
};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as UpstreamClose;

use crate::AppContext;

/// Every room of a call is one tunnel, so a dozen callers with screens shared
/// stay well inside this; beyond it the gateway refuses rather than queue.
const MAX_TUNNELS: usize = 128;

static TUNNELS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(MAX_TUNNELS)));

/// Enough for LiveKit's validate answer, which is a sentence.
const MAX_PROXIED_BODY: usize = 64 * 1024;

pub async fn proxy(
    ws: Option<WebSocketUpgrade>,
    State(ctx): State<Arc<AppContext>>,
    Path(rest): Path<String>,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    let (cfg, _) = ctx.livekit.route();
    if cfg.explicit_url.is_some() {
        return (
            StatusCode::NOT_FOUND,
            "this server runs no local voice server",
        )
            .into_response();
    }
    let authority = format!("127.0.0.1:{}", cfg.port);
    let path_and_query = match uri.query() {
        Some(q) => format!("/{rest}?{q}"),
        None => format!("/{rest}"),
    };
    match ws {
        Some(ws) => {
            let Ok(permit) = TUNNELS.clone().try_acquire_owned() else {
                tracing::warn!("voice route: signaling tunnel refused, too many open");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "too many voice connections",
                )
                    .into_response();
            };
            let upstream =
                match tokio_tungstenite::connect_async(format!("ws://{authority}{path_and_query}"))
                    .await
                {
                    Ok((stream, _)) => stream,
                    // The SFU refused the handshake, a bad token above all: hand
                    // the caller its verdict, not a gateway error.
                    Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                        let status = StatusCode::from_u16(response.status().as_u16())
                            .unwrap_or(StatusCode::BAD_GATEWAY);
                        tracing::info!(%status, "voice route: SFU refused a tunneled handshake");
                        return status.into_response();
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "voice route: SFU unreachable for a tunnel");
                        return (StatusCode::BAD_GATEWAY, "the voice server is not answering")
                            .into_response();
                    }
                };
            tracing::info!(path = %rest, "voice route: signaling tunnel opened");
            ws.on_upgrade(move |socket| async move {
                let _permit = permit;
                pipe(socket, upstream).await;
                tracing::info!("voice route: signaling tunnel closed");
            })
            .into_response()
        }
        None => forward_get(&authority, &path_and_query, &headers).await,
    }
}

async fn pipe(
    client: WebSocket,
    upstream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let (mut client_tx, mut client_rx) = client.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    let to_upstream = async {
        while let Some(Ok(message)) = client_rx.next().await {
            let Some(message) = to_upstream(message) else {
                continue;
            };
            if upstream_tx.send(message).await.is_err() {
                break;
            }
        }
    };
    let to_client = async {
        while let Some(Ok(message)) = upstream_rx.next().await {
            let Some(message) = to_client(message) else {
                continue;
            };
            if client_tx.send(message).await.is_err() {
                break;
            }
        }
    };
    tokio::select! {
        _ = to_upstream => {}
        _ = to_client => {}
    }
}

fn to_upstream(message: AxumMessage) -> Option<UpstreamMessage> {
    Some(match message {
        AxumMessage::Text(t) => UpstreamMessage::Text(t),
        AxumMessage::Binary(b) => UpstreamMessage::Binary(b),
        AxumMessage::Ping(p) => UpstreamMessage::Ping(p),
        AxumMessage::Pong(p) => UpstreamMessage::Pong(p),
        AxumMessage::Close(frame) => UpstreamMessage::Close(frame.map(|f| UpstreamClose {
            code: f.code.into(),
            reason: f.reason,
        })),
    })
}

fn to_client(message: UpstreamMessage) -> Option<AxumMessage> {
    Some(match message {
        UpstreamMessage::Text(t) => AxumMessage::Text(t),
        UpstreamMessage::Binary(b) => AxumMessage::Binary(b),
        UpstreamMessage::Ping(p) => AxumMessage::Ping(p),
        UpstreamMessage::Pong(p) => AxumMessage::Pong(p),
        UpstreamMessage::Close(frame) => AxumMessage::Close(frame.map(|f| AxumClose {
            code: f.code.into(),
            reason: f.reason,
        })),
        UpstreamMessage::Frame(_) => return None,
    })
}

/// The SDK's `rtc/validate` probe: a plain GET it makes after a failed
/// handshake to learn why. Only the status and a short body come back.
async fn forward_get(authority: &str, path_and_query: &str, headers: &HeaderMap) -> Response {
    let stream = match tokio::net::TcpStream::connect(authority).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "voice route: SFU unreachable for a validate probe");
            return (StatusCode::BAD_GATEWAY, "the voice server is not answering").into_response();
        }
    };
    let (mut sender, connection) =
        match hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream)).await {
            Ok(pair) => pair,
            Err(e) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("voice server handshake: {e}"),
                )
                    .into_response();
            }
        };
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let mut request = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(path_and_query)
        .header(header::HOST, authority);
    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        request = request.header(header::AUTHORIZATION, auth);
    }
    let Ok(request) = request.body(axum::body::Body::empty()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let response = match sender.send_request(request).await {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_GATEWAY, format!("voice server: {e}")).into_response();
        }
    };
    let (parts, body) = response.into_parts();
    let body = axum::body::to_bytes(axum::body::Body::new(body), MAX_PROXIED_BODY)
        .await
        .unwrap_or_default();
    (parts.status, body).into_response()
}
