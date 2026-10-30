use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine;
use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{
    ActivityCapability, ActivityManifest, AppManifest, BotInstall, ChannelKind, ClientMessage,
    GuildLook, Id, Intent, Permission, ServerMessage,
};
use sha2::{Digest, Sha256};

async fn next_timeout(bot: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), bot.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

async fn spawn_gateway() -> (String, discordia_server::ServerHandle) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "discordia-apps-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let config = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
        admission: Default::default(),
    };
    let preferred: SocketAddr = "127.0.0.1:19400".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 100, config)
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

fn data_url(bytes: &[u8]) -> String {
    format!(
        "data:{};base64,{}",
        discordia_server::protocol::BUNDLE_MIME,
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

async fn blobs(session: &mut Bot, image: &str) -> Vec<String> {
    session
        .send(&ClientMessage::FetchEmoji {
            images: vec![image.to_string()],
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::EmojiBlobs { blobs } = next_timeout(session).await {
            return blobs.into_iter().map(|b| b.data_url).collect();
        }
    }
}

#[tokio::test]
async fn a_bundle_is_held_only_when_it_is_the_one_the_manifest_names() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, guild_id) = owner_with_guild(&url).await;
    let page = b"<!doctype html><p>rock</p>".to_vec();
    let sha = hex::encode(Sha256::digest(&page));
    let app = BotIdentity::generate();
    let mut m = manifest(1, vec![]);
    m.activity.as_mut().unwrap().sha256 = sha.clone();
    owner
        .send(&install(guild_id, &app, vec![], app.sign_manifest(&m)))
        .await
        .unwrap();
    integrations(&mut owner, app.pubkey()).await;

    owner
        .send(&ClientMessage::FetchGuildActivities { guild_id })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildActivities { activities, .. } = next_timeout(&mut owner).await {
            assert!(activities.is_empty(), "no bundle yet, so nothing to open");
            break;
        }
    }

    let upload = |bytes: &[u8]| ClientMessage::UploadActivityBundle {
        guild_id,
        bot_pubkey: app.pubkey().to_string(),
        data_url: data_url(bytes),
    };
    owner
        .send(&upload(b"<p>a different page</p>"))
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("not the bundle"));
    owner
        .send(&upload(b"\x89PNG\r\n\x1a\nnot a page"))
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("invalid file"));

    owner.send(&upload(&page)).await.unwrap();
    let listed = loop {
        if let ServerMessage::GuildActivities { activities, .. } = next_timeout(&mut owner).await {
            break activities;
        }
    };
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].sha256, sha);
    assert_eq!(listed[0].max_players, 2);

    let blob = discordia_server::protocol::bundle_blob(&sha);
    let got = blobs(&mut owner, &blob).await;
    assert_eq!(got, vec![data_url(&page)]);

    let stranger_id = BotIdentity::generate();
    let mut stranger = Bot::connect_as_user(&url, &stranger_id, "Stranger")
        .await
        .unwrap();
    while !matches!(
        next_timeout(&mut stranger).await,
        ServerMessage::Ready { .. }
    ) {}
    assert!(
        blobs(&mut stranger, &blob).await.is_empty(),
        "a non-member gets no bundle"
    );
    stranger
        .send(&ClientMessage::FetchGuildActivities { guild_id })
        .await
        .unwrap();
    assert!(next_error(&mut stranger).await.contains("not a member"));

    handle.abort();
}

