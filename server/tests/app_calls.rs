use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{
    BotCommand, ChannelKind, ClientMessage, Id, Intent, Invocation, Permission, ServerMessage,
};

async fn next_timeout(bot: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), bot.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

async fn spawn_gateway() -> (String, dioxusfun_server::ServerHandle) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-calls-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let config = dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    };
    let preferred: SocketAddr = "127.0.0.1:19600".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, config)
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn next_error(b: &mut Bot) -> String {
    loop {
        if let ServerMessage::Error { message } = next_timeout(b).await {
            return message;
        }
    }
}

async fn invoked(b: &mut Bot) -> Invocation {
    loop {
        match next_timeout(b).await {
            ServerMessage::CommandInvoked(inv) => return inv,
            ServerMessage::Error { message } => panic!("unexpected error: {message}"),
            _ => {}
        }
    }
}

struct App {
    id: BotIdentity,
    bot: Bot,
}

struct World {
    handle: dioxusfun_server::ServerHandle,
    owner: Bot,
    member: Bot,
    guild_id: Id,
    text: Id,
    apps: Vec<App>,
}

impl World {
    fn key(&self, i: usize) -> String {
        self.apps[i].id.pubkey().to_string()
    }

    async fn allow(&mut self, caller: usize, callees: &[usize]) {
        let may_call = callees.iter().map(|c| self.key(*c)).collect();
        let bot_pubkey = self.key(caller);
        self.owner
            .send(&ClientMessage::SetAppCalls {
                guild_id: self.guild_id,
                bot_pubkey: bot_pubkey.clone(),
                may_call,
            })
            .await
            .unwrap();
        loop {
            if let ServerMessage::GuildIntegrations { bots, .. } =
                next_timeout(&mut self.owner).await
                && bots
                    .iter()
                    .any(|b| b.bot_pubkey == bot_pubkey && b.may_call.len() == callees.len())
            {
                return;
            }
        }
    }

    async fn call(
        &mut self,
        caller: usize,
        parent: Option<&Invocation>,
        callee: usize,
        command: &str,
    ) {
        let (guild_id, text, callee) = (self.guild_id, self.text, self.key(callee));
        self.apps[caller]
            .bot
            .invoke(parent, guild_id, text, &callee, command, vec![])
            .await
            .unwrap();
    }
}

async fn press(by: &mut Bot, guild_id: Id, channel_id: Id, bot_pubkey: String) {
    by.send(&ClientMessage::InvokeCommand {
        guild_id,
        channel_id,
        bot_pubkey,
        command: "relay".into(),
        args: vec![],
        parent: None,
    })
    .await
    .unwrap();
}

fn commands() -> Vec<BotCommand> {
    vec![
        BotCommand::new("relay", "Relay"),
        BotCommand::new("echo", "Echo").callable_by_apps(),
        BotCommand::new("purge", "Purge")
            .requires(Permission::ManageMessages)
            .callable_by_apps(),
        BotCommand::new("secret", "Secret"),
    ]
}

async fn world() -> World {
    let (url, handle) = spawn_gateway().await;
    let connect = async |name: &str| {
        let id = BotIdentity::generate();
        let mut b = Bot::connect_as_user(&url, &id, name).await.unwrap();
        while !matches!(next_timeout(&mut b).await, ServerMessage::Ready { .. }) {}
        b
    };
    let mut owner = connect("Owner").await;
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Agents".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild_id, text) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut owner).await
        {
            break (
                guild.id,
                channels
                    .iter()
                    .find(|c| c.kind == ChannelKind::Text)
                    .unwrap()
                    .id,
            );
        }
    };
    let mut member = connect("Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    while !matches!(
        next_timeout(&mut member).await,
        ServerMessage::GuildJoined { .. }
    ) {}

    let mut apps = Vec::new();
    for (i, perms) in [
        vec![Permission::SendMessages],
        vec![Permission::SendMessages, Permission::ManageMessages],
        vec![Permission::SendMessages],
        vec![Permission::SendMessages],
    ]
    .into_iter()
    .enumerate()
    {
        let id = BotIdentity::generate();
        owner
            .send(&ClientMessage::InstallBot {
                guild_id,
                bot_pubkey: id.pubkey().to_string(),
                name: format!("App{i}"),
                permissions: perms,
                intents: vec![Intent::GuildMessages],
                manifest: None,
            })
            .await
            .unwrap();
        loop {
            if let ServerMessage::GuildIntegrations { bots, .. } = next_timeout(&mut owner).await
                && bots.iter().any(|b| b.bot_pubkey == id.pubkey())
            {
                break;
            }
        }
        let mut bot = Bot::connect(&url, &id, &format!("App{i}")).await.unwrap();
        while !matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {}
        bot.register_commands(commands()).await.unwrap();
        apps.push(App { id, bot });
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    World {
        handle,
        owner,
        member,
        guild_id,
        text,
        apps,
    }
}

