//! `FcmSender` against a local server playing Google: the OAuth token endpoint
//! and FCM HTTP v1 `messages:send`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Form, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use discordia_protocol::push::{PushData, RegisterDevice};
use discordia_push_relay::fcm::{FcmSender, Retry, SCOPE};
use discordia_push_relay::limits::Limits;
use discordia_push_relay::sender::{Delivery, Sender};
use discordia_push_relay::store::{Caps, Store};
use discordia_push_relay::{Config, Relay, now_unix};
use ring::signature::{KeyPair, RSA_PKCS1_2048_8192_SHA256, RsaKeyPair, UnparsedPublicKey};
use serde_json::{Value, json};

/// Generated for these tests alone; it signs nothing anywhere else.
const TEST_KEY: &str = include_str!("fixtures/test-only-rsa.pem");
const PROJECT: &str = "test-project";
const EMAIL: &str = "relay@test-project.iam.example";

#[derive(Default)]
struct Google {
    token_uri: String,
    tokens_minted: usize,
    sends: Vec<(String, Value)>,
    /// Answers for the next sends, in order; then 200.
    script: VecDeque<(StatusCode, Value)>,
}

type Shared = Arc<Mutex<Google>>;

fn public_key() -> Vec<u8> {
    let der: String = TEST_KEY
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();
    let pair = RsaKeyPair::from_pkcs8(&STANDARD.decode(der).unwrap()).unwrap();
    pair.public_key().as_ref().to_vec()
}

async fn token(
    State(g): State<Shared>,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    assert_eq!(
        form.get("grant_type").map(String::as_str),
        Some("urn:ietf:params:oauth:grant-type:jwt-bearer")
    );
    let jwt = form.get("assertion").expect("an assertion");
    let parts: Vec<&str> = jwt.split('.').collect();
    assert_eq!(parts.len(), 3);
    let signed = format!("{}.{}", parts[0], parts[1]);
    let sig = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, public_key())
        .verify(signed.as_bytes(), &sig)
        .expect("the assertion is RS256 under the service account's key");
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(header["alg"], "RS256");
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    let mut g = g.lock().unwrap();
    assert_eq!(claims["iss"], EMAIL);
    assert_eq!(claims["sub"], EMAIL);
    assert_eq!(claims["scope"], SCOPE);
    assert_eq!(claims["aud"], g.token_uri.as_str());
    assert_eq!(
        claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(),
        3600
    );
    g.tokens_minted += 1;
    Json(json!({
        "access_token": format!("at-{}", g.tokens_minted),
        "expires_in": 3599,
        "token_type": "Bearer",
    }))
}

async fn send(
    State(g): State<Shared>,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut g = g.lock().unwrap();
    assert_eq!(uri.path(), format!("/v1/projects/{PROJECT}/messages:send"));
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    g.sends.push((auth.clone(), body));
    if auth != format!("Bearer at-{}", g.tokens_minted) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"code": 401, "status": "UNAUTHENTICATED"}})),
        );
    }
    let (status, body) = g.script.pop_front().unwrap_or((
        StatusCode::OK,
        json!({"name": "projects/test-project/messages/1"}),
    ));
    (status, Json(body))
}

async fn google() -> (String, Shared) {
    let state: Shared = Arc::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    state.lock().unwrap().token_uri = format!("{base}/token");
    let app = Router::new()
        .route("/token", post(token))
        .fallback(post(send))
        .with_state(state.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (base, state)
}

fn sender(base: &str, attempts: u32) -> FcmSender {
    let account = json!({
        "type": "service_account",
        "project_id": PROJECT,
        "client_email": EMAIL,
        "private_key": TEST_KEY,
        "token_uri": format!("{base}/token"),
    });
    FcmSender::from_json(
        &account.to_string(),
        base,
        Retry {
            attempts,
            backoff: Duration::from_millis(5),
        },
    )
    .unwrap()
}

fn unregistered() -> (StatusCode, Value) {
    (
        StatusCode::NOT_FOUND,
        json!({"error": {"code": 404, "status": "NOT_FOUND", "message": "Requested entity was not found.",
            "details": [{"@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError", "errorCode": "UNREGISTERED"}]}}),
    )
}

fn unavailable() -> (StatusCode, Value) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        json!({"error": {"code": 503, "status": "UNAVAILABLE"}}),
    )
}

