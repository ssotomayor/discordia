//! Ready and GuildJoined say when each text channel last had a message, and
//! nothing about what anyone read: read marks are the client's alone.

use std::net::SocketAddr;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{ChannelKind, ClientMessage, ServerMessage};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

async fn spawn_gateway() -> (String, discordia_server::ServerHandle) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "discordia-latest-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let cfg = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
        admission: Default::default(),
    };
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn connect(url: &str, identity: &BotIdentity, name: &str) -> (Bot, ServerMessage) {
    let mut bot = Bot::connect_as_user(url, identity, name).await.unwrap();
    loop {
        let event = next_timeout(&mut bot).await;
        if matches!(event, ServerMessage::Ready { .. }) {
            return (bot, event);
        }
    }
}

#[tokio::test]
async fn ready_and_join_carry_each_text_channels_newest_message_time() {
    let (url, handle) = spawn_gateway().await;
    let owner_key = BotIdentity::generate();
    let (mut owner, _) = connect(&url, &owner_key, "Owner").await;
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Marks".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild_id, channels) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut owner).await
        {
            break (guild.id, channels);
        }
    };
    let text = channels
        .iter()
        .find(|c| c.kind == ChannelKind::Text)
        .unwrap()
        .id;
    owner.send_message(text, "first").await.unwrap();
    owner.send_message(text, "second").await.unwrap();
    let newest = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut owner).await
            && m.content == "second"
        {
            break m.created_at.timestamp_millis();
        }
    };
    drop(owner);

    let (_owner, ready) = connect(&url, &owner_key, "Owner").await;
    let ServerMessage::Ready {
        latest_messages, ..
    } = ready
    else {
        unreachable!()
    };
    assert_eq!(latest_messages.get(&text), Some(&newest));
    assert_eq!(
        latest_messages.len(),
        1,
        "a channel with no messages has no time"
    );

    let (mut member, _) = connect(&url, &BotIdentity::generate(), "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    let joined = loop {
        if let ServerMessage::GuildJoined {
            latest_messages, ..
        } = next_timeout(&mut member).await
        {
            break latest_messages;
        }
    };
    assert_eq!(joined.get(&text), Some(&newest));
    handle.abort();
}
