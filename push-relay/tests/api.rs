use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use discordia_protocol::push::{
    MAX_PUSH_BODY_BYTES, PUBKEY_HEADER, PushData, SIGNATURE_HEADER, TIME_HEADER,
};
use discordia_push_relay::auth::sign_request;
use discordia_push_relay::limits::{Limiter, Limits};
use discordia_push_relay::sender::{Delivery, RecordingSender};
use discordia_push_relay::store::{Caps, Store};
use discordia_push_relay::{Config, Relay, now_unix, router};
use reqwest::{Method, StatusCode};
use secp256k1::SecretKey;
use serde_json::json;

/// What every test phone dials: the listener is bound to it.
const AUDIENCE: &str = "127.0.0.1";

struct Harness {
    base: String,
    relay: Arc<Relay>,
    rec: Arc<RecordingSender>,
    http: reqwest::Client,
}

async fn spawn() -> Harness {
    spawn_with(Config::default(), Limits::default()).await
}

async fn spawn_with(config: Config, limits: Limits) -> Harness {
    let rec = Arc::new(RecordingSender::default());
    let relay = Arc::new(Relay::new(
        Store::in_memory(Caps::default()).unwrap(),
        rec.clone(),
        config,
        limits,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(relay.clone());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Harness {
        base: format!("http://{addr}"),
        relay,
        rec,
        http: reqwest::Client::new(),
    }
}

fn key(seed: u8) -> SecretKey {
    SecretKey::from_slice(&[seed; 32]).unwrap()
}

/// How a phone builds a signed request; each field can be bent by a test.
struct Signed<'a> {
    secret: SecretKey,
    method: Method,
    path: &'a str,
    audience: &'a str,
    time: u64,
    body: Vec<u8>,
    pubkey_override: Option<String>,
    body_override: Option<Vec<u8>>,
}

impl<'a> Signed<'a> {
    fn new(secret: SecretKey, method: Method, path: &'a str, body: serde_json::Value) -> Self {
        Self {
            secret,
            method,
            path,
            audience: AUDIENCE,
            time: now_unix(),
            body: if body.is_null() {
                Vec::new()
            } else {
                serde_json::to_vec(&body).unwrap()
            },
            pubkey_override: None,
            body_override: None,
        }
    }

    async fn send(self, h: &Harness) -> reqwest::Response {
        let (pubkey, sig) = sign_request(
            &self.secret,
            self.method.as_str(),
            self.path,
            self.audience,
            self.time,
            &self.body,
        )
        .unwrap();
        h.http
            .request(self.method, format!("{}{}", h.base, self.path))
            .header(PUBKEY_HEADER, self.pubkey_override.unwrap_or(pubkey))
            .header(TIME_HEADER, self.time.to_string())
            .header(SIGNATURE_HEADER, sig)
            .body(self.body_override.unwrap_or(self.body))
            .send()
            .await
            .unwrap()
    }
}

fn device(token: &str) -> serde_json::Value {
    json!({"token": token, "package": "com.discordia.app", "dm": true, "relays": ["wss://relay.example"]})
}

async fn register(h: &Harness, seed: u8, token: &str) -> StatusCode {
    Signed::new(key(seed), Method::PUT, "/v1/devices", device(token))
        .send(h)
        .await
        .status()
}

async fn new_handle(h: &Harness, seed: u8, token: &str) -> String {
    let resp = Signed::new(
        key(seed),
        Method::POST,
        "/v1/handles",
        json!({"token": token}),
    )
    .send(h)
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    resp.json::<serde_json::Value>().await.unwrap()["handle"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn push(h: &Harness, handle: &str, body: impl Into<reqwest::Body>) -> StatusCode {
    h.http
        .post(format!("{}/v1/push/{handle}", h.base))
        .body(body)
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_registered_handle_carries_a_push_to_its_device() {
    let h = spawn().await;
    let resp = Signed::new(key(1), Method::PUT, "/v1/devices", device("tok-1"))
        .send(&h)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let expires = resp.json::<serde_json::Value>().await.unwrap()["expires_at"]
        .as_u64()
        .unwrap();
    assert!(expires >= now_unix() + 29 * 24 * 3600);
    let checks: Vec<_> = h
        .rec
        .sent()
        .into_iter()
        .filter(|r| r.validate_only)
        .collect();
    assert_eq!(checks.len(), 1, "a new token is checked with FCM first");
    assert_eq!(checks[0].token, "tok-1");
    assert_eq!(register(&h, 1, "tok-1").await, StatusCode::OK);
    assert_eq!(
        h.rec.sent().iter().filter(|r| r.validate_only).count(),
        1,
        "a refresh is not checked again"
    );

    let handle = new_handle(&h, 1, "tok-1").await;
    assert_eq!(
        push(&h, &handle, "c2lwaGVy+/==").await,
        StatusCode::ACCEPTED
    );
    let sent = h.rec.wait_for(1, Duration::from_secs(5)).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].token, "tok-1");
    assert_eq!(
        sent[0].data,
        PushData::Mention {
            h: handle,
            c: "c2lwaGVy+/==".into()
        }
    );
}

#[tokio::test]
async fn a_desktop_in_front_holds_and_ends_its_keys_dm_pushes() {
    let h = spawn().await;
    let attention = |seed, method| Signed::new(key(seed), method, "/v1/attention", json!(null));
    let pubkey = sign_request(&key(1), "PUT", "/v1/attention", AUDIENCE, 0, b"")
        .unwrap()
        .0;
    assert_eq!(
        attention(1, Method::PUT).send(&h).await.status(),
        StatusCode::NOT_FOUND,
        "no phone to quiet, so the desktop is told to stop asking"
    );
    assert_eq!(register(&h, 1, "tok-1").await, StatusCode::OK);
    assert_eq!(
        attention(1, Method::PUT).send(&h).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(h.relay.attended(&pubkey, now_unix()));

    assert_eq!(
        attention(2, Method::DELETE).send(&h).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(
        h.relay.attended(&pubkey, now_unix()),
        "another key ends only its own"
    );
    let mut forged = attention(2, Method::DELETE);
    forged.pubkey_override = Some(pubkey.clone());
    assert_eq!(forged.send(&h).await.status(), StatusCode::UNAUTHORIZED);
    assert!(h.relay.attended(&pubkey, now_unix()));

    assert_eq!(
        attention(1, Method::DELETE).send(&h).await.status(),
        StatusCode::NO_CONTENT
    );
    assert!(!h.relay.attended(&pubkey, now_unix()));
}

#[tokio::test]
async fn a_request_the_signature_does_not_cover_is_refused() {
    let h = spawn().await;
    let base = || Signed::new(key(1), Method::PUT, "/v1/devices", device("tok"));

    let mut wrong_audience = base();
    wrong_audience.audience = "evil.example";
    let mut stale = base();
    stale.time = now_unix() - 360;
    let mut future = base();
    future.time = now_unix() + 360;
    let mut tampered = base();
    tampered.body_override = Some(serde_json::to_vec(&device("other")).unwrap());
    let mut upper = base();
    let (pk, _) = sign_request(&key(1), "PUT", "/v1/devices", AUDIENCE, 0, b"").unwrap();
    upper.pubkey_override = Some(pk.to_ascii_uppercase());
    let mut wrong_key = base();
    let (other, _) = sign_request(&key(2), "PUT", "/v1/devices", AUDIENCE, 0, b"").unwrap();
    wrong_key.pubkey_override = Some(other);

    for (what, req) in [
        ("audience", wrong_audience),
        ("stale", stale),
        ("future", future),
        ("tampered body", tampered),
        ("uppercase key", upper),
        ("wrong key", wrong_key),
    ] {
        assert_eq!(
            req.send(&h).await.status(),
            StatusCode::UNAUTHORIZED,
            "{what}"
        );
    }
    let unsigned = h
        .http
        .put(format!("{}/v1/devices", h.base))
        .json(&device("tok"))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(h.relay.store.device_count().unwrap(), 0);
    assert!(h.rec.sent().is_empty(), "nothing reached FCM");
    assert_eq!(register(&h, 1, "tok").await, StatusCode::OK);
}

#[tokio::test]
async fn push_answers_404_413_400_and_429_where_it_should() {
    let h = spawn().await;
    register(&h, 1, "tok").await;
    let handle = new_handle(&h, 1, "tok").await;

    let unknown = "A".repeat(43);
    assert_eq!(push(&h, &unknown, "YQ==").await, StatusCode::NOT_FOUND);
    assert_eq!(push(&h, "short", "YQ==").await, StatusCode::NOT_FOUND);
    assert_eq!(
        push(&h, &handle, "A".repeat(MAX_PUSH_BODY_BYTES + 1)).await,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        push(&h, &handle, "not base64!").await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(push(&h, &handle, "").await, StatusCode::BAD_REQUEST);

    assert_eq!(
        push(&h, &handle, "A".repeat(MAX_PUSH_BODY_BYTES)).await,
        StatusCode::ACCEPTED
    );
    for _ in 1..20 {
        assert_eq!(push(&h, &handle, "YQ==").await, StatusCode::ACCEPTED);
    }
    assert_eq!(
        push(&h, &handle, "YQ==").await,
        StatusCode::TOO_MANY_REQUESTS
    );

    let other = new_handle(&h, 1, "tok").await;
    assert_eq!(
        push(&h, &other, "YQ==").await,
        StatusCode::ACCEPTED,
        "a noisy handle is limited alone"
    );
}

#[tokio::test]
async fn a_token_fcm_calls_unregistered_takes_its_device_and_handles_with_it() {
    let h = spawn().await;
    register(&h, 1, "tok").await;
    let handle = new_handle(&h, 1, "tok").await;
    h.rec.answer(Delivery::TokenGone);
    assert_eq!(push(&h, &handle, "YQ==").await, StatusCode::ACCEPTED);
    h.rec.wait_for(1, Duration::from_secs(5)).await;
    for _ in 0..50 {
        if h.relay.store.device_count().unwrap() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(h.relay.store.device_count().unwrap(), 0);
    assert_eq!(push(&h, &handle, "YQ==").await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_token_fcm_refuses_is_never_stored() {
    let h = spawn().await;
    h.rec.answer(Delivery::TokenGone);
    assert_eq!(register(&h, 1, "dead").await, StatusCode::BAD_REQUEST);
    h.rec.answer(Delivery::Failed("503".into()));
    assert_eq!(register(&h, 1, "unsure").await, StatusCode::BAD_GATEWAY);
    assert_eq!(h.relay.store.device_count().unwrap(), 0);
}

#[tokio::test]
async fn handles_and_devices_are_deleted_only_by_their_key() {
    let h = spawn().await;
    register(&h, 1, "tok").await;
    let handle = new_handle(&h, 1, "tok").await;
    let path = format!("/v1/handles/{handle}");

    let resp = Signed::new(key(2), Method::DELETE, &path, json!(null))
        .send(&h)
        .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = Signed::new(key(2), Method::POST, "/v1/handles", json!({"token": "tok"}))
        .send(&h)
        .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "a handle needs your own device"
    );
    let resp = Signed::new(key(1), Method::DELETE, &path, json!(null))
        .send(&h)
        .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(push(&h, &handle, "YQ==").await, StatusCode::NOT_FOUND);

    let second = new_handle(&h, 1, "tok").await;
    let resp = Signed::new(
        key(2),
        Method::DELETE,
        "/v1/devices",
        json!({"token": "tok"}),
    )
    .send(&h)
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let resp = Signed::new(
        key(1),
        Method::DELETE,
        "/v1/devices",
        json!({"token": "tok"}),
    )
    .send(&h)
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(push(&h, &second, "YQ==").await, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_rotated_token_keeps_its_handles_over_http() {
    let h = spawn().await;
    register(&h, 1, "old").await;
    let handle = new_handle(&h, 1, "old").await;
    let mut body = device("new");
    body["replaces"] = "old".into();
    let resp = Signed::new(key(1), Method::PUT, "/v1/devices", body)
        .send(&h)
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(push(&h, &handle, "YQ==").await, StatusCode::ACCEPTED);
    let sent = h.rec.wait_for(1, Duration::from_secs(5)).await;
    assert_eq!(sent[0].token, "new");
}

#[tokio::test]
async fn a_token_put_under_another_key_moves_to_it_and_leaves_its_handles() {
    let h = spawn().await;
    register(&h, 1, "tok").await;
    let handle = new_handle(&h, 1, "tok").await;
    let mut changes = h.relay.store.changes();
    assert_eq!(register(&h, 2, "tok").await, StatusCode::OK);
    assert!(changes.has_changed().unwrap(), "the DM watcher hears of it");
    changes.mark_unchanged();
    assert_eq!(h.relay.store.device_count().unwrap(), 1);
    let (second, _) = sign_request(&key(2), "PUT", "/", AUDIENCE, 0, b"").unwrap();
    assert_eq!(
        h.relay.store.devices_of(&second, true, now_unix()).unwrap()[0].token,
        "tok"
    );
    assert_eq!(push(&h, &handle, "YQ==").await, StatusCode::NOT_FOUND);
    let resp = Signed::new(
        key(1),
        Method::DELETE,
        "/v1/devices",
        json!({"token": "tok"}),
    )
    .send(&h)
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "the old key holds nothing"
    );
    assert!(!changes.has_changed().unwrap());
}

#[tokio::test]
async fn new_devices_are_limited_per_address_per_day_and_refreshes_are_not() {
    let limits = Limits {
        registrations: Limiter::per_day(2),
        ..Limits::default()
    };
    let h = spawn_with(Config::default(), limits).await;
    assert_eq!(register(&h, 1, "a").await, StatusCode::OK);
    assert_eq!(register(&h, 2, "b").await, StatusCode::OK);
    assert_eq!(register(&h, 3, "c").await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(register(&h, 1, "a").await, StatusCode::OK);
}

#[tokio::test]
async fn a_lapsed_device_put_again_is_new_to_fcm_and_to_the_limiter() {
    let limits = Limits {
        registrations: Limiter::per_day(1),
        ..Limits::default()
    };
    let h = spawn_with(Config::default(), limits).await;
    let pk = discordia_nostr::event::xonly_hex(&key(1));
    let req: discordia_protocol::push::RegisterDevice =
        serde_json::from_value(device("lapsed")).unwrap();
    let ttl = Caps::default().ttl_secs;
    h.relay
        .store
        .upsert_device(&pk, &req, now_unix() - ttl - 1)
        .unwrap();

    h.rec.answer(Delivery::TokenGone);
    assert_eq!(
        register(&h, 1, "lapsed").await,
        StatusCode::BAD_REQUEST,
        "the token is checked again"
    );
    h.rec.answer(Delivery::Sent);
    assert_eq!(register(&h, 2, "b").await, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn a_malformed_registration_is_refused() {
    let h = spawn().await;
    for body in [
        json!({"token": "", "package": "com.discordia.app"}),
        json!({"token": "has space", "package": "com.discordia.app"}),
        json!({"token": "t", "package": "../etc"}),
        json!({"token": "t", "package": "p", "relays": ["https://x"]}),
        json!({"package": "p"}),
    ] {
        let resp = Signed::new(key(1), Method::PUT, "/v1/devices", body)
            .send(&h)
            .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}

async fn get_root(h: &Harness, forwarded_for: &str) -> StatusCode {
    h.http
        .get(format!("{}/", h.base))
        .header("x-forwarded-for", forwarded_for)
        .send()
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn forwarded_for_from_an_untrusted_peer_is_ignored() {
    let one_per_minute = || Limits {
        api: Limiter::per_minute(1),
        ..Limits::default()
    };
    let h = spawn_with(Config::default(), one_per_minute()).await;
    assert_eq!(get_root(&h, "198.51.100.1").await, StatusCode::OK);
    assert_eq!(
        get_root(&h, "198.51.100.2").await,
        StatusCode::TOO_MANY_REQUESTS,
        "a header is a claim unless the peer is a trusted proxy"
    );

    let trusted = Config {
        trusted_proxies: vec!["127.0.0.1".parse().unwrap()],
        ..Config::default()
    };
    let h = spawn_with(trusted, one_per_minute()).await;
    assert_eq!(get_root(&h, "198.51.100.1").await, StatusCode::OK);
    assert_eq!(get_root(&h, "198.51.100.2").await, StatusCode::OK);
    assert_eq!(
        get_root(&h, "198.51.100.1").await,
        StatusCode::TOO_MANY_REQUESTS
    );
}
