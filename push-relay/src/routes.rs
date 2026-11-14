use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json};
use discordia_protocol::push::{
    CreateHandle, DeviceRegistered, HandleCreated, MAX_PUSH_BODY_BYTES, PushData, RegisterDevice,
    UnregisterDevice,
};
use serde::de::DeserializeOwned;

use crate::limits::{bucket, client_ip};
use crate::sender::{Delivery, short};
use crate::store::{StoreError, is_handle};
use crate::{Relay, now_unix};

/// A registration is a few hundred bytes; nothing signed needs more.
const SIGNED_BODY_BYTES: usize = 8 * 1024;
const MAX_TOKEN_CHARS: usize = 1024;
const MAX_RELAYS: usize = 16;
const MAX_RELAY_URL_CHARS: usize = 256;

#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub IpAddr);

pub struct ApiError(StatusCode, String);

impl ApiError {
    fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        Self(status, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NoDevice => ApiError::new(StatusCode::NOT_FOUND, "no such device"),
            StoreError::HandleCap => ApiError::new(
                StatusCode::CONFLICT,
                "this device holds all the handles it may",
            ),
            StoreError::Full => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "this relay takes no new devices at the moment",
            ),
            StoreError::Storage(e) => {
                tracing::error!(error = %e, "push store failed");
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "storage failed")
            }
        }
    }
}

pub fn router(relay: Arc<Relay>) -> Router {
    let signed = Router::new()
        .route("/", get(root))
        .route("/v1/devices", put(put_device).delete(delete_device))
        .route("/v1/handles", post(create_handle))
        .route("/v1/handles/:handle", delete(delete_handle))
        .route("/v1/attention", put(put_attention).delete(delete_attention))
        .layer(DefaultBodyLimit::max(SIGNED_BODY_BYTES));
    let push = Router::new()
        .route("/v1/push/:handle", post(push))
        .layer(DefaultBodyLimit::max(MAX_PUSH_BODY_BYTES));
    signed
        .merge(push)
        .layer(middleware::from_fn_with_state(relay.clone(), per_ip))
        .with_state(relay)
}

async fn per_ip(State(relay): State<Arc<Relay>>, mut req: Request, next: Next) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let forwarded = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok());
    // Served without connect info (a test router), everyone shares one bucket.
    let ip = bucket(peer.map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |p| {
        client_ip(&relay.config.trusted_proxies, p, forwarded)
    }));
    let limiter = if req.uri().path().starts_with("/v1/push/") {
        &relay.limits.push
    } else {
        &relay.limits.api
    };
    if !limiter.admit(&ip) {
        return (StatusCode::TOO_MANY_REQUESTS, "slow down").into_response();
    }
    req.extensions_mut().insert(ClientIp(ip));
    next.run(req).await
}

async fn root() -> String {
    format!("discordia-push-relay {}", env!("CARGO_PKG_VERSION"))
}

fn authenticate(
    relay: &Relay,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<String, ApiError> {
    crate::auth::verify(
        &relay.config.audiences,
        method.as_str(),
        uri.path(),
        headers,
        body,
        now_unix(),
    )
    .map_err(|e| ApiError::new(StatusCode::UNAUTHORIZED, e.to_string()))
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.to_string()))
}

fn token_ok(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_TOKEN_CHARS
        && token.bytes().all(|b| b.is_ascii_graphic())
}

fn validate(req: &RegisterDevice) -> Result<(), ApiError> {
    let bad = |m: &str| Err(ApiError::new(StatusCode::BAD_REQUEST, m));
    if !token_ok(&req.token) || req.replaces.as_deref().is_some_and(|t| !token_ok(t)) {
        return bad("not an FCM token");
    }
    if req.package.is_empty()
        || req.package.len() > 128
        || !req
            .package
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
    {
        return bad("not an Android package name");
    }
    if req.relays.len() > MAX_RELAYS
        || req.relays.iter().any(|r| {
            r.len() > MAX_RELAY_URL_CHARS || !(r.starts_with("wss://") || r.starts_with("ws://"))
        })
    {
        return bad("relays must be at most 16 ws:// or wss:// URLs");
    }
    Ok(())
}

