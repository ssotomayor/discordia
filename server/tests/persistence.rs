//! Phase-1 persistence: state survives a full server restart, and message
//! images are offloaded to the content-addressed blob store.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ChannelKind, ClientMessage, Id, ServerMessage};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn temp_data_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "dioxusfun-persist-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

async fn spawn_on(dir: &PathBuf) -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:19400".parse().unwrap();
    let cfg = dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(),
        operators: Default::default(),
        data_dir: dir.clone(),
    };
    let handle = dioxusfun_server::spawn(preferred, 100, cfg).await.expect("spawn");
    (format!("ws://{}", handle.addr), handle)
}

/// A 1x1 red PNG, base64 (67 bytes decoded).
const TINY_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

#[tokio::test]
async fn state_survives_restart_and_media_is_offloaded() {
    let dir = temp_data_dir();

    // ---- boot #1: create a guild, post text + an image message -------------
    let owner_id = BotIdentity::generate();
    let (guild_id, text_channel): (Id, Id);
    {
        let (url, handle) = spawn_on(&dir).await;
        let mut owner = Bot::connect_as_user(&url, &owner_id, "Owner").await.unwrap();
        loop {
            if matches!(next_timeout(&mut owner).await, ServerMessage::Ready { .. }) {
                break;
            }
        }
        owner
            .send(&ClientMessage::CreateGuild { name: "Persistent".into(), template: None })
            .await
            .unwrap();
        (guild_id, text_channel) = loop {
            if let ServerMessage::GuildJoined { guild, channels, .. } =
                next_timeout(&mut owner).await
            {
                let text = channels
                    .iter()
                    .find(|c| c.kind == ChannelKind::Text)
                    .unwrap()
                    .id;
                break (guild.id, text);
            }
        };
        owner.send_message(text_channel, "survives restarts").await.unwrap();
        owner
            .send(&ClientMessage::SendMessage {
                channel_id: text_channel,
                content: "with an image".into(),
                image: Some(TINY_PNG.into()),
            })
            .await
            .unwrap();
        // Wait until both messages echo back (persisted before delivery).
        let mut seen = 0;
        while seen < 2 {
            if let ServerMessage::MessageCreate(_) = next_timeout(&mut owner).await {
                seen += 1;
            }
        }
        handle.abort();
    }

    // The blob store must hold the offloaded image (content-addressed file),
    // and the DB file must exist.
    let blobs: Vec<_> = std::fs::read_dir(dir.join("media"))
        .expect("media dir exists")
        .filter_map(|e| e.ok())
        .collect();
    assert!(!blobs.is_empty(), "image was offloaded to the blob store");
    assert!(dir.join("discordia.db").exists(), "sqlite file exists");

    // ---- boot #2: same data dir — everything must still be there -----------
    let (url, handle) = spawn_on(&dir).await;
    let mut owner = Bot::connect_as_user(&url, &owner_id, "Owner").await.unwrap();
    let guilds = loop {
        if let ServerMessage::Ready { guilds, .. } = next_timeout(&mut owner).await {
            break guilds;
        }
    };
    assert!(
        guilds.iter().any(|g| g.id == guild_id),
        "owned guild survived the restart (membership rehydrated)"
    );

    owner
        .send(&ClientMessage::FetchMessages { channel_id: text_channel, limit: 50, before_ms: None })
        .await
        .unwrap();
    let history = loop {
        if let ServerMessage::MessageHistory { channel_id, messages } =
            next_timeout(&mut owner).await
        {
            if channel_id == text_channel {
                break messages;
            }
        }
    };
    assert_eq!(history.len(), 2, "both messages survived");
    assert_eq!(history[0].content, "survives restarts");
    assert_eq!(history[1].content, "with an image");
    // The image round-trips: stored as a blob, inlined back as a data URL.
    let img = history[1].image.as_deref().expect("image survived");
    assert!(img.starts_with("data:image/png;base64,"), "inlined on serve");

    handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
}
