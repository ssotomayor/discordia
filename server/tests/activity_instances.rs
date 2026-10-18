use std::net::SocketAddr;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{
    ActivityInstance, ActivityRole, ChannelKind, ClientMessage, Id, ServerMessage,
};

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
        "discordia-instances-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let config = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    };
    let preferred: SocketAddr = "127.0.0.1:19500".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 100, config)
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn user(url: &str, name: &str) -> (Bot, String) {
    let id = BotIdentity::generate();
    let mut b = Bot::connect_as_user(url, &id, name).await.unwrap();
    while !matches!(next_timeout(&mut b).await, ServerMessage::Ready { .. }) {}
    (b, id.pubkey().to_string())
}

struct World {
    url: String,
    handle: discordia_server::ServerHandle,
    alice: (Bot, String),
    bob: (Bot, String),
    carol: (Bot, String),
    guild_id: Id,
    text: Id,
}

async fn world() -> World {
    let (url, handle) = spawn_gateway().await;
    let mut alice = user(&url, "Alice").await;
    alice
        .0
        .send(&ClientMessage::CreateGuild {
            name: "Games".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild_id, text) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut alice.0).await
        {
            let text = channels
                .iter()
                .find(|c| c.kind == ChannelKind::Text)
                .unwrap()
                .id;
            break (guild.id, text);
        }
    };
    let join = async |name: &str| {
        let mut u = user(&url, name).await;
        u.0.send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
        while !matches!(
            next_timeout(&mut u.0).await,
            ServerMessage::GuildJoined { .. }
        ) {}
        u
    };
    let bob = join("Bob").await;
    let carol = join("Carol").await;
    World {
        url,
        handle,
        alice,
        bob,
        carol,
        guild_id,
        text,
    }
}

async fn instances(b: &mut Bot) -> Vec<ActivityInstance> {
    loop {
        match next_timeout(b).await {
            ServerMessage::ActivityInstances { instances, .. } => return instances,
            ServerMessage::Error { message } => panic!("unexpected error: {message}"),
            _ => {}
        }
    }
}

async fn next_error(b: &mut Bot) -> String {
    loop {
        if let ServerMessage::Error { message } = next_timeout(b).await {
            return message;
        }
    }
}

async fn event(b: &mut Bot) -> (String, String) {
    loop {
        if let ServerMessage::ActivityEvent { from, data, .. } = next_timeout(b).await {
            return (from, data);
        }
    }
}

async fn quiet(b: &mut Bot) -> bool {
    loop {
        match tokio::time::timeout(Duration::from_millis(300), b.next_event()).await {
            Err(_) => return true,
            Ok(Some(ServerMessage::ActivityEvent { .. })) => return false,
            Ok(_) => {}
        }
    }
}

