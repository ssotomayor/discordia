use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{
    ArgValue, BotCommand, BotCommandSet, ChannelKind, ClientMessage, CommandArg, CommandOption, Id,
    Intent, Invocation, Permission, ServerMessage,
};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

async fn spawn_gateway() -> (String, dioxusfun_server::ServerHandle) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-commands-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let cfg = dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    };
    let preferred: SocketAddr = "127.0.0.1:19700".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn connect_user(url: &str, id: &BotIdentity, name: &str) -> Bot {
    let mut session = Bot::connect_as_user(url, id, name).await.unwrap();
    loop {
        if matches!(
            next_timeout(&mut session).await,
            ServerMessage::Ready { .. }
        ) {
            return session;
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

async fn next_invocation(bot: &mut Bot) -> Invocation {
    loop {
        if let ServerMessage::CommandInvoked(inv) = next_timeout(bot).await {
            return inv;
        }
    }
}

async fn next_commands(session: &mut Bot) -> BotCommandSet {
    loop {
        if let ServerMessage::BotCommands(set) = next_timeout(session).await {
            return set;
        }
    }
}

fn commands() -> Vec<BotCommand> {
    vec![
        BotCommand::new("ping", "Ping"),
        BotCommand::new("rotate", "Rotate").requires(Permission::ManageGuild),
        BotCommand::new("pacifista", "Pacifista")
            .option(CommandOption::members("user", "Usuario", 1, 1)),
    ]
}

struct World {
    url: String,
    handle: dioxusfun_server::ServerHandle,
    owner: Bot,
    owner_key: String,
    bot: Bot,
    bot_id: BotIdentity,
    guild_id: Id,
    text: Id,
    voice: Id,
}

/// A guild with a bot installed, connected, and its commands registered.
async fn world() -> World {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    owner
        .send(&ClientMessage::CreateGuild {
            name: "LPW".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild_id, text, voice) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut owner).await
        {
            let of = |k| channels.iter().find(|c| c.kind == k).map(|c| c.id);
            break (
                guild.id,
                of(ChannelKind::Text).expect("a text channel"),
                of(ChannelKind::Voice).expect("a voice channel"),
            );
        }
    };

    let bot_id = BotIdentity::generate();
    owner
        .send(&ClientMessage::InstallBot {
            guild_id,
            bot_pubkey: bot_id.pubkey().to_string(),
            name: "LecheBot".into(),
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
    let mut bot = Bot::connect(&url, &bot_id, "LecheBot").await.unwrap();
    loop {
        if matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {
            break;
        }
    }
    bot.register_commands(commands()).await.unwrap();
    let set = next_commands(&mut owner).await;
    assert_eq!(set.bot_pubkey, bot_id.pubkey());
    assert_eq!(set.commands.len(), 3);

    World {
        url,
        handle,
        owner,
        owner_key: owner_id.pubkey().to_string(),
        bot,
        bot_id,
        guild_id,
        text,
        voice,
    }
}

async fn join(url: &str, guild_id: Id, name: &str) -> (Bot, BotIdentity, Option<BotCommandSet>) {
    let id = BotIdentity::generate();
    let mut guest = connect_user(url, &id, name).await;
    guest
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut guest).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }
    let set = tokio::time::timeout(Duration::from_secs(2), next_commands(&mut guest))
        .await
        .ok();
    (guest, id, set)
}

fn invoke(w: &World, channel_id: Id, command: &str, args: Vec<CommandArg>) -> ClientMessage {
    ClientMessage::InvokeCommand {
        guild_id: w.guild_id,
        channel_id,
        bot_pubkey: w.bot_id.pubkey().to_string(),
        command: command.into(),
        args,
    }
}

#[tokio::test]
async fn a_press_reaches_the_bot_and_its_private_answer_only_the_presser() {
    let mut w = world().await;
    let (mut guest, _, _) = join(&w.url, w.guild_id, "Guest").await;

    w.owner
        .send(&invoke(&w, w.text, "ping", vec![]))
        .await
        .unwrap();
    let inv = next_invocation(&mut w.bot).await;
    assert_eq!(inv.command, "ping");
    assert_eq!(inv.channel_id, w.text);
    assert_eq!(inv.invoker.pubkey, w.owner_key);

    w.bot.respond(&inv, "solo vos").await.unwrap();
    let got = loop {
        if let ServerMessage::CommandResponse {
            content,
            invocation_id,
            ..
        } = next_timeout(&mut w.owner).await
        {
            assert_eq!(invocation_id, inv.id);
            break content;
        }
    };
    assert_eq!(got, "solo vos");

    guest.send_message(w.text, "marker").await.unwrap();
    loop {
        match next_timeout(&mut guest).await {
            ServerMessage::CommandResponse { .. } => panic!("a private reply reached a bystander"),
            ServerMessage::MessageCreate(m) if m.content == "marker" => break,
            _ => {}
        }
    }

    w.bot.respond(&inv, "otra vez").await.unwrap();
    assert!(next_error(&mut w.bot).await.contains("already answered"));
    w.handle.abort();
}

#[tokio::test]
async fn the_server_checks_the_presser_holds_the_commands_permission() {
    let mut w = world().await;
    let (mut guest, _, _) = join(&w.url, w.guild_id, "Guest").await;

    guest
        .send(&invoke(&w, w.text, "rotate", vec![]))
        .await
        .unwrap();
    assert!(next_error(&mut guest).await.contains("manage_guild"));

    w.owner
        .send(&invoke(&w, w.text, "rotate", vec![]))
        .await
        .unwrap();
    assert_eq!(next_invocation(&mut w.bot).await.command, "rotate");
    w.handle.abort();
}

#[tokio::test]
async fn a_picked_member_arrives_named_and_a_stranger_is_refused() {
    let mut w = world().await;
    let (mut guest, _, _) = join(&w.url, w.guild_id, "Guest").await;
    let pick = |key: String| {
        vec![CommandArg {
            id: "user".into(),
            value: ArgValue::Members(vec![key]),
        }]
    };

    let stranger = BotIdentity::generate().pubkey().to_string();
    guest
        .send(&invoke(&w, w.text, "pacifista", pick(stranger)))
        .await
        .unwrap();
    assert!(next_error(&mut guest).await.contains("isn't a member"));

    guest
        .send(&invoke(&w, w.text, "pacifista", pick(w.owner_key.clone())))
        .await
        .unwrap();
    let inv = next_invocation(&mut w.bot).await;
    let picked = inv.members("user");
    assert_eq!(picked.len(), 1);
    assert_eq!(picked[0].username, "Owner");
    w.handle.abort();
}

#[tokio::test]
async fn a_press_is_refused_outside_a_text_channel_and_when_the_bot_is_gone() {
    let mut w = world().await;

    w.owner
        .send(&invoke(&w, w.voice, "ping", vec![]))
        .await
        .unwrap();
    assert!(next_error(&mut w.owner).await.contains("text channels"));

    w.owner
        .send(&invoke(&w, w.text, "nope", vec![]))
        .await
        .unwrap();
    assert!(next_error(&mut w.owner).await.contains("no such command"));

    let ping = invoke(&w, w.text, "ping", vec![]);
    let bot_key = w.bot_id.pubkey().to_string();
    drop(w.bot);
    loop {
        if let ServerMessage::MemberLeave { user_pubkey, .. } = next_timeout(&mut w.owner).await
            && user_pubkey == bot_key
        {
            break;
        }
    }
    w.owner.send(&ping).await.unwrap();
    assert!(next_error(&mut w.owner).await.contains("offline"));
    w.handle.abort();
}

#[tokio::test]
async fn commands_reach_a_late_joiner_and_a_fresh_snapshot() {
    let w = world().await;

    let (late, late_id, joined) = join(&w.url, w.guild_id, "Late").await;
    let joined = joined.expect("a joiner is sent the guild's bot commands");
    assert_eq!(joined.bot_pubkey, w.bot_id.pubkey());

    drop(late);
    let mut session = Bot::connect_as_user(&w.url, &late_id, "Late")
        .await
        .unwrap();
    let listed = loop {
        if let ServerMessage::Ready { bot_commands, .. } = next_timeout(&mut session).await {
            break bot_commands;
        }
    };
    assert!(listed.iter().any(|s| s.bot_pubkey == w.bot_id.pubkey()));
    w.handle.abort();
}

#[tokio::test]
async fn only_bots_declare_and_bots_cannot_press() {
    let mut w = world().await;

    w.owner.register_commands(commands()).await.unwrap();
    assert!(next_error(&mut w.owner).await.contains("only a bot"));

    w.bot
        .send(&invoke(&w, w.text, "ping", vec![]))
        .await
        .unwrap();
    assert!(next_error(&mut w.bot).await.contains("bots may only"));

    w.bot
        .register_commands(vec![BotCommand::new("Bad Id", "x")])
        .await
        .unwrap();
    assert!(next_error(&mut w.bot).await.contains("command id"));
    w.handle.abort();
}
