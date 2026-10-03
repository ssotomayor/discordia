use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{
    ChannelKind, ClientMessage, Id, Permission, ServerMessage, VoiceState,
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
        "dioxusfun-voice-move-{}-{}",
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
    let preferred: SocketAddr = "127.0.0.1:19290".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn connect_user(url: &str, id: &BotIdentity, name: &str) -> Bot {
    let mut session = Bot::connect_as_user(url, id, name).await.unwrap();
    loop {
        if let ServerMessage::Ready { .. } = next_timeout(&mut session).await {
            return session;
        }
    }
}

async fn guild_with_voice(owner: &mut Bot, name: &str) -> (Id, Id) {
    owner
        .send(&ClientMessage::CreateGuild {
            name: name.into(),
            template: None,
        })
        .await
        .unwrap();
    let guild_id = loop {
        if let ServerMessage::GuildJoined { guild, .. } = next_timeout(owner).await {
            break guild.id;
        }
    };
    owner
        .send(&ClientMessage::CreateChannel {
            guild_id,
            name: "Voice".into(),
            kind: ChannelKind::Voice,
            topic: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ChannelCreate(ch) = next_timeout(owner).await
            && ch.kind == ChannelKind::Voice
        {
            return (guild_id, ch.id);
        }
    }
}

async fn join_guild(member: &mut Bot, guild_id: Id) {
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { guild, .. } = next_timeout(member).await
            && guild.id == guild_id
        {
            return;
        }
    }
}

/// The next voice state about `who`, skipping everything else.
async fn next_state_of(session: &mut Bot, who: &str) -> VoiceState {
    loop {
        if let ServerMessage::VoiceStateUpdate(vs) = next_timeout(session).await
            && vs.user_pubkey == who
        {
            return vs;
        }
    }
}

#[tokio::test]
async fn moving_to_another_guilds_call_is_a_leave_the_old_guild_hears() {
    let (url, handle) = spawn_gateway().await;
    let mover_id = BotIdentity::generate();
    let mover_key = mover_id.pubkey().to_string();
    let mut mover = connect_user(&url, &mover_id, "Mover").await;
    let (first_guild, first_voice) = guild_with_voice(&mut mover, "First").await;
    let (_, second_voice) = guild_with_voice(&mut mover, "Second").await;

    let mut stayer = connect_user(&url, &BotIdentity::generate(), "Stayer").await;
    join_guild(&mut stayer, first_guild).await;

    mover
        .send(&ClientMessage::JoinVoice {
            channel_id: first_voice,
            preferences: None,
        })
        .await
        .unwrap();
    assert_eq!(
        next_state_of(&mut stayer, &mover_key).await.channel_id,
        Some(first_voice)
    );
    mover
        .send(&ClientMessage::SetScreenShare {
            channel_id: first_voice,
            sharing: true,
        })
        .await
        .unwrap();
    assert!(next_state_of(&mut stayer, &mover_key).await.screen_sharing);

    mover
        .send(&ClientMessage::JoinVoice {
            channel_id: second_voice,
            preferences: None,
        })
        .await
        .unwrap();

    let left = next_state_of(&mut stayer, &mover_key).await;
    assert_eq!(left.guild_id, first_guild);
    assert_eq!(
        left.channel_id, None,
        "the old guild is told the mover left"
    );
    assert!(!left.screen_sharing && !left.camera_on);
    loop {
        if let ServerMessage::ScreenShareState {
            channel_id,
            sharers,
        } = next_timeout(&mut stayer).await
            && channel_id == first_voice
        {
            assert!(
                sharers.is_empty(),
                "the share ended with the move: {sharers:?}"
            );
            break;
        }
    }

    // The mover is in both guilds, so the order is what its own client sees:
    // the leave first, then the join, or the join would be erased.
    let mut seen = Vec::new();
    while seen.len() < 2 {
        if let ServerMessage::VoiceStateUpdate(vs) = next_timeout(&mut mover).await
            && vs.user_pubkey == mover_key
            && (vs.channel_id.is_none() || vs.channel_id == Some(second_voice))
        {
            seen.push(vs.channel_id);
        }
    }
    assert_eq!(seen, [None, Some(second_voice)]);

    handle.abort();
}

#[tokio::test]
async fn moving_within_a_guild_also_drops_the_share() {
    let (url, handle) = spawn_gateway().await;
    let mover_id = BotIdentity::generate();
    let mover_key = mover_id.pubkey().to_string();
    let mut mover = connect_user(&url, &mover_id, "Mover").await;
    let (guild_id, first_voice) = guild_with_voice(&mut mover, "Home").await;
    mover
        .send(&ClientMessage::CreateChannel {
            guild_id,
            name: "Other".into(),
            kind: ChannelKind::Voice,
            topic: None,
        })
        .await
        .unwrap();
    let second_voice = loop {
        if let ServerMessage::ChannelCreate(ch) = next_timeout(&mut mover).await
            && ch.kind == ChannelKind::Voice
            && ch.id != first_voice
        {
            break ch.id;
        }
    };
    let mut watcher = connect_user(&url, &BotIdentity::generate(), "Watcher").await;
    join_guild(&mut watcher, guild_id).await;

    mover
        .send(&ClientMessage::JoinVoice {
            channel_id: first_voice,
            preferences: None,
        })
        .await
        .unwrap();
    next_state_of(&mut watcher, &mover_key).await;
    mover
        .send(&ClientMessage::SetScreenShare {
            channel_id: first_voice,
            sharing: true,
        })
        .await
        .unwrap();
    assert!(next_state_of(&mut watcher, &mover_key).await.screen_sharing);

    mover
        .send(&ClientMessage::JoinVoice {
            channel_id: second_voice,
            preferences: None,
        })
        .await
        .unwrap();
    assert_eq!(
        next_state_of(&mut watcher, &mover_key).await.channel_id,
        None
    );
    let joined = next_state_of(&mut watcher, &mover_key).await;
    assert_eq!(joined.channel_id, Some(second_voice));
    assert!(!joined.screen_sharing, "sharing must be turned on again");

    handle.abort();
}

async fn next_error(session: &mut Bot) -> String {
    loop {
        if let ServerMessage::Error { message } = next_timeout(session).await {
            return message;
        }
    }
}

#[tokio::test]
async fn a_manager_disconnects_someone_and_everyone_sees_them_leave() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let owner_key = owner_id.pubkey().to_string();
    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, voice) = guild_with_voice(&mut owner, "Mods").await;

    let target_id = BotIdentity::generate();
    let target_key = target_id.pubkey().to_string();
    let mut target = connect_user(&url, &target_id, "Target").await;
    join_guild(&mut target, guild_id).await;
    let bystander_id = BotIdentity::generate();
    let mut bystander = connect_user(&url, &bystander_id, "Bystander").await;
    join_guild(&mut bystander, guild_id).await;

    target
        .send(&ClientMessage::JoinVoice {
            channel_id: voice,
            preferences: None,
        })
        .await
        .unwrap();
    next_state_of(&mut owner, &target_key).await;
    target
        .send(&ClientMessage::SetScreenShare {
            channel_id: voice,
            sharing: true,
        })
        .await
        .unwrap();
    assert!(next_state_of(&mut owner, &target_key).await.screen_sharing);

    bystander
        .send(&ClientMessage::DisconnectVoice {
            guild_id,
            user_pubkey: target_key.clone(),
        })
        .await
        .unwrap();
    assert!(
        next_error(&mut bystander)
            .await
            .contains("disconnect_members"),
        "a plain member cannot disconnect anyone"
    );

    owner
        .send(&ClientMessage::DisconnectVoice {
            guild_id,
            user_pubkey: target_key.clone(),
        })
        .await
        .unwrap();
    for session in [&mut target, &mut bystander, &mut owner] {
        let gone = loop {
            let vs = next_state_of(session, &target_key).await;
            if vs.channel_id.is_none() {
                break vs;
            }
        };
        assert!(!gone.screen_sharing && !gone.camera_on);
    }
    loop {
        if let ServerMessage::ScreenShareState {
            channel_id,
            sharers,
        } = next_timeout(&mut bystander).await
            && channel_id == voice
        {
            assert!(sharers.is_empty());
            break;
        }
    }

    // Holding the permission reaches members, never the owner, as for a kick.
    owner
        .send(&ClientMessage::CreateRole {
            guild_id,
            name: "Bouncer".into(),
            color: None,
            permissions: vec![Permission::DisconnectMembers],
        })
        .await
        .unwrap();
    let role_id = loop {
        if let ServerMessage::GuildRoles { roles, .. } = next_timeout(&mut owner).await
            && let Some(r) = roles.iter().find(|r| r.name == "Bouncer")
        {
            break r.id;
        }
    };
    owner
        .send(&ClientMessage::AssignRole {
            guild_id,
            role_id,
            user_pubkey: target_key.clone(),
        })
        .await
        .unwrap();
    owner
        .send(&ClientMessage::JoinVoice {
            channel_id: voice,
            preferences: None,
        })
        .await
        .unwrap();
    next_state_of(&mut target, &owner_key).await;
    target
        .send(&ClientMessage::DisconnectVoice {
            guild_id,
            user_pubkey: owner_key.clone(),
        })
        .await
        .unwrap();
    assert!(next_error(&mut target).await.contains("owner"));

    bystander
        .send(&ClientMessage::JoinVoice {
            channel_id: voice,
            preferences: None,
        })
        .await
        .unwrap();
    let bystander_key = bystander_id.pubkey().to_string();
    next_state_of(&mut target, &bystander_key).await;
    target
        .send(&ClientMessage::DisconnectVoice {
            guild_id,
            user_pubkey: bystander_key.clone(),
        })
        .await
        .unwrap();
    // Their own join is still queued ahead of the disconnect.
    let mut seen = Vec::new();
    while seen.last() != Some(&None) {
        seen.push(
            next_state_of(&mut bystander, &bystander_key)
                .await
                .channel_id,
        );
    }
    assert_eq!(
        seen,
        [Some(voice), None],
        "the role is enough to disconnect a plain member"
    );

    handle.abort();
}

