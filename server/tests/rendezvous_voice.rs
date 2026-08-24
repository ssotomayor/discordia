use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_rendezvous::{AppCtx, Config, registry::Registry, router};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ChannelKind, ClientMessage, Id, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

const LAN_HOST: &str = "192.168.0.61";
const PUBLIC_HOST: &str = "203.0.113.5";

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn test_config(livekit: LiveKitConfig) -> dioxusfun_server::ServerConfig {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-rzv-voice-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    dioxusfun_server::ServerConfig {
        livekit,
        operators: Default::default(),
        data_dir: dir,
    }
}

async fn spawn_rendezvous() -> String {
    let ctx = AppCtx {
        registry: Arc::new(Registry::new()),
        config: Arc::new(Config::default()),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(ctx)).await.unwrap();
    });
    format!("ws://{addr}")
}

async fn register(base: &str) -> serde_json::Value {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("{base}/control"))
        .await
        .unwrap();
    loop {
        if let Message::Text(t) = ws.next().await.unwrap().unwrap() {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            if v["op"] == "challenge" {
                break;
            }
        }
    }
    let frame = serde_json::json!({ "op": "register", "d": { "name": null } });
    ws.send(Message::Text(frame.to_string())).await.unwrap();
    loop {
        if let Message::Text(t) = ws.next().await.unwrap().unwrap() {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            if v["op"] == "registered" {
                tokio::spawn(async move { while ws.next().await.is_some() {} });
                return v["d"].clone();
            }
        }
    }
}

async fn spawn_gateway(livekit: LiveKitConfig) -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, test_config(livekit))
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn connect_user(url: &str, id: &BotIdentity, name: &str) -> Bot {
    let mut session = Bot::connect_as_user(url, id, name).await.unwrap();
    loop {
        if let ServerMessage::Ready { .. } = next_timeout(&mut session).await {
            break;
        }
    }
    session
}

async fn livekit_url_for_voice(session: &mut Bot) -> String {
    session
        .send(&ClientMessage::CreateGuild {
            name: "Reachability".into(),
            template: None,
        })
        .await
        .unwrap();
    let guild_id = loop {
        if let ServerMessage::GuildJoined { guild, .. } = next_timeout(session).await {
            break guild.id;
        }
    };
    session
        .send(&ClientMessage::CreateChannel {
            guild_id,
            name: "General".into(),
            kind: ChannelKind::Voice,
            topic: None,
        })
        .await
        .unwrap();
    let channel_id: Id = loop {
        if let ServerMessage::ChannelCreate(ch) = next_timeout(session).await
            && ch.kind == ChannelKind::Voice
        {
            break ch.id;
        }
    };
    session
        .send(&ClientMessage::JoinVoice { channel_id })
        .await
        .unwrap();
    loop {
        if let ServerMessage::VoiceToken { livekit_url, .. } = next_timeout(session).await {
            break livekit_url;
        }
    }
}

#[tokio::test]
async fn a_rendezvous_without_an_sfu_offers_no_livekit_url() {
    let base = spawn_rendezvous().await;
    let registered = register(&base).await;

    assert!(!registered["shortcode"].as_str().unwrap().is_empty());
    assert!(
        registered["livekit_url"].is_null(),
        "expected no shared SFU, got {registered}"
    );
    assert!(
        registered
            .get("voice_token_grant")
            .is_none_or(|g| g.is_null())
    );
}

#[tokio::test]
async fn a_proxied_friend_is_handed_the_lan_address_when_nothing_better_exists() {
    let (url, _handle) = spawn_gateway(LiveKitConfig {
        explicit_url: None,
        port: 7880,
        lan_host: Some(LAN_HOST.into()),
        public_host: None,
        api_key: "devkey".into(),
        api_secret: "secret-must-be-at-least-32-chars-long".into(),
        minter: None,
    })
    .await;

    let mut friend = connect_user(&url, &BotIdentity::generate(), "friend").await;
    assert_eq!(
        livekit_url_for_voice(&mut friend).await,
        format!("ws://{LAN_HOST}:7880")
    );
}

#[tokio::test]
async fn a_mapped_host_hands_proxied_friends_its_public_address() {
    let (url, _handle) = spawn_gateway(LiveKitConfig {
        explicit_url: None,
        port: 7880,
        lan_host: Some(LAN_HOST.into()),
        public_host: Some(PUBLIC_HOST.into()),
        api_key: "devkey".into(),
        api_secret: "secret-must-be-at-least-32-chars-long".into(),
        minter: None,
    })
    .await;

    let mut friend = connect_user(&url, &BotIdentity::generate(), "friend").await;
    assert_eq!(
        livekit_url_for_voice(&mut friend).await,
        format!("ws://{PUBLIC_HOST}:7880")
    );
}