async fn put_device(
    State(relay): State<Arc<Relay>>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<DeviceRegistered>, ApiError> {
    let pubkey = authenticate(&relay, &method, &uri, &headers, &body)?;
    let req: RegisterDevice = parse(&body)?;
    validate(&req)?;
    if !relay.store.device_exists(&pubkey, &req.token, now_unix())? {
        if !relay.limits.registrations.admit(&ip) {
            return Err(ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too many new devices from this address today",
            ));
        }
        if relay.config.validate_tokens {
            match relay.sender.send(&req.token, &PushData::Ping, true).await {
                Delivery::Sent => {}
                Delivery::TokenGone => {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "FCM does not accept this token",
                    ));
                }
                Delivery::Failed(_) => {
                    return Err(ApiError::new(
                        StatusCode::BAD_GATEWAY,
                        "could not check the token with FCM; try again",
                    ));
                }
            }
        }
    }
    let expires_at = relay.store.upsert_device(&pubkey, &req, now_unix())?;
    tracing::info!(
        pubkey = short(&pubkey),
        token = short(&req.token),
        dm = req.dm,
        "device registered"
    );
    Ok(Json(DeviceRegistered { expires_at }))
}

async fn delete_device(
    State(relay): State<Arc<Relay>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let pubkey = authenticate(&relay, &method, &uri, &headers, &body)?;
    let req: UnregisterDevice = parse(&body)?;
    if relay.store.delete_device(&pubkey, &req.token)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "no such device"))
    }
}

async fn create_handle(
    State(relay): State<Arc<Relay>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<HandleCreated>, ApiError> {
    let pubkey = authenticate(&relay, &method, &uri, &headers, &body)?;
    let req: CreateHandle = parse(&body)?;
    let handle = relay.store.create_handle(&pubkey, &req.token, now_unix())?;
    Ok(Json(HandleCreated { handle }))
}

async fn delete_handle(
    State(relay): State<Arc<Relay>>,
    Path(handle): Path<String>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let pubkey = authenticate(&relay, &method, &uri, &headers, &body)?;
    if is_handle(&handle) && relay.store.delete_handle(&pubkey, &handle)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "no such handle"))
    }
}

/// 404 tells a desktop there is no phone to quiet, so it stops asking.
async fn put_attention(
    State(relay): State<Arc<Relay>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let pubkey = authenticate(&relay, &method, &uri, &headers, &body)?;
    if relay.attend(&pubkey, now_unix())? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(StatusCode::NOT_FOUND, "no device takes DMs"))
    }
}

async fn delete_attention(
    State(relay): State<Arc<Relay>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let pubkey = authenticate(&relay, &method, &uri, &headers, &body)?;
    relay.release(&pubkey);
    Ok(StatusCode::NO_CONTENT)
}

/// NIP-44 output is base64; anything else cannot be a payload the phone opens.
fn plausible_ciphertext(body: &[u8]) -> bool {
    !body.is_empty()
        && body
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_'))
}

async fn push(
    State(relay): State<Arc<Relay>>,
    Path(handle): Path<String>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    let unknown = || ApiError::new(StatusCode::NOT_FOUND, "unknown or expired handle");
    if !is_handle(&handle) {
        return Err(unknown());
    }
    if !plausible_ciphertext(&body) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "the body must be base64 ciphertext",
        ));
    }
    let target = relay
        .store
        .handle_target(&handle, now_unix())?
        .ok_or_else(unknown)?;
    let now = std::time::Instant::now();
    if !(relay.limits.handle_day.has_room(&handle, now)
        && relay.limits.handle_minute.admit_at(&handle, now)
        && relay.limits.handle_day.admit_at(&handle, now))
    {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "this handle is over its limit",
        ));
    }
    let permit = relay.try_in_flight().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "the relay is busy; try again",
        )
    })?;
    let c = String::from_utf8_lossy(&body).into_owned();
    tracing::debug!(handle = short(&handle), bytes = body.len(), "push accepted");
    tokio::spawn(async move {
        let data = PushData::Mention { h: handle, c };
        relay.deliver(&target, &data).await;
        drop(permit);
    });
    Ok(StatusCode::ACCEPTED)
}
