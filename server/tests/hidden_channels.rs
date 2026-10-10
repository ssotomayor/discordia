use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{
    ChannelAccess, ChannelKind, ClientMessage, Id, Permission, ServerMessage, VoiceState,
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
        "dioxusfun-hidden-{}-{}",
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
    let preferred: SocketAddr = "127.0.0.1:19300".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn connect_user(url: &str, id: &BotIdentity, name: &str) -> (Bot, Vec<Id>) {
    let mut session = Bot::connect_as_user(url, id, name).await.unwrap();
    loop {
        if let ServerMessage::Ready { channels, .. } = next_timeout(&mut session).await {
            return (session, channels.iter().map(|c| c.id).collect());
        }
    }
}

async fn setup(owner: &mut Bot) -> (Id, Id, Id) {
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Secrets".into(),
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
            divider: Default::default(),
            guild_id,
            name: "Staff room".into(),
            kind: ChannelKind::Voice,
            topic: None,
        })
        .await
        .unwrap();
    let voice = loop {
        if let ServerMessage::ChannelCreate(ch) = next_timeout(owner).await
            && ch.kind == ChannelKind::Voice
        {
            break ch.id;
        }
    };
    owner
        .send(&ClientMessage::CreateRole {
            guild_id,
            name: "Staff".into(),
            color: None,
            permissions: vec![Permission::SendMessages],
        })
        .await
        .unwrap();
    let role = loop {
        if let ServerMessage::GuildRoles { roles, .. } = next_timeout(owner).await
            && let Some(r) = roles.iter().find(|r| r.name == "Staff")
        {
            break r.id;
        }
    };
    (guild_id, voice, role)
}

async fn join_guild(member: &mut Bot, guild_id: Id) -> Vec<Id> {
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { channels, .. } = next_timeout(member).await {
            return channels.iter().map(|c| c.id).collect();
        }
    }
}

async fn next_state_of(session: &mut Bot, who: &str) -> VoiceState {
    loop {
        if let ServerMessage::VoiceStateUpdate(vs) = next_timeout(session).await
            && vs.user_pubkey == who
        {
            return vs;
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

async fn set_role(owner: &mut Bot, guild_id: Id, role_id: Id, who: &str, on: bool) {
    let msg = if on {
        ClientMessage::AssignRole {
            guild_id,
            role_id,
            user_pubkey: who.to_string(),
        }
    } else {
        ClientMessage::UnassignRole {
            guild_id,
            role_id,
            user_pubkey: who.to_string(),
        }
    };
    owner.send(&msg).await.unwrap();
}

#[tokio::test]
async fn a_hidden_channel_is_only_sent_to_those_allowed_and_follows_their_roles() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, _) = connect_user(&url, &BotIdentity::generate(), "Owner").await;
    let (guild_id, voice, staff) = setup(&mut owner).await;

    let staffer_id = BotIdentity::generate();
    let staffer_key = staffer_id.pubkey().to_string();
    let (mut staffer, _) = connect_user(&url, &staffer_id, "Staffer").await;
    join_guild(&mut staffer, guild_id).await;
    let guest_id = BotIdentity::generate();
    let guest_key = guest_id.pubkey().to_string();
    let (mut guest, _) = connect_user(&url, &guest_id, "Guest").await;
    join_guild(&mut guest, guild_id).await;
    set_role(&mut owner, guild_id, staff, &staffer_key, true).await;

    owner
        .send(&ClientMessage::SetChannelAccess {
            channel_id: voice,
            access: Some(ChannelAccess {
                roles: vec![staff],
                users: vec![],
            }),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ChannelDelete { channel_id, .. } = next_timeout(&mut guest).await
            && channel_id == voice
        {
            break;
        }
    }

    guest
        .send(&ClientMessage::JoinVoice {
            channel_id: voice,
            preferences: None,
        })
        .await
        .unwrap();
    assert!(
        next_error(&mut guest).await.contains("not a voice channel"),
        "a hidden channel answers as if it did not exist"
    );

    staffer
        .send(&ClientMessage::JoinVoice {
            channel_id: voice,
            preferences: None,
        })
        .await
        .unwrap();
    let seen_by_guest = next_state_of(&mut guest, &staffer_key).await;
    assert_eq!(
        seen_by_guest.channel_id, None,
        "who is inside is hidden along with the channel"
    );
    assert_eq!(
        next_state_of(&mut owner, &staffer_key).await.channel_id,
        Some(voice),
        "the owner manages channels, so sees it"
    );

    set_role(&mut owner, guild_id, staff, &guest_key, true).await;
    loop {
        if let ServerMessage::ChannelCreate(ch) = next_timeout(&mut guest).await
            && ch.id == voice
        {
            break;
        }
    }
    assert_eq!(
        next_state_of(&mut guest, &staffer_key).await.channel_id,
        Some(voice),
        "gaining the role shows who is already in there"
    );

    set_role(&mut owner, guild_id, staff, &staffer_key, false).await;
    // Their own join is still queued ahead of the disconnect.
    let mut seen = Vec::new();
    while seen.last() != Some(&None) {
        seen.push(next_state_of(&mut staffer, &staffer_key).await.channel_id);
    }
    assert_eq!(
        seen,
        [Some(voice), None],
        "losing access while inside is losing the call"
    );
    loop {
        if let ServerMessage::ChannelDelete { channel_id, .. } = next_timeout(&mut staffer).await
            && channel_id == voice
        {
            break;
        }
    }

    handle.abort();
}

#[tokio::test]
async fn a_person_can_be_let_in_by_key_and_it_holds_across_a_fresh_connect() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, _) = connect_user(&url, &BotIdentity::generate(), "Owner").await;
    let (guild_id, voice, _) = setup(&mut owner).await;
    let friend_id = BotIdentity::generate();
    let (mut friend, _) = connect_user(&url, &friend_id, "Friend").await;
    assert!(join_guild(&mut friend, guild_id).await.contains(&voice));
    let stranger_id = BotIdentity::generate();
    let (mut stranger, _) = connect_user(&url, &stranger_id, "Stranger").await;
    join_guild(&mut stranger, guild_id).await;

    owner
        .send(&ClientMessage::SetChannelAccess {
            channel_id: voice,
            access: Some(ChannelAccess {
                roles: vec![],
                users: vec![friend_id.pubkey().to_string()],
            }),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ChannelUpdate(ch) = next_timeout(&mut friend).await
            && ch.id == voice
        {
            assert!(
                ch.access.is_some(),
                "someone still allowed gets the new list"
            );
            break;
        }
    }
    drop(friend);
    drop(stranger);

    let (_friend, visible) = connect_user(&url, &friend_id, "Friend").await;
    assert!(visible.contains(&voice), "listed by key, sent on connect");
    let (_stranger, visible) = connect_user(&url, &stranger_id, "Stranger").await;
    assert!(!visible.contains(&voice), "never sent to anyone else");

    owner
        .send(&ClientMessage::CreateChannel {
            divider: Default::default(),
            guild_id,
            name: "general-2".into(),
            kind: ChannelKind::Text,
            topic: None,
        })
        .await
        .unwrap();
    let text = loop {
        if let ServerMessage::ChannelCreate(ch) = next_timeout(&mut owner).await
            && ch.kind == ChannelKind::Text
        {
            break ch.id;
        }
    };
    owner
        .send(&ClientMessage::SetChannelAccess {
            channel_id: text,
            access: Some(ChannelAccess::default()),
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("only voice channels"));

    handle.abort();
}