#[tokio::test]
async fn rock_paper_scissors_is_joined_watched_and_relayed_only_inside() {
    let mut w = world().await;
    let (alice_key, bob_key, carol_key) = (w.alice.1.clone(), w.bob.1.clone(), w.carol.1.clone());
    let alice = &mut w.alice.0;
    alice
        .send(&ClientMessage::ActivityStart {
            channel_id: w.text,
            app: "builtin:rps".into(),
        })
        .await
        .unwrap();
    let started = loop {
        if let ServerMessage::ActivityStarted { instance } = next_timeout(alice).await {
            break instance;
        }
    };
    assert_eq!(started.max_players, 2);
    assert_eq!(started.host(), Some(alice_key.as_str()));

    let seen = instances(&mut w.carol.0).await;
    assert_eq!(
        seen,
        vec![started.clone()],
        "everyone who can see the channel sees it"
    );
    instances(&mut w.bob.0).await;

    let bob = &mut w.bob.0;
    bob.send(&ClientMessage::ActivityJoin {
        instance_id: started.id,
        role: ActivityRole::Player,
    })
    .await
    .unwrap();
    let now = instances(bob).await;
    assert_eq!(now[0].players(), 2);

    let carol = &mut w.carol.0;
    instances(carol).await;
    carol
        .send(&ClientMessage::ActivityJoin {
            instance_id: started.id,
            role: ActivityRole::Player,
        })
        .await
        .unwrap();
    assert!(next_error(carol).await.contains("full"));
    carol
        .send(&ClientMessage::ActivityJoin {
            instance_id: started.id,
            role: ActivityRole::Spectator,
        })
        .await
        .unwrap();
    let watched = instances(carol).await;
    assert_eq!(
        watched[0].role_of(&carol_key),
        Some(ActivityRole::Spectator)
    );
    carol
        .send(&ClientMessage::ActivitySend {
            instance_id: started.id,
            to: None,
            data: "{\"move\":\"rock\"}".into(),
        })
        .await
        .unwrap();
    assert!(next_error(carol).await.contains("only players"));

    let bob = &mut w.bob.0;
    bob.send(&ClientMessage::ActivitySend {
        instance_id: started.id,
        to: None,
        data: "{\"commit\":\"abc\"}".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        event(&mut w.alice.0).await,
        (bob_key.clone(), "{\"commit\":\"abc\"}".into())
    );
    assert_eq!(event(&mut w.carol.0).await.0, bob_key);

    let (outsider, _) = &mut user(&w.url, "Outsider").await;
    outsider
        .send(&ClientMessage::FetchActivityInstances { channel_id: w.text })
        .await
        .unwrap();
    assert!(
        quiet(outsider).await,
        "someone outside the guild hears nothing"
    );

    w.alice
        .0
        .send(&ClientMessage::ActivitySend {
            instance_id: started.id,
            to: Some(w.bob.1.clone()),
            data: "{\"snapshot\":1}".into(),
        })
        .await
        .unwrap();
    assert_eq!(event(&mut w.bob.0).await.1, "{\"snapshot\":1}");
    assert!(
        quiet(&mut w.carol.0).await,
        "a message to one reaches only that one"
    );

    w.handle.abort();
}

#[tokio::test]
async fn the_host_moves_on_when_it_leaves_and_a_disconnect_cleans_up() {
    let mut w = world().await;
    w.alice
        .0
        .send(&ClientMessage::ActivityStart {
            channel_id: w.text,
            app: "builtin:rps".into(),
        })
        .await
        .unwrap();
    let started = loop {
        if let ServerMessage::ActivityStarted { instance } = next_timeout(&mut w.alice.0).await {
            break instance;
        }
    };
    instances(&mut w.bob.0).await;
    w.bob
        .0
        .send(&ClientMessage::ActivityJoin {
            instance_id: started.id,
            role: ActivityRole::Player,
        })
        .await
        .unwrap();
    instances(&mut w.bob.0).await;

    w.alice
        .0
        .send(&ClientMessage::ActivityLeave {
            instance_id: started.id,
        })
        .await
        .unwrap();
    let after = instances(&mut w.bob.0).await;
    assert_eq!(after[0].host(), Some(w.bob.1.as_str()));

    drop(w.bob.0);
    let gone = loop {
        let list = instances(&mut w.carol.0).await;
        if list.is_empty() {
            break list;
        }
    };
    assert!(gone.is_empty(), "the last participant leaving ends it");

    w.carol
        .0
        .send(&ClientMessage::ActivityStart {
            channel_id: w.text,
            app: "builtin:nonsense".into(),
        })
        .await
        .unwrap();
    assert!(
        next_error(&mut w.carol.0)
            .await
            .contains("no such activity")
    );

    w.handle.abort();
}

#[tokio::test]
async fn an_apps_bot_is_the_backend_of_its_own_activity() {
    use base64::Engine;
    use discordia_server::protocol::{ActivityCapability, ActivityManifest, AppManifest, Intent};
    use sha2::Digest;

    let mut w = world().await;
    let page = b"<!doctype html><p>chess</p>";
    let sha = hex::encode(sha2::Sha256::digest(page));
    let app = BotIdentity::generate();
    let manifest = AppManifest {
        name: "Chess".into(),
        description: String::new(),
        icon: "♟".into(),
        version: 1,
        permissions: Vec::new(),
        intents: vec![Intent::GuildMessages],
        commands: Vec::new(),
        activity: Some(ActivityManifest {
            sha256: sha.clone(),
            capabilities: vec![ActivityCapability::Instance],
            max_players: 2,
            url: None,
        }),
    };
    let gid = w.guild_id;
    w.alice
        .0
        .send(&ClientMessage::InstallBot {
            guild_id: gid,
            bot_pubkey: app.pubkey().to_string(),
            name: String::new(),
            permissions: Vec::new(),
            intents: vec![Intent::GuildMessages],
            manifest: Some(app.sign_manifest(&manifest)),
        })
        .await
        .unwrap();
    w.alice
        .0
        .send(&ClientMessage::UploadActivityBundle {
            guild_id: gid,
            bot_pubkey: app.pubkey().to_string(),
            data_url: format!(
                "data:{};base64,{}",
                discordia_server::protocol::BUNDLE_MIME,
                base64::engine::general_purpose::STANDARD.encode(page)
            ),
        })
        .await
        .unwrap();
    let mut bot = Bot::connect(&w.url, &app, "Chess").await.unwrap();
    while !matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {}

    w.alice
        .0
        .send(&ClientMessage::ActivityStart {
            channel_id: w.text,
            app: app.pubkey().to_string(),
        })
        .await
        .unwrap();
    let started = loop {
        match next_timeout(&mut w.alice.0).await {
            ServerMessage::ActivityStarted { instance } => break instance,
            ServerMessage::Error { message } => panic!("{message}"),
            _ => {}
        }
    };
    let heard = instances(&mut bot).await;
    assert_eq!(
        heard.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![started.id]
    );

    w.alice
        .0
        .send(&ClientMessage::ActivitySend {
            instance_id: started.id,
            to: None,
            data: "{\"move\":\"e4\"}".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        event(&mut bot).await,
        (w.alice.1.clone(), "{\"move\":\"e4\"}".into())
    );
    bot.activity_send(started.id, None, "{\"board\":\"after e4\"}")
        .await
        .unwrap();
    assert_eq!(event(&mut w.alice.0).await.1, "{\"board\":\"after e4\"}");

    w.alice
        .0
        .send(&ClientMessage::UninstallBot {
            guild_id: gid,
            bot_pubkey: app.pubkey().to_string(),
        })
        .await
        .unwrap();
    loop {
        if instances(&mut w.alice.0).await.is_empty() {
            break;
        }
    }
    assert!(
        bot.activity_send(started.id, None, "{}").await.is_ok(),
        "the frame is sent; the host refuses it"
    );
    loop {
        if let ServerMessage::Error { message } = next_timeout(&mut bot).await {
            assert!(
                message.contains("ended") || message.contains("isn't installed"),
                "{message}"
            );
            break;
        }
    }

    w.handle.abort();
}

async fn ready_members(url: &str, id: &BotIdentity) -> Vec<String> {
    let mut b = Bot::connect_as_user(url, id, "Again").await.unwrap();
    loop {
        if let ServerMessage::Ready { members, .. } = next_timeout(&mut b).await {
            return members.into_iter().map(|m| m.user.pubkey).collect();
        }
    }
}

async fn seat_change(b: &mut Bot, app: &str) -> bool {
    loop {
        match next_timeout(b).await {
            ServerMessage::MemberJoin(m) if m.user.pubkey == app => {
                assert!(m.bot);
                return true;
            }
            ServerMessage::MemberRemove { user_pubkey, .. } if user_pubkey == app => return false,
            ServerMessage::Error { message } => panic!("{message}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn an_activity_only_app_plays_without_a_seat_among_the_members() {
    use base64::Engine;
    use discordia_server::protocol::{ActivityCapability, ActivityManifest, AppManifest, Intent};
    use sha2::Digest;

    let (url, handle) = spawn_gateway().await;
    let alice_id = BotIdentity::generate();
    let mut alice = Bot::connect_as_user(&url, &alice_id, "Alice")
        .await
        .unwrap();
    while !matches!(next_timeout(&mut alice).await, ServerMessage::Ready { .. }) {}
    alice
        .send(&ClientMessage::CreateGuild {
            name: "Games".into(),
            template: None,
        })
        .await
        .unwrap();
    let (gid, text) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut alice).await
        {
            let text = channels
                .iter()
                .find(|c| c.kind == ChannelKind::Text)
                .unwrap()
                .id;
            break (guild.id, text);
        }
    };
    let page = b"<!doctype html><p>wizards</p>";
    let app = BotIdentity::generate();
    let key = app.pubkey().to_string();
    let mut manifest = AppManifest {
        name: "Wizards".into(),
        description: String::new(),
        icon: "🧙".into(),
        version: 1,
        permissions: Vec::new(),
        intents: Vec::new(),
        commands: Vec::new(),
        activity: Some(ActivityManifest {
            sha256: hex::encode(sha2::Sha256::digest(page)),
            capabilities: vec![ActivityCapability::Instance],
            max_players: 2,
            url: None,
        }),
    };
    alice
        .send(&ClientMessage::InstallBot {
            guild_id: gid,
            bot_pubkey: key.clone(),
            name: String::new(),
            permissions: Vec::new(),
            intents: Vec::new(),
            manifest: Some(app.sign_manifest(&manifest)),
        })
        .await
        .unwrap();
    alice
        .send(&ClientMessage::UploadActivityBundle {
            guild_id: gid,
            bot_pubkey: key.clone(),
            data_url: format!(
                "data:{};base64,{}",
                discordia_server::protocol::BUNDLE_MIME,
                base64::engine::general_purpose::STANDARD.encode(page)
            ),
        })
        .await
        .unwrap();
    let mut bot = Bot::connect(&url, &app, "Wizards").await.unwrap();
    while !matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {}
    assert!(!ready_members(&url, &alice_id).await.contains(&key));

    alice
        .send(&ClientMessage::ActivityStart {
            channel_id: text,
            app: key.clone(),
        })
        .await
        .unwrap();
    let started = loop {
        match next_timeout(&mut alice).await {
            ServerMessage::ActivityStarted { instance } => break instance,
            ServerMessage::MemberJoin(m) if m.user.pubkey == key => {
                panic!("an activity-only app took a seat")
            }
            ServerMessage::Error { message } => panic!("{message}"),
            _ => {}
        }
    };
    assert_eq!(instances(&mut bot).await[0].id, started.id);
    bot.activity_send(started.id, None, "{\"turn\":1}")
        .await
        .unwrap();
    assert_eq!(
        event(&mut alice).await,
        (key.clone(), "{\"turn\":1}".into())
    );

    // Reading chat makes it a bot like any other, and dropping that unseats it again.
    manifest.version = 2;
    manifest.intents = vec![Intent::GuildMessages];
    bot.register_manifest(&app, &manifest).await.unwrap();
    alice
        .send(&ClientMessage::ApproveAppUpdate {
            guild_id: gid,
            bot_pubkey: key.clone(),
        })
        .await
        .unwrap();
    assert!(seat_change(&mut alice, &key).await);
    assert!(ready_members(&url, &alice_id).await.contains(&key));
    manifest.version = 3;
    manifest.intents.clear();
    bot.register_manifest(&app, &manifest).await.unwrap();
    assert!(!seat_change(&mut alice, &key).await);
    assert!(!ready_members(&url, &alice_id).await.contains(&key));

    handle.abort();
}

#[tokio::test]
async fn a_kicked_member_leaves_the_game_and_cannot_rejoin() {
    let mut w = world().await;
    w.alice
        .0
        .send(&ClientMessage::ActivityStart {
            channel_id: w.text,
            app: "builtin:rps".into(),
        })
        .await
        .unwrap();
    let started = loop {
        if let ServerMessage::ActivityStarted { instance } = next_timeout(&mut w.alice.0).await {
            break instance;
        }
    };
    w.carol
        .0
        .send(&ClientMessage::ActivityJoin {
            instance_id: started.id,
            role: ActivityRole::Spectator,
        })
        .await
        .unwrap();
    loop {
        let list = instances(&mut w.alice.0).await;
        if list.first().is_some_and(|i| i.participants.len() == 2) {
            break;
        }
    }
    w.alice
        .0
        .send(&ClientMessage::KickMember {
            guild_id: w.guild_id,
            user_pubkey: w.carol.1.clone(),
        })
        .await
        .unwrap();
    loop {
        let list = instances(&mut w.alice.0).await;
        if list.first().is_some_and(|i| i.participants.len() == 1) {
            break;
        }
    }
    w.carol
        .0
        .send(&ClientMessage::ActivityJoin {
            instance_id: started.id,
            role: ActivityRole::Spectator,
        })
        .await
        .unwrap();
    assert!(next_error(&mut w.carol.0).await.contains("can't see"));

    w.handle.abort();
}

async fn guild_instances(b: &mut Bot) -> Vec<ActivityInstance> {
    loop {
        match next_timeout(b).await {
            ServerMessage::GuildActivityInstances { instances, .. } => return instances,
            ServerMessage::Error { message } => panic!("unexpected error: {message}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_member_finds_every_game_in_the_guild_and_an_outsider_none() {
    let mut w = world().await;
    w.alice
        .0
        .send(&ClientMessage::ActivityStart {
            channel_id: w.text,
            app: "builtin:rps".into(),
        })
        .await
        .unwrap();
    let started = loop {
        if let ServerMessage::ActivityStarted { instance } = next_timeout(&mut w.alice.0).await {
            break instance;
        }
    };
    w.bob
        .0
        .send(&ClientMessage::FetchGuildActivityInstances {
            guild_id: w.guild_id,
        })
        .await
        .unwrap();
    assert_eq!(guild_instances(&mut w.bob.0).await, vec![started]);

    let (outsider, _) = &mut user(&w.url, "Outsider").await;
    outsider
        .send(&ClientMessage::FetchGuildActivityInstances {
            guild_id: w.guild_id,
        })
        .await
        .unwrap();
    assert!(guild_instances(outsider).await.is_empty());

    w.handle.abort();
}
