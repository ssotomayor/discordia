//! Server Settings: only an operator reads or changes them, every member hears
//! the name and the ceilings, the ceilings hold guilds to them, and what the
//! launch configuration set wins over the dialog.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{
    ClientMessage, FilePolicy, GuildCreation, Id, ServerInfo, ServerMessage, ServerSettings,
    ServerSettingsView,
};
use discordia_server::server_settings::LaunchSettings;

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn temp_data_dir() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "discordia-server-settings-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

async fn spawn_in(
    dir: PathBuf,
    operator: &BotIdentity,
    launch: LaunchSettings,
) -> (String, discordia_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: HashSet::from([operator.pubkey().to_string()]),
        identities: Default::default(),
        launch,
        data_dir: dir,
        admission: Default::default(),
    };
    let handle = discordia_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn");
    (format!("ws://{}", handle.addr), handle)
}

async fn arrive(url: &str, identity: &BotIdentity, name: &str) -> (Bot, ServerInfo, Vec<Id>) {
    let mut bot = Bot::connect_as_user(url, identity, name).await.unwrap();
    loop {
        if let ServerMessage::Ready { server, guilds, .. } = next_timeout(&mut bot).await {
            return (bot, server, guilds.iter().map(|g| g.id).collect());
        }
    }
}

async fn fetch(bot: &mut Bot) -> Result<ServerSettingsView, String> {
    bot.send(&ClientMessage::FetchServerSettings).await.unwrap();
    loop {
        match next_timeout(bot).await {
            ServerMessage::ServerSettings { view } => return Ok(view),
            ServerMessage::Error { message } => return Err(message),
            _ => continue,
        }
    }
}

async fn save(bot: &mut Bot, settings: ServerSettings) -> Result<ServerSettingsView, String> {
    bot.send(&ClientMessage::SetServerSettings { settings })
        .await
        .unwrap();
    loop {
        match next_timeout(bot).await {
            ServerMessage::ServerSettings { view } => return Ok(view),
            ServerMessage::Error { message } => return Err(message),
            _ => continue,
        }
    }
}

async fn error_after(bot: &mut Bot, msg: ClientMessage) -> String {
    bot.send(&msg).await.unwrap();
    loop {
        if let ServerMessage::Error { message } = next_timeout(bot).await {
            return message;
        }
    }
}

fn tightened() -> ServerSettings {
    let mut settings = ServerSettings {
        name: Some("Ana's place".into()),
        ..ServerSettings::default()
    };
    settings.limits.max_upload_bytes = 100_000;
    settings.limits.max_message_retention_days = Some(30);
    settings.limits.guild_creation = GuildCreation::Operators;
    settings
}

#[tokio::test]
async fn only_an_operator_changes_them_and_every_member_hears() {
    let operator_id = BotIdentity::generate();
    let (url, _h) = spawn_in(temp_data_dir(), &operator_id, Default::default()).await;
    let (mut operator, info, _) = arrive(&url, &operator_id, "Op").await;
    assert_eq!(info, ServerInfo::default());
    let (mut member, _, lobby) = arrive(&url, &BotIdentity::generate(), "Member").await;

    assert!(fetch(&mut member).await.is_err());
    assert!(
        save(&mut member, tightened()).await.is_err(),
        "a member cannot change the server"
    );
    assert_eq!(
        fetch(&mut operator).await.unwrap().settings,
        ServerSettings::default()
    );

    let view = save(&mut operator, tightened()).await.unwrap();
    assert_eq!(view.settings, tightened());
    assert!(!view.pinned.name);

    let heard = loop {
        if let ServerMessage::ServerInfo(info) = next_timeout(&mut member).await {
            break info;
        }
    };
    assert_eq!(heard.name.as_deref(), Some("Ana's place"));
    assert_eq!(heard.limits.max_upload_bytes, 100_000);

    let refused = error_after(
        &mut member,
        ClientMessage::CreateGuild {
            name: "Mine".into(),
            template: None,
        },
    )
    .await;
    assert!(refused.contains("operator"), "{refused}");

    let lobby = lobby[0];
    let over = error_after(
        &mut operator,
        ClientMessage::SetGuildFilePolicy {
            guild_id: lobby,
            policy: FilePolicy::default(),
        },
    )
    .await;
    assert!(over.contains("server allows"), "{over}");
    let longer = error_after(
        &mut operator,
        ClientMessage::SetGuildRetention {
            guild_id: lobby,
            days: Some(90),
        },
    )
    .await;
    assert!(longer.contains("30 days"), "{longer}");

    let invalid = ServerSettings {
        media_max_bytes: 1,
        ..tightened()
    };
    assert!(save(&mut operator, invalid).await.is_err());
}

#[tokio::test]
async fn the_launch_configuration_wins_and_the_dialog_keeps_the_rest() {
    let dir = temp_data_dir();
    let operator_id = BotIdentity::generate();
    let pinned = LaunchSettings {
        name: Some(Some("From env".into())),
        media_max_bytes: Some(64 << 20),
        ..LaunchSettings::default()
    };
    let (url, handle) = spawn_in(dir.clone(), &operator_id, pinned).await;
    let (mut operator, info, _) = arrive(&url, &operator_id, "Op").await;
    assert_eq!(info.name.as_deref(), Some("From env"));

    let view = save(&mut operator, tightened()).await.unwrap();
    assert!(view.pinned.name && view.pinned.media_max_bytes);
    assert_eq!(view.settings.name.as_deref(), Some("From env"));
    assert_eq!(view.settings.media_max_bytes, 64 << 20);
    assert_eq!(view.settings.limits, tightened().limits);
    drop(operator);
    handle.abort();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (url, _h) = spawn_in(dir, &operator_id, Default::default()).await;
    let (mut operator, info, _) = arrive(&url, &operator_id, "Op").await;
    assert_eq!(
        info.name, None,
        "a name typed while the variable pinned it was never the operator's to keep"
    );
    let view = fetch(&mut operator).await.unwrap();
    assert_eq!(view.settings.limits, tightened().limits);
    assert_eq!(
        view.settings.media_max_bytes,
        ServerSettings::default().media_max_bytes
    );
}
