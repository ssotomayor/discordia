//! Editing a message: its author only, where they could still post it, and a
//! refused edit hands the author back what is stored.

use std::net::SocketAddr;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{ChannelKind, ClientMessage, Id, ServerMessage};

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
        "discordia-edit-{}-{}",
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

async fn connect(url: &str, name: &str) -> Bot {
    let mut bot = Bot::connect_as_user(url, &BotIdentity::generate(), name)
        .await
        .unwrap();
    loop {
        if matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {
            return bot;
        }
    }
}

/// A guild with one text channel, and a second member in it.
async fn setup(url: &str) -> (Bot, Bot, Id) {
    let mut owner = connect(url, "Owner").await;
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Edits".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild_id, channel) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut owner).await
        {
            let text = channels.into_iter().find(|c| c.kind == ChannelKind::Text);
            break (guild.id, text.unwrap());
        }
    };
    let mut member = connect(url, "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut member).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }
    (owner, member, channel.id)
}

async fn post(bot: &mut Bot, channel: Id, text: &str) -> Id {
    bot.send_message(channel, text).await.unwrap();
    loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(bot).await
            && m.content == text
        {
            return m.id;
        }
    }
}

async fn next_update(bot: &mut Bot) -> (Id, String, bool) {
    loop {
        if let ServerMessage::MessageUpdate {
            message_id,
            content,
            edited_at,
            ..
        } = next_timeout(bot).await
        {
            return (message_id, content, edited_at.is_some());
        }
    }
}

async fn next_error(bot: &mut Bot) -> String {
    loop {
        if let ServerMessage::Error { message } = next_timeout(bot).await {
            return message;
        }
    }
}

#[tokio::test]
async fn the_author_edits_and_everyone_in_the_channel_sees_it_marked() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, mut member, channel) = setup(&url).await;
    let id = post(&mut member, channel, "helo").await;

    member.edit_message(channel, id, "  hello  ").await.unwrap();
    assert_eq!(next_update(&mut member).await, (id, "hello".into(), true));
    assert_eq!(next_update(&mut owner).await, (id, "hello".into(), true));

    owner
        .send(&ClientMessage::FetchMessages {
            channel_id: channel,
            limit: 10,
            before_ms: None,
        })
        .await
        .unwrap();
    let history = loop {
        if let ServerMessage::MessageHistory { messages, .. } = next_timeout(&mut owner).await {
            break messages;
        }
    };
    let stored = history.iter().find(|m| m.id == id).unwrap();
    assert_eq!(stored.content, "hello");
    assert!(stored.edited_at.is_some(), "history carries the mark too");
    handle.abort();
}

#[tokio::test]
async fn nobody_else_edits_it_and_a_refusal_puts_the_text_back() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, mut member, channel) = setup(&url).await;
    let id = post(&mut member, channel, "mine").await;

    owner.edit_message(channel, id, "yours now").await.unwrap();
    assert!(next_error(&mut owner).await.contains("only the author"));

    member.edit_message(channel, id, "   ").await.unwrap();
    assert!(next_error(&mut member).await.contains("needs text"));
    assert_eq!(
        next_update(&mut member).await,
        (id, "mine".into(), false),
        "what is stored comes back, never edited"
    );

    owner
        .send(&ClientMessage::UpdateChannel {
            channel_id: channel,
            divider: None,
            name: "general".into(),
            topic: None,
            read_only: true,
            position: 0,
            slowmode_secs: 0,
            markdown: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ChannelUpdate(c) = next_timeout(&mut member).await
            && c.read_only
        {
            break;
        }
    }
    member.edit_message(channel, id, "sneaky").await.unwrap();
    assert!(next_error(&mut member).await.contains("read-only"));
    handle.abort();
}