#[tokio::test]
async fn a_category_is_a_separator_nobody_can_join_or_post_to() {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-category-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let cfg = dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir.clone(),
    };
    let handle = dioxusfun_server::spawn("127.0.0.1:19291".parse().unwrap(), 100, cfg)
        .await
        .expect("spawn server");
    let url = format!("ws://{}", handle.addr);
    let mut owner = connect_user(&url, &BotIdentity::generate(), "Owner").await;
    let (guild_id, _voice) = guild_with_voice(&mut owner, "Sorted").await;

    owner
        .send(&ClientMessage::CreateChannel {
            guild_id,
            name: "Gaming Rooms".into(),
            kind: ChannelKind::Category,
            topic: None,
        })
        .await
        .unwrap();
    let category = loop {
        if let ServerMessage::ChannelCreate(ch) = next_timeout(&mut owner).await
            && ch.kind == ChannelKind::Category
        {
            break ch;
        }
    };
    assert_eq!(
        category.name, "Gaming Rooms",
        "a title keeps its spaces and capitals"
    );

    owner
        .send(&ClientMessage::JoinVoice {
            channel_id: category.id,
            preferences: None,
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("not a voice channel"));
    owner
        .send(&ClientMessage::SendMessage {
            channel_id: category.id,
            content: "hello".into(),
            image: None,
            reply_to: None,
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("can't post"));

    tokio::time::sleep(Duration::from_millis(300)).await;
    let store = dioxusfun_server::store::Store::open(&dir.join("discordia.db"))
        .await
        .expect("open the store");
    let loaded = store.load_all().await.expect("load");
    let stored = loaded
        .channels
        .iter()
        .find(|c| c.id == category.id)
        .expect("the category was stored");
    assert_eq!(
        stored.kind,
        ChannelKind::Category,
        "it stays a category after a restart"
    );

    handle.abort();
}

#[tokio::test]
async fn a_media_key_only_travels_between_two_people_in_that_call() {
    let (url, handle) = spawn_gateway().await;
    let sender_id = BotIdentity::generate();
    let mut sender = connect_user(&url, &sender_id, "Sender").await;
    let (guild_id, voice) = guild_with_voice(&mut sender, "Keys").await;
    let peer_id = BotIdentity::generate();
    let peer_key = peer_id.pubkey().to_string();
    let mut peer = connect_user(&url, &peer_id, "Peer").await;
    join_guild(&mut peer, guild_id).await;

    sender
        .send(&ClientMessage::JoinVoice {
            channel_id: voice,
            preferences: None,
        })
        .await
        .unwrap();
    let share = |to: &str| ClientMessage::ShareMediaKey {
        channel_id: voice,
        to: to.to_string(),
        epoch: 1,
        blob: "sealed".into(),
    };
    sender.send(&share(&peer_key)).await.unwrap();
    let outside = async {
        loop {
            if let Some(ServerMessage::MediaKey { .. }) = peer.next_event().await {
                return;
            }
        }
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(500), outside)
            .await
            .is_err(),
        "someone outside the call is never handed its key"
    );

    peer.send(&ClientMessage::JoinVoice {
        channel_id: voice,
        preferences: None,
    })
    .await
    .unwrap();
    next_state_of(&mut sender, &peer_key).await;
    sender.send(&share(&peer_key)).await.unwrap();
    loop {
        if let ServerMessage::MediaKey { from, epoch, .. } = next_timeout(&mut peer).await {
            assert_eq!(from, sender_id.pubkey());
            assert_eq!(epoch, 1);
            break;
        }
    }

    handle.abort();
}
