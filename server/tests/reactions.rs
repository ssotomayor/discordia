use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ChannelKind, ClientMessage, Id, ServerMessage};

const PIXEL_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn test_config(operators: std::collections::HashSet<String>) -> dioxusfun_server::ServerConfig {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators,
        identities: Default::default(),
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
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
        .send(&ClientMessage::CreateGuild {
            name: name.into(),
            template: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(owner).await
        {
            let text = channels
                .iter()
                .find(|c| c.kind == ChannelKind::Text)
                .expect("guild has a text channel")
                .id;
            return (guild.id, text);
        }
    }
}

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

async fn post(session: &mut Bot, channel_id: Id, content: &str) -> Id {
    session.send_message(channel_id, content).await.unwrap();
    loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(session).await
            && m.content == content
        {
            return m.id;
        }
    }
}

async fn next_reactions(session: &mut Bot) -> Vec<dioxusfun_server::protocol::Reaction> {
    loop {
        if let ServerMessage::ReactionUpdate { reactions, .. } = next_timeout(session).await {
            return reactions;
        }
    }
}

#[tokio::test]
async fn a_guild_emoji_reacts_by_shortcode_and_an_unknown_one_is_refused() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, text) = create_guild(&mut owner, "Reactland").await;

    owner
        .send(&ClientMessage::CreateGuildEmoji {
            guild_id,
            shortcode: "blobcat".into(),
            image: PIXEL_PNG.into(),
        })
        .await
        .unwrap();
    assert_eq!(next_emojis(&mut owner).await.len(), 1);
    let message_id = post(&mut owner, text, "react to me").await;

    owner.react(text, message_id, ":blobcat:").await.unwrap();
    let reactions = next_reactions(&mut owner).await;
    assert_eq!(reactions.len(), 1);
    assert_eq!(
        reactions[0].emoji, ":blobcat:",
        "the shortcode is stored whole, not cut"
    );
    assert_eq!(reactions[0].users, vec![owner_id.pubkey()]);

    owner.react(text, message_id, ":nope:").await.unwrap();
    assert!(next_error(&mut owner).await.contains("not in this guild"));

    owner
        .react(text, message_id, "👍👍👍👍👍👍👍👍👍👍")
        .await
        .unwrap();
    let reactions = next_reactions(&mut owner).await;
    let cut = reactions
        .iter()
        .find(|r| r.emoji != ":blobcat:")
        .expect("the unicode one");
    assert_eq!(
        cut.emoji.chars().count(),
        8,
        "plain strings are still cut, not refused"
    );

    handle.abort();
}
