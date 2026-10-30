use std::net::SocketAddr;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{
    BotCommand, ChannelKind, ClientMessage, Id, Intent, Permission, ServerMessage,
};

async fn next_timeout(bot: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), bot.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

async fn next_error(bot: &mut Bot) -> String {
    loop {
        if let ServerMessage::Error { message } = next_timeout(bot).await {
            return message;
        }
    }
}

fn test_config() -> discordia_server::ServerConfig {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "discordia-test-installs-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    }
}

struct World {
    url: String,
    handle: discordia_server::ServerHandle,
    owner: Bot,
    owner_id: BotIdentity,
    guild_id: Id,
    text: Id,
}

async fn world() -> World {
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 100, test_config())
        .await
        .expect("spawn server");
    let url = format!("ws://{}", handle.addr);
    let owner_id = BotIdentity::generate();
    let mut owner = user(&url, &owner_id, "Owner").await;
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Apps".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild_id, text) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut owner).await
        {
            let text = channels
                .iter()
                .find(|c| c.kind == ChannelKind::Text)
                .unwrap()
                .id;
            break (guild.id, text);
        }
    };
    World {
        url,
        handle,
        owner,
        owner_id,
        guild_id,
        text,
    }
}

async fn user(url: &str, id: &BotIdentity, name: &str) -> Bot {
    let mut bot = Bot::connect_as_user(url, id, name).await.unwrap();
    while !matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {}
    bot
}

async fn member(w: &World, name: &str) -> (Bot, BotIdentity) {
    let id = BotIdentity::generate();
    let mut m = user(&w.url, &id, name).await;
    m.send(&ClientMessage::JoinGuild {
        guild_id: w.guild_id,
        accept: true,
        pow_nonce: None,
    })
    .await
    .unwrap();
    while !matches!(
        next_timeout(&mut m).await,
        ServerMessage::GuildJoined { .. }
    ) {}
    (m, id)
}

fn install(w: &World, key: &str, intents: Vec<Intent>) -> ClientMessage {
    ClientMessage::InstallBot {
        guild_id: w.guild_id,
        bot_pubkey: key.to_string(),
        name: "Impostor".into(),
        permissions: vec![Permission::ReadMessageHistory],
        intents,
        manifest: None,
    }
}

async fn installed(w: &mut World, key: &str, intents: Vec<Intent>) {
    let msg = install(w, key, intents);
    w.owner.send(&msg).await.unwrap();
    loop {
        if let ServerMessage::GuildIntegrations { bots, .. } = next_timeout(&mut w.owner).await
            && bots.iter().any(|b| b.bot_pubkey == key)
        {
            return;
        }
    }
}

#[tokio::test]
async fn an_install_is_refused_a_key_that_is_not_a_key() {
    let mut w = world().await;
    for bad in [
        "x".repeat(100_000),
        BotIdentity::generate().pubkey().to_uppercase(),
        String::new(),
    ] {
        let msg = install(&w, &bad, vec![Intent::GuildMessages]);
        w.owner.send(&msg).await.unwrap();
        let err = next_error(&mut w.owner).await;
        assert!(err.contains("64 lowercase hex"), "got: {err}");
    }
    w.handle.abort();
}

