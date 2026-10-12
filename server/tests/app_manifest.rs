use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{
    ActivityCapability, ActivityManifest, AppManifest, BotInstall, ChannelKind, ClientMessage, Id,
    Intent, Permission, ServerMessage,
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
        "dioxusfun-apps-{}-{}",
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
    let preferred: SocketAddr = "127.0.0.1:19400".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, config)
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn owner_with_guild(url: &str) -> (Bot, Id) {
    let id = BotIdentity::generate();
    let mut owner = Bot::connect_as_user(url, &id, "Owner").await.unwrap();
    while !matches!(next_timeout(&mut owner).await, ServerMessage::Ready { .. }) {}
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Apps".into(),
            template: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut owner).await
        {
            assert!(channels.iter().any(|c| c.kind == ChannelKind::Text));
            return (owner, guild.id);
        }
    }
}

fn manifest(version: u64, permissions: Vec<Permission>) -> AppManifest {
    AppManifest {
        name: "Rock Paper Scissors".into(),
        description: "Best of three".into(),
        icon: "✊".into(),
        version,
        permissions,
        intents: vec![Intent::GuildMessages],
        commands: Vec::new(),
        activity: Some(ActivityManifest {
            sha256: "ab".repeat(32),
            capabilities: vec![ActivityCapability::UserRead, ActivityCapability::Instance],
            max_players: 2,
            url: None,
        }),
    }
}

async fn integrations(owner: &mut Bot, bot: &str) -> BotInstall {
    loop {
        match next_timeout(owner).await {
            ServerMessage::GuildIntegrations { bots, .. } => {
                if let Some(b) = bots.into_iter().find(|b| b.bot_pubkey == bot) {
                    return b;
                }
            }
            ServerMessage::Error { message } => panic!("unexpected error: {message}"),
            _ => {}
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

fn install(
    guild_id: Id,
    app: &BotIdentity,
    perms: Vec<Permission>,
    event: String,
) -> ClientMessage {
    ClientMessage::InstallBot {
        guild_id,
        bot_pubkey: app.pubkey().to_string(),
        name: String::new(),
        permissions: perms,
        intents: vec![Intent::GuildMessages],
        manifest: Some(event),
    }
}

#[tokio::test]
async fn an_install_pins_its_manifest_and_only_a_wider_update_waits_for_approval() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, guild_id) = owner_with_guild(&url).await;
    let app = BotIdentity::generate();
    let v1 = app.sign_manifest(&manifest(1, vec![Permission::SendMessages]));

    owner
        .send(&install(
            guild_id,
            &app,
            vec![Permission::AddReactions],
            v1.clone(),
        ))
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("does not ask for"));

    let impostor = BotIdentity::generate();
    owner
        .send(&install(guild_id, &impostor, vec![], v1.clone()))
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("different key"));

    owner
        .send(&install(guild_id, &app, vec![Permission::SendMessages], v1))
        .await
        .unwrap();
    let pinned = integrations(&mut owner, app.pubkey()).await;
    assert_eq!(
        pinned.name, "Rock Paper Scissors",
        "an empty name takes the manifest's"
    );
    assert_eq!(pinned.app.as_ref().unwrap().version, 1);

    let mut bot = Bot::connect(&url, &app, "RPS").await.unwrap();
    while !matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {}

    let mut v2 = manifest(2, vec![Permission::SendMessages]);
    v2.description = "Best of five".into();
    bot.register_manifest(&app, &v2).await.unwrap();
    let applied = integrations(&mut owner, app.pubkey()).await;
    assert_eq!(applied.app.as_ref().unwrap().description, "Best of five");
    assert!(applied.pending.is_none());

    let v3 = manifest(3, vec![Permission::SendMessages, Permission::AddReactions]);
    bot.register_manifest(&app, &v3).await.unwrap();
    let waiting = integrations(&mut owner, app.pubkey()).await;
    assert_eq!(waiting.app.as_ref().unwrap().version, 2);
    assert_eq!(waiting.pending.as_ref().unwrap().version, 3);
    assert_eq!(waiting.permissions, vec![Permission::SendMessages]);

    bot.register_manifest(&app, &manifest(1, vec![]))
        .await
        .unwrap();

    owner
        .send(&ClientMessage::ApproveAppUpdate {
            guild_id,
            bot_pubkey: app.pubkey().to_string(),
        })
        .await
        .unwrap();
    let approved = integrations(&mut owner, app.pubkey()).await;
    assert_eq!(approved.app.as_ref().unwrap().version, 3);
    assert!(approved.pending.is_none());
    assert!(approved.permissions.contains(&Permission::AddReactions));

    owner
        .send(&ClientMessage::FetchAppManifest {
            bot_pubkey: app.pubkey().to_string(),
        })
        .await
        .unwrap();
    let latest = loop {
        if let ServerMessage::AppManifestEvent { event, .. } = next_timeout(&mut owner).await {
            break event.expect("the host keeps the latest manifest");
        }
    };
    assert!(latest.contains("\\\"version\\\":3"), "{latest}");

    handle.abort();
}

#[tokio::test]
async fn only_a_bot_registers_a_manifest_and_only_its_own() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, _) = owner_with_guild(&url).await;
    let mine = BotIdentity::generate();
    owner
        .send(&ClientMessage::RegisterManifest {
            event: mine.sign_manifest(&manifest(1, vec![])),
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("only a bot"));

    let app = BotIdentity::generate();
    let mut bot = Bot::connect(&url, &app, "RPS").await.unwrap();
    while !matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {}
    bot.send(&ClientMessage::RegisterManifest {
        event: mine.sign_manifest(&manifest(1, vec![])),
    })
    .await
    .unwrap();
    assert!(next_error(&mut bot).await.contains("different key"));

    let mut forged =
        serde_json::from_str::<serde_json::Value>(&app.sign_manifest(&manifest(1, vec![])))
            .unwrap();
    forged["content"] = serde_json::Value::String(
        serde_json::to_string(&manifest(1, vec![Permission::ManageMessages])).unwrap(),
    );
    bot.send(&ClientMessage::RegisterManifest {
        event: forged.to_string(),
    })
    .await
    .unwrap();
    assert!(next_error(&mut bot).await.contains("id does not match"));

    handle.abort();
}