#[tokio::test]
async fn an_app_calls_another_only_where_the_guild_and_the_command_allow() {
    let mut w = world().await;
    w.call(0, None, 1, "echo").await;
    assert!(
        next_error(&mut w.apps[0].bot)
            .await
            .contains("hasn't allowed")
    );

    w.allow(0, &[1]).await;
    w.call(0, None, 1, "secret").await;
    assert!(
        next_error(&mut w.apps[0].bot)
            .await
            .contains("can't be called by other apps")
    );

    w.call(0, None, 1, "echo").await;
    let inv = invoked(&mut w.apps[1].bot).await;
    assert_eq!(inv.invoker.pubkey, w.key(0));
    assert_eq!(inv.via, vec![w.key(0)]);
    w.apps[1]
        .bot
        .respond_with_data(&inv, "echoed", "{\"ok\":true}")
        .await
        .unwrap();
    let data = loop {
        if let ServerMessage::CommandResponse {
            data, bot_pubkey, ..
        } = next_timeout(&mut w.apps[0].bot).await
        {
            assert_eq!(bot_pubkey, w.key(1));
            break data;
        }
    };
    assert_eq!(data.as_deref(), Some("{\"ok\":true}"));

    w.handle.abort();
}

#[tokio::test]
async fn a_call_is_held_to_the_calling_app_and_to_the_person_who_started_it() {
    let mut w = world().await;
    w.allow(0, &[1]).await;
    w.allow(2, &[1]).await;

    // App0 lacks Manage messages, so it cannot have App1 purge for it.
    w.call(0, None, 1, "purge").await;
    assert!(next_error(&mut w.apps[0].bot).await.contains("lacks"));

    // Reinstalled with it, App0 still cannot lend a plain member the power.
    let key0 = w.key(0);
    w.owner
        .send(&ClientMessage::InstallBot {
            guild_id: w.guild_id,
            bot_pubkey: key0.clone(),
            name: "App0".into(),
            permissions: vec![Permission::SendMessages, Permission::ManageMessages],
            intents: vec![Intent::GuildMessages],
            manifest: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildIntegrations { bots, .. } = next_timeout(&mut w.owner).await
            && bots
                .iter()
                .any(|b| b.bot_pubkey == key0 && b.has_permission(Permission::ManageMessages))
        {
            assert!(
                bots.iter()
                    .find(|b| b.bot_pubkey == key0)
                    .unwrap()
                    .may_call
                    .contains(&w.key(1)),
                "a reinstall keeps the allowed calls"
            );
            break;
        }
    }
    let (g, t) = (w.guild_id, w.text);
    press(&mut w.member, g, t, key0.clone()).await;
    let from_member = invoked(&mut w.apps[0].bot).await;
    w.call(0, Some(&from_member), 1, "purge").await;
    let refused = next_error(&mut w.apps[0].bot).await;
    assert!(
        refused.contains("permission"),
        "the member at the root lacks it: {refused}"
    );

    press(&mut w.owner, g, t, key0.clone()).await;
    let from_owner = invoked(&mut w.apps[0].bot).await;
    w.call(0, Some(&from_owner), 1, "purge").await;
    let purge = invoked(&mut w.apps[1].bot).await;
    assert_eq!(purge.via.len(), 2);
    assert_eq!(purge.via[1], key0);

    // A parent must be one the caller is handling.
    w.call(2, Some(&from_owner), 1, "echo").await;
    assert!(
        next_error(&mut w.apps[2].bot)
            .await
            .contains("isn't one this app is handling")
    );

    w.handle.abort();
}

#[tokio::test]
async fn chains_cannot_loop_run_deep_or_run_hot() {
    let mut w = world().await;
    w.allow(0, &[1]).await;
    w.allow(1, &[0, 2]).await;
    w.allow(2, &[3]).await;

    let (g, t, key0) = (w.guild_id, w.text, w.key(0));
    press(&mut w.owner, g, t, key0).await;
    let at0 = invoked(&mut w.apps[0].bot).await;
    w.call(0, Some(&at0), 1, "echo").await;
    let at1 = invoked(&mut w.apps[1].bot).await;
    w.call(1, Some(&at1), 0, "echo").await;
    assert!(next_error(&mut w.apps[1].bot).await.contains("loop back"));

    w.call(1, Some(&at1), 2, "echo").await;
    let at2 = invoked(&mut w.apps[2].bot).await;
    assert_eq!(
        at2.via.len(),
        3,
        "the person, then each app it passed through"
    );
    assert_eq!(at2.via[1..], [w.key(0), w.key(1)]);
    w.call(2, Some(&at2), 3, "echo").await;
    assert!(next_error(&mut w.apps[2].bot).await.contains("too deep"));

    // Inside one connection's write budget, so the pair's own limit is what trips.
    for _ in 0..21 {
        w.call(0, None, 1, "echo").await;
    }
    assert!(
        next_error(&mut w.apps[0].bot).await.contains("too often"),
        "the pair is rate limited"
    );

    w.handle.abort();
}
