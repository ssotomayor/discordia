use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{
    ActivityInstance, ActivityRole, ChannelKind, ClientMessage, Id, ServerMessage,
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
        "dioxusfun-instances-{}-{}",
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
    let preferred: SocketAddr = "127.0.0.1:19500".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, config)
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
    handle: dioxusfun_server::ServerHandle,
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
    use dioxusfun_server::protocol::{ActivityCapability, ActivityManifest, AppManifest, Intent};
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
                dioxusfun_server::protocol::BUNDLE_MIME,
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

    w.handle.abort();
}