#[tokio::test]
async fn one_token_serves_many_sends_and_the_message_is_data_only() {
    let (base, g) = google().await;
    let fcm = sender(&base, 3);
    let data = PushData::Mention {
        h: "handle".into(),
        c: "Y2lwaGVy".into(),
    };
    assert_eq!(fcm.send("tok-1", &data, true).await, Delivery::Sent);
    assert_eq!(fcm.send("tok-1", &data, false).await, Delivery::Sent);
    let g = g.lock().unwrap();
    assert_eq!(g.tokens_minted, 1, "the access token is cached");
    assert_eq!(g.sends.len(), 2);
    let (auth, check) = &g.sends[0];
    assert_eq!(auth, "Bearer at-1");
    assert_eq!(check["validate_only"], true);
    assert_eq!(g.sends[1].1["validate_only"], false);
    let message = &check["message"];
    assert_eq!(message["token"], "tok-1");
    assert_eq!(
        message["data"],
        json!({"t": "m", "h": "handle", "c": "Y2lwaGVy"})
    );
    assert_eq!(
        message["android"],
        json!({"priority": "HIGH", "ttl": "86400s"})
    );
    assert!(
        message.get("notification").is_none(),
        "data-only, or Android skips the app"
    );
}

#[tokio::test]
async fn a_503_is_retried_and_a_lasting_one_gives_up() {
    let (base, g) = google().await;
    let fcm = sender(&base, 3);
    g.lock().unwrap().script.push_back(unavailable());
    assert_eq!(
        fcm.send("tok", &PushData::Ping, false).await,
        Delivery::Sent
    );
    assert_eq!(g.lock().unwrap().sends.len(), 2);

    {
        let mut g = g.lock().unwrap();
        g.sends.clear();
        for _ in 0..3 {
            g.script.push_back(unavailable());
        }
    }
    assert!(matches!(
        fcm.send("tok", &PushData::Ping, false).await,
        Delivery::Failed(_)
    ));
    assert_eq!(g.lock().unwrap().sends.len(), 3, "bounded by the attempts");
}

#[tokio::test]
async fn a_rejected_access_token_is_minted_again_once() {
    let (base, g) = google().await;
    let fcm = sender(&base, 3);
    assert_eq!(
        fcm.send("tok", &PushData::Ping, false).await,
        Delivery::Sent
    );
    // Google revoking the token: the next send's bearer no longer matches.
    g.lock().unwrap().tokens_minted += 1;
    assert_eq!(
        fcm.send("tok", &PushData::Ping, false).await,
        Delivery::Sent
    );
    assert_eq!(g.lock().unwrap().tokens_minted, 3);
}

#[tokio::test]
async fn an_unregistered_token_loses_its_device() {
    let (base, g) = google().await;
    let relay = Relay::new(
        Store::in_memory(Caps::default()).unwrap(),
        Arc::new(sender(&base, 3)),
        Config::default(),
        Limits::default(),
    );
    let pubkey = "ab".repeat(32);
    relay
        .store
        .upsert_device(
            &pubkey,
            &RegisterDevice {
                token: "gone".into(),
                package: "com.discordia.app".into(),
                dm: true,
                relays: vec![],
                replaces: None,
            },
            now_unix(),
        )
        .unwrap();
    g.lock().unwrap().script.push_back(unregistered());
    assert_eq!(relay.ping(&pubkey).await.unwrap(), [Delivery::TokenGone]);
    assert_eq!(relay.store.device_count().unwrap(), 0);
    assert_eq!(
        g.lock().unwrap().sends.len(),
        1,
        "a dead token is not retried"
    );
}
