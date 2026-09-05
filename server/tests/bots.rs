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
    let preferred: SocketAddr = "127.0.0.1:19000".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, test_config(Default::default()))
        .await
        .expect("spawn server");
    let url = format!("ws://{}", handle.addr);
    (url, handle)
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

#[tokio::test]
async fn bot_install_and_ping_roundtrip() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let mut owner = Bot::connect_as_user(&url, &owner_id, "Owner")
        .await
        .unwrap();
    assert!(matches!(
        next_timeout(&mut owner).await,
        ServerMessage::Ready { .. }
    ));

    let (guild_id, text_channel) = create_guild(&mut owner, "Test Guild").await;

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

    let mut bot = Bot::connect(&url, &bot_id, "PingBot").await.unwrap();
    let bot_guilds = loop {
        if let ServerMessage::Ready { guilds, .. } = next_timeout(&mut bot).await {
            break guilds;
        }
    };
    assert_eq!(bot_guilds.len(), 1, "bot only sees its installed guild");
    assert_eq!(bot_guilds[0].id, guild_id);

    owner.send_message(text_channel, "!ping").await.unwrap();
    let got = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut bot).await
            && m.channel_id == text_channel
        {
            break m;
        }
    };
    assert_eq!(got.content, "!ping");

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

#[tokio::test]
async fn an_installed_bot_posts_under_the_name_its_installer_chose() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let mut owner = Bot::connect_as_user(&url, &owner_id, "Owner")
        .await
        .unwrap();
    assert!(matches!(
        next_timeout(&mut owner).await,
        ServerMessage::Ready { .. }
    ));
    let (guild_id, text_channel) = create_guild(&mut owner, "Impersonation").await;

    let bot_id = BotIdentity::generate();
    owner
        .send(&ClientMessage::InstallBot {
            guild_id,
            bot_pubkey: bot_id.pubkey().to_string(),
            name: "PingBot".into(),
            permissions: vec![Permission::SendMessages],
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

    let mut bot = Bot::connect(&url, &bot_id, "Server Admin").await.unwrap();
    loop {
        if matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {
            break;
        }
    }
    bot.send_message(text_channel, "trust me").await.unwrap();

    let seen = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut owner).await
            && m.author.pubkey == bot_id.pubkey()
        {
            break m;
        }
    };
    assert_eq!(
        seen.author.username, "PingBot",
        "a bot must be shown under the name its installer chose, not the one it picked"
    );

    handle.abort();
}
