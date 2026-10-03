use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use dioxusfun_protocol::rendezvous::VoiceEvictRequest;
use dioxusfun_rendezvous::{AppCtx, Config, registry::Registry, router};

async fn serve(app: axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{address}")
}

#[tokio::test]
async fn eviction_is_authenticated_scoped_and_removes_all_four_seats() {
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::<Vec<u8>>::new()));
    let fake_sfu = axum::Router::new()
        .route(
            "/twirp/livekit.RoomService/:method",
            post(
                |Path(method): Path<String>,
                 State(calls): State<Arc<tokio::sync::Mutex<Vec<Vec<u8>>>>>,
                 body: axum::body::Bytes| async move {
                    assert_eq!(method, "RemoveParticipant");
                    calls.lock().await.push(body.to_vec());
                    (
                        StatusCode::OK,
                        [("content-type", "application/protobuf")],
                        Vec::<u8>::new(),
                    )
                },
            ),
        )
        .with_state(calls.clone());
    let sfu = serve(fake_sfu).await;
    let registry = Arc::new(Registry::new());
    let grant = registry.issue_voice_grant("host-a");
    let url = serve(router(AppCtx {
        registry,
        config: Arc::new(Config {
            livekit_url: Some(sfu),
            livekit_api_key: Some("key".into()),
            livekit_api_secret: Some("secret-at-least-thirty-two-characters".into()),
            ..Default::default()
        }),
    }))
    .await;
    let channel_id = uuid::Uuid::new_v4();
    let pubkey = "ab".repeat(32);
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{url}/voice-evict"))
        .json(&VoiceEvictRequest {
            grant: "wrong".into(),
            channel_id,
            pubkey: pubkey.clone(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(calls.lock().await.is_empty());
    let response = client
        .post(format!("{url}/voice-evict"))
        .json(&VoiceEvictRequest {
            grant: grant.clone(),
            channel_id,
            pubkey: "host-b--someone".into(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(calls.lock().await.is_empty());
    let response = client
        .post(format!("{url}/voice-evict"))
        .json(&VoiceEvictRequest {
            grant,
            channel_id,
            pubkey: pubkey.clone(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let calls = calls.lock().await;
    assert_eq!(calls.len(), 4);
    for identity in [
        &pubkey,
        &format!("{pubkey}#audio"),
        &format!("{pubkey}#video"),
    ] {
        assert!(calls.iter().any(|body| {
            body.windows(identity.len())
                .any(|window| window == identity.as_bytes())
        }));
    }
    for body in calls.iter() {
        let voice = format!("host-a--voice-{channel_id}");
        let screen = format!("host-a--screen-{channel_id}");
        assert!(
            body.windows(voice.len()).any(|w| w == voice.as_bytes())
                || body.windows(screen.len()).any(|w| w == screen.as_bytes())
        );
        assert!(!body.windows(6).any(|w| w == b"host-b"));
    }
}

#[tokio::test]
async fn eviction_reports_sfu_failure_instead_of_claiming_success() {
    let sfu = serve(axum::Router::new().route(
        "/twirp/livekit.RoomService/:method",
        post(|| async {
            (
                StatusCode::FORBIDDEN,
                axum::Json(serde_json::json!({"code":"permission_denied","msg":"denied"})),
            )
        }),
    ))
    .await;
    let registry = Arc::new(Registry::new());
    let grant = registry.issue_voice_grant("host-a");
    let url = serve(router(AppCtx {
        registry,
        config: Arc::new(Config {
            livekit_url: Some(sfu),
            livekit_api_key: Some("key".into()),
            livekit_api_secret: Some("secret-at-least-thirty-two-characters".into()),
            ..Default::default()
        }),
    }))
    .await;
    let response = reqwest::Client::new()
        .post(format!("{url}/voice-evict"))
        .json(&VoiceEvictRequest {
            grant,
            channel_id: uuid::Uuid::new_v4(),
            pubkey: "ab".repeat(32),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}
