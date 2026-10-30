use std::net::SocketAddr;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{
    ChannelAccess, ChannelKind, ClientMessage, Id, Permission, ServerMessage, VoiceState,
};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

async fn spawn_gateway() -> (String, discordia_server::ServerHandle) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "discordia-hidden-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let cfg = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    };
    let preferred: SocketAddr = "127.0.0.1:19300".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 100, cfg)
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

/// Every voice state sent to `session` so far: the reply to this probe is
/// queued behind anything delivered before it.
async fn voice_states_so_far(session: &mut Bot) -> Vec<VoiceState> {
    let probe = Id::new_v4();
    session.fetch_messages(probe, 1, None).await.unwrap();
    let mut seen = Vec::new();
    loop {
        match next_timeout(session).await {
            ServerMessage::VoiceStateUpdate(vs) => seen.push(vs),
            ServerMessage::MessageHistory { channel_id, .. } if channel_id == probe => return seen,
            ServerMessage::Error { .. } => return seen,
            _ => {}
        }
    }
}

async fn voice_channel(owner: &mut Bot, guild_id: Id, name: &str) -> Id {
    owner
        .send(&ClientMessage::CreateChannel {
            divider: Default::default(),
            guild_id,
            name: name.into(),
            kind: ChannelKind::Voice,
            topic: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ChannelCreate(ch) = next_timeout(owner).await
            && ch.name == name
        {
            return ch.id;
        }
    }
}

async fn hide(owner: &mut Bot, voice: Id, role: Id) {
    owner
        .send(&ClientMessage::SetChannelAccess {
            channel_id: voice,
            access: Some(ChannelAccess {
                roles: vec![role],
                users: vec![],
            }),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ChannelUpdate(ch) = next_timeout(owner).await
            && ch.id == voice
        {
            return;
        }
    }
}

async fn join_voice(session: &mut Bot, channel_id: Id) {
    session
        .send(&ClientMessage::JoinVoice {
            channel_id,
            preferences: None,
        })
        .await
        .unwrap();
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
    assert_eq!(
        next_state_of(&mut owner, &staffer_key).await.channel_id,
        Some(voice),
        "the owner manages channels, so sees it"
    );
    assert!(
        voice_states_so_far(&mut guest).await.is_empty(),
        "who is inside is hidden along with the channel"
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

#[tokio::test]
async fn a_hidden_divider_goes_only_to_those_allowed() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, _) = connect_user(&url, &BotIdentity::generate(), "Owner").await;
    let (guild_id, _, staff) = setup(&mut owner).await;
    owner
        .send(&ClientMessage::CreateChannel {
            divider: Default::default(),
            guild_id,
            name: "Staff only".into(),
            kind: ChannelKind::Divider,
            topic: None,
        })
        .await
        .unwrap();
    let divider = loop {
        if let ServerMessage::ChannelCreate(ch) = next_timeout(&mut owner).await
            && ch.kind == ChannelKind::Divider
        {
            break ch.id;
        }
    };

    let staffer_id = BotIdentity::generate();
    let (mut staffer, _) = connect_user(&url, &staffer_id, "Staffer").await;
    join_guild(&mut staffer, guild_id).await;
    let guest_id = BotIdentity::generate();
    let (mut guest, _) = connect_user(&url, &guest_id, "Guest").await;
    join_guild(&mut guest, guild_id).await;
    set_role(&mut owner, guild_id, staff, staffer_id.pubkey(), true).await;

    owner
        .send(&ClientMessage::SetChannelAccess {
            channel_id: divider,
            access: Some(ChannelAccess {
                roles: vec![staff],
                users: vec![],
            }),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ChannelDelete { channel_id, .. } = next_timeout(&mut guest).await
            && channel_id == divider
        {
            break;
        }
    }
    loop {
        match next_timeout(&mut staffer).await {
            ServerMessage::ChannelUpdate(ch) if ch.id == divider => {
                assert!(ch.access.is_some());
                break;
            }
            ServerMessage::ChannelDelete { channel_id, .. } if channel_id == divider => {
                panic!("the role that is allowed lost the divider")
            }
            _ => continue,
        }
    }
    drop(staffer);
    drop(guest);

    let (_staffer, visible) = connect_user(&url, &staffer_id, "Staffer").await;
    assert!(visible.contains(&divider));
    let (_guest, visible) = connect_user(&url, &guest_id, "Guest").await;
    assert!(!visible.contains(&divider), "never sent on connect either");

    handle.abort();
}

#[tokio::test]
async fn a_non_viewer_hears_nothing_from_inside_a_hidden_room() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, _) = connect_user(&url, &BotIdentity::generate(), "Owner").await;
    let (guild_id, voice, staff) = setup(&mut owner).await;
    let lounge = voice_channel(&mut owner, guild_id, "Lounge").await;
    hide(&mut owner, voice, staff).await;
    let staffer_id = BotIdentity::generate();
    let staffer_key = staffer_id.pubkey().to_string();
    let (mut staffer, _) = connect_user(&url, &staffer_id, "Staffer").await;
    join_guild(&mut staffer, guild_id).await;
    set_role(&mut owner, guild_id, staff, &staffer_key, true).await;
    let (mut guest, _) = connect_user(&url, &BotIdentity::generate(), "Guest").await;
    join_guild(&mut guest, guild_id).await;

    join_voice(&mut staffer, voice).await;
    for msg in [
        ClientMessage::SetVoiceMute {
            muted: true,
            deafened: true,
        },
        ClientMessage::SetSpeaking { speaking: true },
        ClientMessage::SetSpeaking { speaking: false },
        ClientMessage::LeaveVoice,
    ] {
        staffer.send(&msg).await.unwrap();
    }
    while next_state_of(&mut owner, &staffer_key)
        .await
        .channel_id
        .is_some()
    {}
    assert!(
        voice_states_so_far(&mut guest).await.is_empty(),
        "a whole call in a hidden room, from entry to leaving, sends a non-viewer nothing"
    );

    join_voice(&mut staffer, lounge).await;
    while next_state_of(&mut owner, &staffer_key).await.channel_id != Some(lounge) {}
    staffer
        .send(&ClientMessage::SetVoiceMute {
            muted: true,
            deafened: true,
        })
        .await
        .unwrap();
    while !next_state_of(&mut owner, &staffer_key).await.muted {}
    join_voice(&mut staffer, voice).await;
    staffer
        .send(&ClientMessage::SetSpeaking { speaking: true })
        .await
        .unwrap();
    while !next_state_of(&mut owner, &staffer_key).await.speaking {}
    let seen: Vec<_> = voice_states_so_far(&mut guest)
        .await
        .into_iter()
        .filter(|v| v.user_pubkey == staffer_key)
        .map(|v| (v.channel_id, v.muted, v.deafened, v.speaking))
        .collect();
    assert_eq!(
        seen,
        [
            (Some(lounge), false, false, false),
            (Some(lounge), true, true, false),
            (None, true, true, false)
        ],
        "leaving a room they could see is one frame, saying only what they saw there"
    );
    handle.abort();
}

#[tokio::test]
async fn a_moderator_who_cannot_see_the_room_cannot_disconnect_or_find_who_is_in_it() {
    let (url, handle) = spawn_gateway().await;
    let (mut owner, _) = connect_user(&url, &BotIdentity::generate(), "Owner").await;
    let (guild_id, voice, staff) = setup(&mut owner).await;
    hide(&mut owner, voice, staff).await;
    owner
        .send(&ClientMessage::CreateRole {
            guild_id,
            name: "Bouncer".into(),
            color: None,
            permissions: vec![Permission::DisconnectMembers],
        })
        .await
        .unwrap();
    let bouncer_role = loop {
        if let ServerMessage::GuildRoles { roles, .. } = next_timeout(&mut owner).await
            && let Some(r) = roles.iter().find(|r| r.name == "Bouncer")
        {
            break r.id;
        }
    };
    let staffer_id = BotIdentity::generate();
    let staffer_key = staffer_id.pubkey().to_string();
    let (mut staffer, _) = connect_user(&url, &staffer_id, "Staffer").await;
    join_guild(&mut staffer, guild_id).await;
    set_role(&mut owner, guild_id, staff, &staffer_key, true).await;
    let idle_id = BotIdentity::generate();
    let (mut idle, _) = connect_user(&url, &idle_id, "Idle").await;
    join_guild(&mut idle, guild_id).await;
    let bouncer_id = BotIdentity::generate();
    let (mut bouncer, _) = connect_user(&url, &bouncer_id, "Bouncer").await;
    join_guild(&mut bouncer, guild_id).await;
    set_role(
        &mut owner,
        guild_id,
        bouncer_role,
        bouncer_id.pubkey(),
        true,
    )
    .await;

    join_voice(&mut staffer, voice).await;
    while next_state_of(&mut owner, &staffer_key).await.channel_id != Some(voice) {}
    let mut replies = Vec::new();
    for target in [staffer_key.clone(), idle_id.pubkey().to_string()] {
        bouncer
            .send(&ClientMessage::DisconnectVoice {
                guild_id,
                user_pubkey: target,
            })
            .await
            .unwrap();
        replies.push(next_error(&mut bouncer).await);
    }
    assert_eq!(
        replies[0], replies[1],
        "the hidden occupant answers like someone not in voice"
    );
    assert!(
        voice_states_so_far(&mut staffer)
            .await
            .iter()
            .all(|v| v.channel_id == Some(voice)),
        "they are still in the call"
    );
    drop(idle);
    handle.abort();
}