#[tokio::test]
async fn an_install_cannot_take_over_a_persons_row() {
    let mut w = world().await;
    let (_m, m_id) = member(&w, "Mallory").await;
    let msg = install(&w, m_id.pubkey(), vec![Intent::GuildMessages]);
    w.owner.send(&msg).await.unwrap();
    let err = next_error(&mut w.owner).await;
    assert!(err.contains("belongs to a member"), "got: {err}");

    w.owner
        .send(&ClientMessage::KickMember {
            guild_id: w.guild_id,
            user_pubkey: m_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    loop {
        match next_timeout(&mut w.owner).await {
            ServerMessage::MemberRemove { user_pubkey, .. } if user_pubkey == m_id.pubkey() => {
                break;
            }
            ServerMessage::Error { message } => panic!("kick refused: {message}"),
            _ => {}
        }
    }
    w.handle.abort();
}

#[tokio::test]
async fn uninstalling_removes_only_an_app() {
    let mut w = world().await;
    let (mut m, m_id) = member(&w, "Mod").await;
    for key in [m_id.pubkey(), w.owner_id.pubkey()] {
        w.owner
            .send(&ClientMessage::UninstallBot {
                guild_id: w.guild_id,
                bot_pubkey: key.to_string(),
            })
            .await
            .unwrap();
        let err = next_error(&mut w.owner).await;
        assert!(err.contains("isn't installed"), "got: {err}");
    }
    m.send_message(w.text, "still here").await.unwrap();
    loop {
        match next_timeout(&mut m).await {
            ServerMessage::MessageCreate(msg) if msg.content == "still here" => break,
            ServerMessage::Error { message } => panic!("member lost: {message}"),
            _ => {}
        }
    }
    w.handle.abort();
}

#[tokio::test]
async fn edits_and_replies_carry_no_text_to_a_bot_without_the_content_intent() {
    let mut w = world().await;
    let bot_id = BotIdentity::generate();
    installed(&mut w, bot_id.pubkey(), vec![Intent::GuildMessages]).await;
    let mut bot = Bot::connect(&w.url, &bot_id, "Quiet").await.unwrap();
    while !matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {}

    w.owner.send_message(w.text, "secret-A").await.unwrap();
    let id = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut w.owner).await {
            break m.id;
        }
    };
    w.owner.edit_message(w.text, id, "secret-B").await.unwrap();
    w.owner.reply_message(w.text, "x", id).await.unwrap();

    let mut saw_update = false;
    loop {
        match next_timeout(&mut bot).await {
            ServerMessage::MessageUpdate { content, .. } => {
                assert_eq!(content, "");
                saw_update = true;
            }
            ServerMessage::MessageCreate(m) if m.reply_to.is_some() => {
                assert_eq!(m.content, "");
                assert_eq!(m.reply_to.unwrap().excerpt, "");
                break;
            }
            _ => {}
        }
    }
    assert!(saw_update, "the edit reached the bot");

    bot.fetch_messages(w.text, 50, None).await.unwrap();
    let history = loop {
        if let ServerMessage::MessageHistory { messages, .. } = next_timeout(&mut bot).await {
            break messages;
        }
    };
    assert_eq!(history.len(), 2);
    for m in history {
        assert_eq!(m.content, "");
        assert!(m.reply_to.is_none_or(|r| r.excerpt.is_empty()));
    }
    w.handle.abort();
}

async fn declare_and_leave(url: &str, id: &BotIdentity) {
    let mut bot = Bot::connect(url, id, "Declarer").await.unwrap();
    while !matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {}
    bot.register_commands(vec![BotCommand::new("ping", "Ping")])
        .await
        .unwrap();
    // Any reply proves the registration ahead of it was handled.
    bot.fetch_messages(Id::new_v4(), 1, None).await.unwrap();
    next_error(&mut bot).await;
}

#[tokio::test]
async fn an_uninstalled_bots_commands_end_with_its_session() {
    let mut w = world().await;
    let kept = BotIdentity::generate();
    installed(&mut w, kept.pubkey(), vec![Intent::GuildMessages]).await;
    let drifter = BotIdentity::generate();
    declare_and_leave(&w.url, &kept).await;
    declare_and_leave(&w.url, &drifter).await;
    tokio::time::sleep(Duration::from_millis(3_500)).await;

    installed(&mut w, drifter.pubkey(), vec![Intent::GuildMessages]).await;
    let mut fresh = Bot::connect_as_user(&w.url, &w.owner_id, "Owner")
        .await
        .unwrap();
    let commands = loop {
        if let ServerMessage::Ready { bot_commands, .. } = next_timeout(&mut fresh).await {
            break bot_commands;
        }
    };
    let has = |id: &BotIdentity| commands.iter().any(|c| c.bot_pubkey == id.pubkey());
    assert!(has(&kept), "an installed bot keeps its commands offline");
    assert!(
        !has(&drifter),
        "commands outlived an uninstalled bot's session"
    );
    w.handle.abort();
}
