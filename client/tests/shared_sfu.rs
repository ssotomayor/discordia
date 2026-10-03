use std::sync::Arc;
use std::time::Duration;

use dioxusfun_rendezvous::{AppCtx, Config, registry::Registry};
use dioxusfun_server::protocol::rendezvous::VoiceEvictRequest;
use livekit::prelude::*;
use livekit_api::access_token::{AccessToken, VideoGrants};

#[tokio::test]
#[ignore = "requires LIVEKIT_URL, LIVEKIT_API_KEY and LIVEKIT_API_SECRET for a local test SFU"]
async fn rendezvous_removes_all_five_native_and_webview_seats() {
    let url = std::env::var("LIVEKIT_URL").unwrap();
    let key = std::env::var("LIVEKIT_API_KEY").unwrap();
    let secret = std::env::var("LIVEKIT_API_SECRET").unwrap();
    let channel = uuid::Uuid::new_v4();
    let pubkey = "ab".repeat(32);
    let registry = Arc::new(Registry::new());
    let grant = registry.issue_voice_grant("test-host");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let coordinator = format!("http://{}", listener.local_addr().unwrap());
    let app = dioxusfun_rendezvous::router(AppCtx {
        registry,
        config: Arc::new(Config {
            livekit_url: Some(url.clone()),
            livekit_api_key: Some(key.clone()),
            livekit_api_secret: Some(secret.clone()),
            ..Default::default()
        }),
    });
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let voice = format!("test-host--voice-{channel}");
    let screen = format!("test-host--screen-{channel}");
    let mut seats = Vec::new();
    for (room, identity) in [
        (&voice, pubkey.clone()),
        (&screen, pubkey.clone()),
        (&screen, format!("{pubkey}#audio")),
        (&screen, format!("{pubkey}#video")),
        (&screen, format!("{pubkey}#viewer")),
    ] {
        let token = AccessToken::with_api_key(&key, &secret)
            .with_identity(&identity)
            .with_grants(VideoGrants {
                room_join: true,
                room: room.clone(),
                can_publish: true,
                can_subscribe: true,
                ..Default::default()
            })
            .to_jwt()
            .unwrap();
        let (joined, events) = Room::connect(&url, &token, RoomOptions::default())
            .await
            .unwrap();
        seats.push((identity, joined, events));
    }
    let response = reqwest::Client::new()
        .post(format!("{coordinator}/voice-evict"))
        .json(&VoiceEvictRequest {
            grant,
            channel_id: channel,
            pubkey,
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    for (identity, room, mut events) in seats {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match events.recv().await {
                    Some(RoomEvent::Disconnected {
                        reason: livekit::DisconnectReason::ParticipantRemoved,
                    }) => break,
                    Some(_) => {}
                    None => panic!("{identity} closed without the eviction reason"),
                }
            }
        })
        .await
        .unwrap();
        drop(room);
    }
    server.abort();
}