#[tokio::test]
async fn a_followed_blocklist_refuses_an_install_and_only_a_newer_list_replaces_it() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, guild_id) = owner_with_guild(&url).await;
    let curator = BotIdentity::generate();
    let (bad, good) = (BotIdentity::generate(), BotIdentity::generate());

    owner
        .send(&ClientMessage::UploadBlocklist {
            event: curator.sign_blocklist(&[bad.pubkey()], 100),
        })
        .await
        .unwrap();
    assert!(
        next_error(&mut owner)
            .await
            .contains("no guild here follows")
    );

    owner
        .send(&ClientMessage::SetGuildBlocklists {
            guild_id,
            curators: vec![curator.pubkey().to_string()],
        })
        .await
        .unwrap();
    let lists = async |owner: &mut Bot| loop {
        if let ServerMessage::GuildBlocklists {
            curators, lists, ..
        } = next_timeout(owner).await
        {
            break (curators, lists);
        }
    };
    let (curators, held) = lists(&mut owner).await;
    assert_eq!(curators, vec![curator.pubkey().to_string()]);
    assert!(
        held.is_empty(),
        "following is not holding: the list arrives from a client"
    );

    owner
        .send(&ClientMessage::UploadBlocklist {
            event: curator.sign_blocklist(&[bad.pubkey()], 100),
        })
        .await
        .unwrap();
    let (_, held) = lists(&mut owner).await;
    assert_eq!(held[0].blocked, vec![bad.pubkey().to_string()]);

    owner
        .send(&install(
            guild_id,
            &bad,
            vec![],
            bad.sign_manifest(&manifest(1, vec![])),
        ))
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("blocklist"));
    owner
        .send(&install(
            guild_id,
            &good,
            vec![],
            good.sign_manifest(&manifest(1, vec![])),
        ))
        .await
        .unwrap();
    integrations(&mut owner, good.pubkey()).await;

    owner
        .send(&ClientMessage::UploadBlocklist {
            event: curator.sign_blocklist(&[], 50),
        })
        .await
        .unwrap();
    owner
        .send(&install(
            guild_id,
            &bad,
            vec![],
            bad.sign_manifest(&manifest(1, vec![])),
        ))
        .await
        .unwrap();
    assert!(
        next_error(&mut owner).await.contains("blocklist"),
        "an older list changes nothing"
    );

    let mut forged: serde_json::Value =
        serde_json::from_str(&curator.sign_blocklist(&[bad.pubkey()], 200)).unwrap();
    forged["tags"] = serde_json::json!([["d", "discordia-app-blocklist"]]);
    owner
        .send(&ClientMessage::UploadBlocklist {
            event: forged.to_string(),
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("id does not match"));

    handle.abort();
}

#[tokio::test]
async fn an_app_delivers_its_own_bundle_when_a_guild_wants_it() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, guild_id) = owner_with_guild(&url).await;
    let page = b"<!doctype html><p>wizards</p>".to_vec();
    let sha = hex::encode(Sha256::digest(&page));
    let app = BotIdentity::generate();

    // Not installed anywhere: never asked, and an offer stores nothing.
    let mut bot = Bot::connect(&url, &app, "Wizards").await.unwrap();
    while !matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {}
    bot.offer_bundle(&page).await.unwrap();
    assert!(
        next_error(&mut bot)
            .await
            .contains("no guild here has installed")
    );

    let mut m = manifest(1, vec![]);
    m.activity.as_mut().unwrap().sha256 = sha.clone();
    owner
        .send(&install(guild_id, &app, vec![], app.sign_manifest(&m)))
        .await
        .unwrap();
    let wanted = loop {
        if let ServerMessage::BundleWanted { sha256 } = next_timeout(&mut bot).await {
            break sha256;
        }
    };
    assert_eq!(
        wanted, sha,
        "the online bot is asked for exactly the approved bytes"
    );

    bot.offer_bundle(b"<p>something else</p>").await.unwrap();
    assert!(
        next_error(&mut bot)
            .await
            .contains("isn't the one any guild approved")
    );

    bot.offer_bundle(&page).await.unwrap();
    let listed = loop {
        if let ServerMessage::GuildActivities { activities, .. } = next_timeout(&mut owner).await
            && !activities.is_empty()
        {
            break activities;
        }
    };
    assert_eq!(
        listed[0].sha256, sha,
        "members see it with no upload by anyone"
    );

    drop(bot);
    let mut again = Bot::connect(&url, &app, "Wizards").await.unwrap();
    while !matches!(next_timeout(&mut again).await, ServerMessage::Ready { .. }) {}
    assert!(
        tokio::time::timeout(Duration::from_millis(400), async {
            loop {
                if let Some(ServerMessage::BundleWanted { .. }) = again.next_event().await {
                    return;
                }
            }
        })
        .await
        .is_err(),
        "a held bundle is not asked for again"
    );

    handle.abort();
}

