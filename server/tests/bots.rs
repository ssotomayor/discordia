//! End-to-end tests for the Tier 1 bot platform: install → connect → the
//! intent-filtering and permission-gating that make it safe. Both the human
//! owner and the bot are driven through the real gateway over a WebSocket via
//! the bot SDK (which can speak the whole protocol, not just bot helpers).

use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{
    ChannelKind, ClientMessage, Id, Intent, Permission, ServerMessage,
};

async fn next_timeout(bot: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), bot.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

/// Per-test ServerConfig: unique temp data dir (SQLite + media) so tests are
/// hermetic and parallel-safe.
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

/// Spawn a gateway on a free port and return its `ws://` URL plus the handle.
async fn spawn_gateway() -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:19000".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, test_config(Default::default()))
        .await
        .expect("spawn server");
    let url = format!("ws://{}", handle.addr);
    (url, handle)
}

/// Owner creates a guild; returns (guild_id, first text channel id).
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

#[tokio::test]
async fn bot_install_and_ping_roundtrip() {
    let (url, handle) = spawn_gateway().await;

    // Human owner connects and lands on a Ready.
    let owner_id = BotIdentity::generate();
    let mut owner = Bot::connect_as_user(&url, &owner_id, "Owner")
        .await
        .unwrap();
    assert!(matches!(
        next_timeout(&mut owner).await,
        ServerMessage::Ready { .. }
    ));

    let (guild_id, text_channel) = create_guild(&mut owner, "Test Guild").await;

    // Install a bot with send + the privileged message-content intent.
    let bot_id = BotIdentity::generate();
    owner
        .send(&ClientMessage::InstallBot {
            guild_id,
            bot_pubkey: bot_id.pubkey().to_string(),
            name: "PingBot".into(),
            permissions: vec![Permission::SendMessages],
            intents: vec![Intent::GuildMessages, Intent::MessageContent],
        })
        .await
        .unwrap();
    let listed = loop {
        if let ServerMessage::GuildIntegrations { bots, .. } = next_timeout(&mut owner).await {
            break bots;
        }
    };
    assert!(listed.iter().any(|b| b.bot_pubkey == bot_id.pubkey()));

    // The bot connects; its Ready is scoped to exactly the installed guild.
    let mut bot = Bot::connect(&url, &bot_id, "PingBot").await.unwrap();
    let bot_guilds = loop {
        if let ServerMessage::Ready { guilds, .. } = next_timeout(&mut bot).await {
            break guilds;
        }
    };
    assert_eq!(bot_guilds.len(), 1, "bot only sees its installed guild");
    assert_eq!(bot_guilds[0].id, guild_id);

    // Owner posts "!ping" — the bot receives it WITH content (intent granted).
    owner.send_message(text_channel, "!ping").await.unwrap();
    let got = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut bot).await
            && m.channel_id == text_channel
        {
            break m;
        }
    };
    assert_eq!(got.content, "!ping");

    // Bot replies; the owner sees the bot's message.
    bot.send_message(text_channel, "pong").await.unwrap();
    let reply = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut owner).await
            && m.author.pubkey == bot_id.pubkey()
        {
            break m;
        }
    };
    assert_eq!(reply.content, "pong");

    handle.abort();
}

#[tokio::test]
async fn intents_and_permissions_are_enforced() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let mut owner = Bot::connect_as_user(&url, &owner_id, "Owner")
        .await
        .unwrap();
    assert!(matches!(
        next_timeout(&mut owner).await,
        ServerMessage::Ready { .. }
    ));

    let (guild_id, text_channel) = create_guild(&mut owner, "Locked Down").await;

    // Install with message events but NO content intent and NO send permission.
    let bot_id = BotIdentity::generate();
    owner
        .send(&ClientMessage::InstallBot {
            guild_id,
            bot_pubkey: bot_id.pubkey().to_string(),
            name: "QuietBot".into(),
            permissions: vec![],
            intents: vec![Intent::GuildMessages],
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut owner).await,
            ServerMessage::GuildIntegrations { .. }
        ) {
            break;
        }
    }

    let mut bot = Bot::connect(&url, &bot_id, "QuietBot").await.unwrap();
    loop {
        if matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {
            break;
        }
    }

    // Owner posts a secret; the bot is told a message happened but NOT its text.
    owner
        .send_message(text_channel, "the password is hunter2")
        .await
        .unwrap();
    let blanked = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut bot).await
            && m.channel_id == text_channel
        {
            break m;
        }
    };
    assert_eq!(
        blanked.content, "",
        "content withheld without MessageContent intent"
    );
    assert!(blanked.image.is_none());

    // The bot tries to post without the SendMessages permission → rejected.
    bot.send_message(text_channel, "i shouldn't be able to say this")
        .await
        .unwrap();
    let err = loop {
        if let ServerMessage::Error { message } = next_timeout(&mut bot).await {
            break message;
        }
    };
    assert!(err.contains("send_messages"), "got: {err}");

    handle.abort();
}
