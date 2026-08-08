//! End-to-end tests for guild custom emoji: who may manage them, that the
//! catalog reaches members, and that images come back through the on-demand
//! fetch. Same harness as `owner_controls.rs` — a real gateway driven through
//! the bot SDK's `connect_as_user`.

use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ChannelKind, ClientMessage, Id, ServerMessage};

/// A 1x1 transparent PNG, as a data URL — small enough to keep the test fast,
/// real enough that the media store decodes and hashes it like any other image.
const PIXEL_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn test_config(operators: std::collections::HashSet<String>) -> dioxusfun_server::ServerConfig {
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(),
        operators,
        data_dir: dir,
    }
}

async fn spawn_gateway() -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:19260".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, test_config(Default::default()))
        .await
        .expect("spawn server");
    let url = format!("ws://{}", handle.addr);
    (url, handle)
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

async fn create_guild(owner: &mut Bot, name: &str) -> (Id, Id) {
    owner
        .send(&ClientMessage::CreateGuild { name: name.into(), template: None })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { guild, channels, .. } = next_timeout(owner).await {
            let text = channels
                .iter()
                .find(|c| c.kind == ChannelKind::Text)
                .expect("guild has a text channel")
                .id;
            return (guild.id, text);
        }
    }
}

/// Wait for the next emoji catalog push.
async fn next_emojis(session: &mut Bot) -> Vec<dioxusfun_server::protocol::GuildEmoji> {
    loop {
        if let ServerMessage::GuildEmojis { emojis, .. } = next_timeout(session).await {
            return emojis;
        }
    }
}

async fn next_error(session: &mut Bot) -> String {
    loop {
        if let ServerMessage::Error { message } = next_timeout(session).await {
            return message;
        }
    }
}

#[tokio::test]
async fn owner_can_add_rename_and_delete_emoji() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _text) = create_guild(&mut owner, "Emojiland").await;

    owner
        .send(&ClientMessage::CreateGuildEmoji {
            guild_id,
            shortcode: "blobcat".into(),
            image: PIXEL_PNG.into(),
        })
        .await
        .unwrap();
    let emojis = next_emojis(&mut owner).await;
    assert_eq!(emojis.len(), 1);
    assert_eq!(emojis[0].shortcode, "blobcat");
    assert_eq!(emojis[0].added_by, owner_id.pubkey());
    // The image is a content address, never the bytes.
    assert!(
        emojis[0].image.ends_with(".png") && emojis[0].image.len() == 68,
        "expected <64 hex>.png, got {}",
        emojis[0].image
    );

    let emoji_id = emojis[0].id;
    owner
        .send(&ClientMessage::RenameGuildEmoji {
            guild_id,
            emoji_id,
            shortcode: "blobcat_hug".into(),
        })
        .await
        .unwrap();
    let renamed = next_emojis(&mut owner).await;
    assert_eq!(renamed[0].shortcode, "blobcat_hug");
    // Renaming must not move the image — clients holding the bytes keep them.
    assert_eq!(renamed[0].image, emojis[0].image);

    owner
        .send(&ClientMessage::DeleteGuildEmoji { guild_id, emoji_id })
        .await
        .unwrap();
    assert!(next_emojis(&mut owner).await.is_empty());

    handle.abort();
}

/// A plain member holds no `ManageEmojis`, and the client-side `can()` is only
/// advisory — so the gateway has to refuse.
#[tokio::test]
async fn member_without_permission_is_refused() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _text) = create_guild(&mut owner, "Locked").await;

    let mut member = connect_user(&url, &member_id, "Member").await;
    member
        .send(&ClientMessage::JoinGuild { guild_id, accept: true, pow_nonce: None })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { .. } = next_timeout(&mut member).await {
            break;
        }
    }

    member
        .send(&ClientMessage::CreateGuildEmoji {
            guild_id,
            shortcode: "sneaky".into(),
            image: PIXEL_PNG.into(),
        })
        .await
        .unwrap();
    let err = next_error(&mut member).await;
    assert!(err.contains("manage_emojis") || err.contains("permission"), "unexpected: {err}");

    handle.abort();
}

/// The catalog goes to the guild's members, and the images come back through
/// the separate on-demand fetch rather than riding along with it.
#[tokio::test]
async fn catalog_reaches_members_and_images_fetch_on_demand() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _text) = create_guild(&mut owner, "Shared").await;

    let mut member = connect_user(&url, &member_id, "Member").await;
    member
        .send(&ClientMessage::JoinGuild { guild_id, accept: true, pow_nonce: None })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { .. } = next_timeout(&mut member).await {
            break;
        }
    }

    owner
        .send(&ClientMessage::CreateGuildEmoji {
            guild_id,
            shortcode: "party".into(),
            image: PIXEL_PNG.into(),
        })
        .await
        .unwrap();

    // The member is told about it without asking.
    let seen = next_emojis(&mut member).await;
    assert_eq!(seen.len(), 1);
    let image = seen[0].image.clone();

    // ...but the bytes only arrive when requested.
    member
        .send(&ClientMessage::FetchEmoji { images: vec![image.clone()] })
        .await
        .unwrap();
    let blobs = loop {
        if let ServerMessage::EmojiBlobs { blobs } = next_timeout(&mut member).await {
            break blobs;
        }
    };
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].image, image);
    assert_eq!(blobs[0].data_url, PIXEL_PNG, "round-tripped image should be byte-identical");

    handle.abort();
}

#[tokio::test]
async fn shortcodes_are_validated_and_unique() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _text) = create_guild(&mut owner, "Picky").await;

    // Uppercase and punctuation are outside the allowed set.
    owner
        .send(&ClientMessage::CreateGuildEmoji {
            guild_id,
            shortcode: "Not Valid!".into(),
            image: PIXEL_PNG.into(),
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("shortcode"));

    owner
        .send(&ClientMessage::CreateGuildEmoji {
            guild_id,
            shortcode: "dup".into(),
            image: PIXEL_PNG.into(),
        })
        .await
        .unwrap();
    assert_eq!(next_emojis(&mut owner).await.len(), 1);

    owner
        .send(&ClientMessage::CreateGuildEmoji {
            guild_id,
            shortcode: "dup".into(),
            image: PIXEL_PNG.into(),
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("already exists"));

    // A non-image payload never reaches the blob store.
    owner
        .send(&ClientMessage::CreateGuildEmoji {
            guild_id,
            shortcode: "bogus".into(),
            image: "not-a-data-url".into(),
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("image"));

    handle.abort();
}