async fn member_join(owner: &mut Bot, pubkey: &str) -> discordia_server::protocol::Member {
    loop {
        match next_timeout(owner).await {
            ServerMessage::MemberJoin(m) if m.user.pubkey == pubkey => return m,
            ServerMessage::Error { message } => panic!("unexpected error: {message}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_bot_connected_before_its_install_is_seated_online_and_renamed_by_its_manifest() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, guild_id) = owner_with_guild(&url).await;
    let app = BotIdentity::generate();
    let mut bot = Bot::connect(&url, &app, "Early").await.unwrap();
    while !matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {}

    let v1 = app.sign_manifest(&manifest(1, vec![Permission::SendMessages]));
    owner
        .send(&install(guild_id, &app, vec![Permission::SendMessages], v1))
        .await
        .unwrap();
    let seated = member_join(&mut owner, app.pubkey()).await;
    assert!(seated.online, "its socket is already open");
    assert_eq!(seated.user.username, "Rock Paper Scissors");

    let mut v2 = manifest(2, vec![Permission::SendMessages]);
    v2.name = "Rock Paper Lizard".into();
    bot.register_manifest(&app, &v2).await.unwrap();
    let renamed = member_join(&mut owner, app.pubkey()).await;
    assert_eq!(renamed.user.username, "Rock Paper Lizard");
    assert!(renamed.online);
    assert_eq!(
        integrations(&mut owner, app.pubkey()).await.name,
        "Rock Paper Lizard"
    );

    handle.abort();
}

fn set_look(guild_id: Id, app: &BotIdentity, name: &str, look: GuildLook) -> ClientMessage {
    ClientMessage::SetAppLook {
        guild_id,
        bot_pubkey: app.pubkey().to_string(),
        name: name.into(),
        look,
    }
}

#[tokio::test]
async fn a_guild_dresses_an_app_for_itself_alone_and_a_new_version_keeps_its_name() {
    const PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGM4IbfgPwAFvgKGCAEh9QAAAABJRU5ErkJggg==";
    let (url, handle) = spawn_gateway().await;
    let (mut owner, guild_id) = owner_with_guild(&url).await;
    let (mut other, other_guild) = owner_with_guild(&url).await;
    let app = BotIdentity::generate();
    let v1 = app.sign_manifest(&manifest(1, vec![Permission::SendMessages]));
    for (session, guild) in [(&mut owner, guild_id), (&mut other, other_guild)] {
        session
            .send(&install(
                guild,
                &app,
                vec![Permission::SendMessages],
                v1.clone(),
            ))
            .await
            .unwrap();
        member_join(session, app.pubkey()).await;
    }

    owner
        .send(&set_look(
            guild_id,
            &app,
            "Our Bot",
            GuildLook {
                avatar: Some(PNG.into()),
                banner: None,
                bio: Some("  Ours  ".into()),
            },
        ))
        .await
        .unwrap();
    let dressed = member_join(&mut owner, app.pubkey()).await;
    assert_eq!(dressed.user.username, "Our Bot");
    let avatar = dressed.look.avatar.clone().expect("a picture");
    assert!(
        avatar.starts_with("media:"),
        "stored, not inlined: {avatar}"
    );
    assert_eq!(dressed.look.bio.as_deref(), Some("Ours"));
    assert_eq!(
        integrations(&mut owner, app.pubkey()).await.look,
        dressed.look
    );

    other
        .send(&set_look(guild_id, &app, "Theirs", GuildLook::default()))
        .await
        .unwrap();
    assert!(next_error(&mut other).await.contains("permission"));

    let mut bot = Bot::connect(&url, &app, "App").await.unwrap();
    let mut v2 = manifest(2, vec![Permission::SendMessages]);
    v2.name = "Rock Paper Lizard".into();
    bot.register_manifest(&app, &v2).await.unwrap();
    // Coming online announces the row too, still under the old name.
    let theirs = loop {
        let m = member_join(&mut other, app.pubkey()).await;
        if m.user.username != "Rock Paper Scissors" {
            break m;
        }
    };
    assert_eq!(theirs.user.username, "Rock Paper Lizard");
    assert_eq!(
        theirs.look,
        GuildLook::default(),
        "only one guild dressed it"
    );
    let ours = loop {
        let i = integrations(&mut owner, app.pubkey()).await;
        if i.app.as_ref().is_some_and(|m| m.version == 2) {
            break i;
        }
    };
    assert_eq!(ours.name, "Our Bot", "the guild chose it");
    assert_eq!(ours.look, dressed.look);

    owner
        .send(&set_look(guild_id, &app, " ", GuildLook::default()))
        .await
        .unwrap();
    let plain = member_join(&mut owner, app.pubkey()).await;
    assert_eq!(
        plain.user.username, "Rock Paper Lizard",
        "back to the app's own"
    );
    assert_eq!(plain.look, GuildLook::default());

    handle.abort();
}
