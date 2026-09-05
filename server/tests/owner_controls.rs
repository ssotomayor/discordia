use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{
    Activity, ActivityKind, ChannelKind, ClientMessage, Id, Intent, LevelTier, Leveling,
    MemberSort, Permission, ServerMessage,
};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn test_config(operators: std::collections::HashSet<String>) -> dioxusfun_server::ServerConfig {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators,
        identities: Default::default(),
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    }
}

async fn spawn_gateway() -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:19200".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, test_config(Default::default()))
        .await
        .expect("spawn server");
    let url = format!("ws://{}", handle.addr);
    (url, handle)
}

async fn connect_user(
    url: &str,
    id: &BotIdentity,
    name: &str,
) -> (Bot, Vec<dioxusfun_server::protocol::Guild>) {
    let mut session = Bot::connect_as_user(url, id, name).await.unwrap();
    let guilds = loop {
        if let ServerMessage::Ready { guilds, .. } = next_timeout(&mut session).await {
            break guilds;
        }
    };
    (session, guilds)
}

async fn create_guild(owner: &mut Bot, name: &str) -> (Id, Id) {
    owner
        .send(&ClientMessage::CreateGuild {
            name: name.into(),
            template: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(owner).await
        {
            let text = channels
                .iter()
                .find(|c| c.kind == ChannelKind::Text)
                .expect("guild has a text channel")
                .id;
            return (guild.id, text);
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

#[tokio::test]
async fn role_crud_and_broadcast() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _text) = create_guild(&mut owner, "Roleplay").await;

    let (mut member, _) = connect_user(&url, &member_id, "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut member).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    owner
        .send(&ClientMessage::CreateRole {
            guild_id,
            name: "Mod".into(),
            color: Some("#ff0000".into()),
            permissions: vec![Permission::KickMembers],
        })
        .await
        .unwrap();
    let roles = loop {
        if let ServerMessage::GuildRoles {
            guild_id: gid,
            roles,
        } = next_timeout(&mut member).await
            && gid == guild_id
        {
            break roles;
        }
    };
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0].name, "Mod");
    assert_eq!(roles[0].permissions, vec![Permission::KickMembers]);
    let role_id = roles[0].id;

    owner
        .send(&ClientMessage::AssignRole {
            guild_id,
            role_id,
            user_pubkey: member_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let updated = loop {
        if let ServerMessage::MemberUpdate(m) = next_timeout(&mut member).await
            && m.user.pubkey == member_id.pubkey()
        {
            break m;
        }
    };
    assert!(updated.roles.contains(&role_id));

    let fresh_id = BotIdentity::generate();
    let (mut fresh, _) = connect_user(&url, &fresh_id, "Fresh").await;
    fresh
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    let joined_roles = loop {
        if let ServerMessage::GuildJoined { roles, .. } = next_timeout(&mut fresh).await {
            break roles;
        }
    };
    assert_eq!(joined_roles.len(), 1, "roles travel with GuildJoined");

    owner
        .send(&ClientMessage::DeleteRole { guild_id, role_id })
        .await
        .unwrap();
    let mut saw_empty_roles = false;
    let mut saw_strip = false;
    while !(saw_empty_roles && saw_strip) {
        match next_timeout(&mut member).await {
            ServerMessage::GuildRoles {
                guild_id: gid,
                roles,
            } if gid == guild_id => {
                assert!(roles.is_empty());
                saw_empty_roles = true;
            }
            ServerMessage::MemberUpdate(m) if m.user.pubkey == member_id.pubkey() => {
                assert!(m.roles.is_empty());
                saw_strip = true;
            }
            _ => {}
        }
    }

    handle.abort();
}

#[tokio::test]
async fn manage_guild_role_unlocks_accent() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Styled").await;

    let (mut member, _) = connect_user(&url, &member_id, "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut member).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    member
        .send(&ClientMessage::SetGuildAccent {
            guild_id,
            accent: Some("#123456".into()),
        })
        .await
        .unwrap();
    let err = next_error(&mut member).await;
    assert!(err.contains("manage_guild"), "got: {err}");

    owner
        .send(&ClientMessage::CreateRole {
            guild_id,
            name: "Admin".into(),
            color: None,
            permissions: vec![Permission::ManageGuild],
        })
        .await
        .unwrap();
    let role_id = loop {
        if let ServerMessage::GuildRoles { roles, .. } = next_timeout(&mut owner).await {
            break roles[0].id;
        }
    };
    owner
        .send(&ClientMessage::AssignRole {
            guild_id,
            role_id,
            user_pubkey: member_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::MemberUpdate(m) = next_timeout(&mut member).await
            && m.user.pubkey == member_id.pubkey()
            && m.roles.contains(&role_id)
        {
            break;
        }
    }

    member
        .send(&ClientMessage::SetGuildAccent {
            guild_id,
            accent: Some("#123456".into()),
        })
        .await
        .unwrap();
    let updated = loop {
        if let ServerMessage::GuildUpdate(g) = next_timeout(&mut owner).await
            && g.id == guild_id
        {
            break g;
        }
    };
    assert_eq!(updated.accent.as_deref(), Some("#123456"));

    handle.abort();
}

#[tokio::test]
async fn role_escalation_blocked() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let mod_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Fortress").await;

    let (mut moderator, _) = connect_user(&url, &mod_id, "Mod").await;
    moderator
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut moderator).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    owner
        .send(&ClientMessage::CreateRole {
            guild_id,
            name: "RoleWrangler".into(),
            color: None,
            permissions: vec![Permission::ManageRoles, Permission::KickMembers],
        })
        .await
        .unwrap();
    let role_id = loop {
        if let ServerMessage::GuildRoles { roles, .. } = next_timeout(&mut owner).await {
            break roles[0].id;
        }
    };
    owner
        .send(&ClientMessage::AssignRole {
            guild_id,
            role_id,
            user_pubkey: mod_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::MemberUpdate(m) = next_timeout(&mut moderator).await
            && m.user.pubkey == mod_id.pubkey()
            && m.roles.contains(&role_id)
        {
            break;
        }
    }

    moderator
        .send(&ClientMessage::CreateRole {
            guild_id,
            name: "Sneaky".into(),
            color: None,
            permissions: vec![Permission::ManageGuild],
        })
        .await
        .unwrap();
    let err = next_error(&mut moderator).await;
    assert!(err.contains("owner-only"), "got: {err}");

    moderator
        .send(&ClientMessage::CreateRole {
            guild_id,
            name: "Banhammer".into(),
            color: None,
            permissions: vec![Permission::BanMembers],
        })
        .await
        .unwrap();
    let err = next_error(&mut moderator).await;
    assert!(err.contains("don't hold it yourself"), "got: {err}");

    moderator
        .send(&ClientMessage::DeleteRole { guild_id, role_id })
        .await
        .unwrap();
    let err = next_error(&mut moderator).await;
    assert!(err.contains("owner-only"), "got: {err}");

    moderator
        .send(&ClientMessage::CreateRole {
            guild_id,
            name: "Doorman".into(),
            color: None,
            permissions: vec![Permission::KickMembers],
        })
        .await
        .unwrap();
    let ok = loop {
        match next_timeout(&mut moderator).await {
            ServerMessage::GuildRoles { roles, .. } => {
                break roles.iter().any(|r| r.name == "Doorman");
            }
            ServerMessage::Error { message } => panic!("unexpected error: {message}"),
            _ => {}
        }
    };
    assert!(ok);

    handle.abort();
}

#[tokio::test]
async fn system_guild_is_immutable() {
    let (url, handle) = spawn_gateway().await;

    let user_id = BotIdentity::generate();
    let (mut session, guilds) = connect_user(&url, &user_id, "Anyone").await;
    let lobby = guilds
        .iter()
        .find(|g| g.owner_pubkey.is_empty())
        .expect("seeded system guild present")
        .id;

    session
        .send(&ClientMessage::CreateRole {
            guild_id: lobby,
            name: "Takeover".into(),
            color: None,
            permissions: vec![Permission::ManageGuild],
        })
        .await
        .unwrap();
    let err = next_error(&mut session).await;
    assert!(err.contains("manage_roles"), "got: {err}");

    session
        .send(&ClientMessage::SetGuildAccent {
            guild_id: lobby,
            accent: Some("#fff".into()),
        })
        .await
        .unwrap();
    let err = next_error(&mut session).await;
    assert!(err.contains("manage_guild"), "got: {err}");

    handle.abort();
}

#[tokio::test]
async fn private_guild_hidden_and_invite_flow() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Secret Club").await;

    owner
        .send(&ClientMessage::SetGuildVisibility {
            guild_id,
            visibility: dioxusfun_server::protocol::GuildVisibility::Private,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildUpdate(g) = next_timeout(&mut owner).await
            && g.id == guild_id
        {
            break;
        }
    }

    let guest_id = BotIdentity::generate();
    let mut guest = Bot::connect_as_user(&url, &guest_id, "Guest")
        .await
        .unwrap();
    let catalog = loop {
        if let ServerMessage::Ready { catalog, .. } = next_timeout(&mut guest).await {
            break catalog;
        }
    };
    assert!(
        !catalog.iter().any(|g| g.id == guild_id),
        "private guild leaked into the catalog"
    );
    guest
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    let err = next_error(&mut guest).await;
    assert!(err.contains("invite-only"), "got: {err}");

    owner
        .send(&ClientMessage::CreateInvite {
            guild_id,
            rotate: false,
            expires_in_secs: None,
            max_uses: None,
        })
        .await
        .unwrap();
    let code = loop {
        if let ServerMessage::GuildInvite {
            guild_id: gid,
            code,
            ..
        } = next_timeout(&mut owner).await
            && gid == guild_id
        {
            break code;
        }
    };
    assert_eq!(code.len(), 12, "high-entropy code expected");
    guest
        .send(&ClientMessage::JoinByInvite {
            code: code.clone(),
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { guild, .. } = next_timeout(&mut guest).await {
            assert_eq!(guild.id, guild_id);
            break;
        }
    }

    owner
        .send(&ClientMessage::CreateInvite {
            guild_id,
            rotate: true,
            expires_in_secs: None,
            max_uses: None,
        })
        .await
        .unwrap();
    let rotated = loop {
        if let ServerMessage::GuildInvite { code, .. } = next_timeout(&mut owner).await {
            break code;
        }
    };
    assert_ne!(rotated, code);
    let late_id = BotIdentity::generate();
    let (mut late, _) = connect_user(&url, &late_id, "Late").await;
    late.send(&ClientMessage::JoinByInvite {
        code,
        accept: false,
        pow_nonce: None,
    })
    .await
    .unwrap();
    let err = next_error(&mut late).await;
    assert!(err.contains("unknown or expired"), "got: {err}");

    handle.abort();
}

#[tokio::test]
async fn kick_removes_and_ban_blocks() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let target_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Bouncy Castle").await;

    let (mut target, _) = connect_user(&url, &target_id, "Target").await;
    target
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut target).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    owner
        .send(&ClientMessage::KickMember {
            guild_id,
            user_pubkey: target_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildDelete { guild_id: gid } = next_timeout(&mut target).await {
            assert_eq!(gid, guild_id);
            break;
        }
    }
    loop {
        if let ServerMessage::MemberRemove { user_pubkey, .. } = next_timeout(&mut owner).await {
            assert_eq!(user_pubkey, target_id.pubkey());
            break;
        }
    }

    target
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut target).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    owner
        .send(&ClientMessage::BanMember {
            guild_id,
            user_pubkey: target_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut target).await,
            ServerMessage::GuildDelete { .. }
        ) {
            break;
        }
    }
    let bans = loop {
        if let ServerMessage::GuildBans { users, .. } = next_timeout(&mut owner).await {
            break users;
        }
    };
    assert!(bans.iter().any(|u| u.pubkey == target_id.pubkey()));

    target
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    let err = next_error(&mut target).await;
    assert!(err.contains("banned"), "got: {err}");

    owner
        .send(&ClientMessage::CreateInvite {
            guild_id,
            rotate: false,
            expires_in_secs: None,
            max_uses: None,
        })
        .await
        .unwrap();
    let code = loop {
        if let ServerMessage::GuildInvite { code, .. } = next_timeout(&mut owner).await {
            break code;
        }
    };
    target
        .send(&ClientMessage::JoinByInvite {
            code,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    let err = next_error(&mut target).await;
    assert!(
        err.contains("banned"),
        "a valid invite must not beat a ban; got: {err}"
    );

    owner
        .send(&ClientMessage::UnbanMember {
            guild_id,
            user_pubkey: target_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let bans = loop {
        if let ServerMessage::GuildBans { users, .. } = next_timeout(&mut owner).await {
            break users;
        }
    };
    assert!(bans.is_empty());
    target
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut target).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    handle.abort();
}

#[tokio::test]
async fn moderation_guard_rails() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let mod_a_id = BotIdentity::generate();
    let mod_b_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Guarded").await;

    let (mut mod_a, _) = connect_user(&url, &mod_a_id, "ModA").await;
    let (mut mod_b, _) = connect_user(&url, &mod_b_id, "ModB").await;
    for m in [&mut mod_a, &mut mod_b] {
        m.send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
        loop {
            if matches!(next_timeout(m).await, ServerMessage::GuildJoined { .. }) {
                break;
            }
        }
    }
    owner
        .send(&ClientMessage::CreateRole {
            guild_id,
            name: "Mod".into(),
            color: None,
            permissions: vec![Permission::KickMembers],
        })
        .await
        .unwrap();
    let role_id = loop {
        if let ServerMessage::GuildRoles { roles, .. } = next_timeout(&mut owner).await {
            break roles[0].id;
        }
    };
    for pk in [mod_a_id.pubkey(), mod_b_id.pubkey()] {
        owner
            .send(&ClientMessage::AssignRole {
                guild_id,
                role_id,
                user_pubkey: pk.to_string(),
            })
            .await
            .unwrap();
    }
    let mut assigned = 0;
    while assigned < 2 {
        if let ServerMessage::MemberUpdate(m) = next_timeout(&mut mod_a).await
            && m.roles.contains(&role_id)
        {
            assigned += 1;
        }
    }

    mod_a
        .send(&ClientMessage::KickMember {
            guild_id,
            user_pubkey: owner_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let err = next_error(&mut mod_a).await;
    assert!(err.contains("owner"), "got: {err}");

    mod_a
        .send(&ClientMessage::KickMember {
            guild_id,
            user_pubkey: mod_a_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let err = next_error(&mut mod_a).await;
    assert!(err.contains("yourself"), "got: {err}");

    mod_a
        .send(&ClientMessage::KickMember {
            guild_id,
            user_pubkey: mod_b_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let err = next_error(&mut mod_a).await;
    assert!(err.contains("only the owner"), "got: {err}");

    let bot_id = BotIdentity::generate();
    owner
        .send(&ClientMessage::InstallBot {
            guild_id,
            bot_pubkey: bot_id.pubkey().to_string(),
            name: "HelpBot".into(),
            permissions: vec![Permission::SendMessages],
            intents: vec![],
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
    owner
        .send(&ClientMessage::KickMember {
            guild_id,
            user_pubkey: bot_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let err = next_error(&mut owner).await;
    assert!(err.contains("uninstall"), "got: {err}");

    owner
        .send(&ClientMessage::KickMember {
            guild_id,
            user_pubkey: mod_b_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut mod_b).await,
            ServerMessage::GuildDelete { .. }
        ) {
            break;
        }
    }

    mod_a
        .send(&ClientMessage::LeaveGuild { guild_id })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut mod_a).await,
            ServerMessage::GuildDelete { .. }
        ) {
            break;
        }
    }
    owner
        .send(&ClientMessage::LeaveGuild { guild_id })
        .await
        .unwrap();
    let err = next_error(&mut owner).await;
    assert!(err.contains("transfer"), "got: {err}");

    handle.abort();
}

#[tokio::test]
async fn channel_crud_gated_and_broadcast() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, first_text) = create_guild(&mut owner, "Builders").await;

    let (mut member, _) = connect_user(&url, &member_id, "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut member).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    member
        .send(&ClientMessage::CreateChannel {
            guild_id,
            name: "nope".into(),
            kind: ChannelKind::Text,
            topic: None,
        })
        .await
        .unwrap();
    let err = next_error(&mut member).await;
    assert!(err.contains("manage_channels"), "got: {err}");

    owner
        .send(&ClientMessage::CreateChannel {
            guild_id,
            name: "announcements".into(),
            kind: ChannelKind::Text,
            topic: Some("Big news only".into()),
        })
        .await
        .unwrap();
    let created = loop {
        if let ServerMessage::ChannelCreate(c) = next_timeout(&mut member).await {
            break c;
        }
    };
    assert_eq!(created.name, "announcements");
    assert_eq!(created.topic.as_deref(), Some("Big news only"));

    owner
        .send(&ClientMessage::UpdateChannel {
            channel_id: created.id,
            name: "news".into(),
            topic: created.topic.clone(),
            read_only: true,
            position: created.position,
            slowmode_secs: 0,
        })
        .await
        .unwrap();
    let updated = loop {
        if let ServerMessage::ChannelUpdate(c) = next_timeout(&mut member).await {
            break c;
        }
    };
    assert_eq!(updated.name, "news");
    assert!(updated.read_only);

    owner
        .send(&ClientMessage::DeleteChannel {
            channel_id: created.id,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ChannelDelete { channel_id, .. } = next_timeout(&mut member).await {
            assert_eq!(channel_id, created.id);
            break;
        }
    }

    owner
        .send(&ClientMessage::DeleteChannel {
            channel_id: first_text,
        })
        .await
        .unwrap();
    let err = next_error(&mut owner).await;
    assert!(err.contains("at least one text channel"), "got: {err}");

    handle.abort();
}

#[tokio::test]
async fn read_only_channel_gates_posting() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, text_channel) = create_guild(&mut owner, "Announcements").await;

    let (mut member, _) = connect_user(&url, &member_id, "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut member).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    owner
        .send(&ClientMessage::UpdateChannel {
            channel_id: text_channel,
            name: "general".into(),
            topic: None,
            read_only: true,
            position: 0,
            slowmode_secs: 0,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut member).await,
            ServerMessage::ChannelUpdate(_)
        ) {
            break;
        }
    }

    member
        .send_message(text_channel, "can I talk?")
        .await
        .unwrap();
    let err = next_error(&mut member).await;
    assert!(err.contains("read-only"), "got: {err}");

    owner
        .send_message(text_channel, "official news")
        .await
        .unwrap();
    loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut member).await {
            assert_eq!(m.content, "official news");
            break;
        }
    }

    let bot_id = BotIdentity::generate();
    owner
        .send(&ClientMessage::InstallBot {
            guild_id,
            bot_pubkey: bot_id.pubkey().to_string(),
            name: "NewsBot".into(),
            permissions: vec![Permission::SendMessages],
            intents: vec![],
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
    let mut bot = Bot::connect(&url, &bot_id, "NewsBot").await.unwrap();
    loop {
        if matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {
            break;
        }
    }
    bot.send_message(text_channel, "beep").await.unwrap();
    let err = next_error(&mut bot).await;
    assert!(err.contains("read-only"), "got: {err}");

    owner
        .send(&ClientMessage::InstallBot {
            guild_id,
            bot_pubkey: bot_id.pubkey().to_string(),
            name: "NewsBot".into(),
            permissions: vec![Permission::SendMessages, Permission::ManageMessages],
            intents: vec![],
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
    bot.send_message(text_channel, "official beep")
        .await
        .unwrap();
    loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut member).await
            && m.author.pubkey == bot_id.pubkey()
        {
            assert_eq!(m.content, "official beep");
            break;
        }
    }

    handle.abort();
}

#[tokio::test]
async fn delete_message_rules() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let author_id = BotIdentity::generate();
    let plain_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, text_channel) = create_guild(&mut owner, "Court").await;

    let (mut author, _) = connect_user(&url, &author_id, "Author").await;
    let (mut plain, _) = connect_user(&url, &plain_id, "Plain").await;
    for m in [&mut author, &mut plain] {
        m.send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
        loop {
            if matches!(next_timeout(m).await, ServerMessage::GuildJoined { .. }) {
                break;
            }
        }
    }

    author
        .send_message(text_channel, "oops, typo")
        .await
        .unwrap();
    let msg_id = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut author).await
            && m.author.pubkey == author_id.pubkey()
        {
            break m.id;
        }
    };
    author
        .send(&ClientMessage::DeleteMessage {
            channel_id: text_channel,
            message_id: msg_id,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::MessageDelete { message_id, .. } = next_timeout(&mut plain).await {
            assert_eq!(message_id, msg_id);
            break;
        }
    }

    author.send_message(text_channel, "hot take").await.unwrap();
    let msg_id = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut plain).await
            && m.author.pubkey == author_id.pubkey()
        {
            break m.id;
        }
    };
    plain
        .send(&ClientMessage::DeleteMessage {
            channel_id: text_channel,
            message_id: msg_id,
        })
        .await
        .unwrap();
    let err = next_error(&mut plain).await;
    assert!(err.contains("manage_messages"), "got: {err}");

    owner
        .send(&ClientMessage::DeleteMessage {
            channel_id: text_channel,
            message_id: msg_id,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::MessageDelete { message_id, .. } = next_timeout(&mut author).await
            && message_id == msg_id
        {
            break;
        }
    }

    handle.abort();
}

#[tokio::test]
async fn transfer_ownership_swaps_powers() {
    let (url, handle) = spawn_gateway().await;

    let old_id = BotIdentity::generate();
    let new_id = BotIdentity::generate();
    let outsider_id = BotIdentity::generate();
    let (mut old_owner, _) = connect_user(&url, &old_id, "OldOwner").await;
    let (guild_id, _) = create_guild(&mut old_owner, "Handover").await;

    let (mut new_owner, _) = connect_user(&url, &new_id, "NewOwner").await;
    new_owner
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut new_owner).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    let (mut outsider, _) = connect_user(&url, &outsider_id, "Outsider").await;
    outsider
        .send(&ClientMessage::TransferOwnership {
            guild_id,
            new_owner_pubkey: outsider_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let err = next_error(&mut outsider).await;
    assert!(err.contains("only the owner"), "got: {err}");
    old_owner
        .send(&ClientMessage::TransferOwnership {
            guild_id,
            new_owner_pubkey: outsider_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let err = next_error(&mut old_owner).await;
    assert!(err.contains("must already be a member"), "got: {err}");

    old_owner
        .send(&ClientMessage::TransferOwnership {
            guild_id,
            new_owner_pubkey: new_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let updated = loop {
        if let ServerMessage::GuildUpdate(g) = next_timeout(&mut new_owner).await
            && g.id == guild_id
        {
            break g;
        }
    };
    assert_eq!(updated.owner_pubkey, new_id.pubkey());

    old_owner
        .send(&ClientMessage::DeleteGuild { guild_id })
        .await
        .unwrap();
    let err = next_error(&mut old_owner).await;
    assert!(err.contains("owner"), "got: {err}");

    new_owner
        .send(&ClientMessage::DeleteGuild { guild_id })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut new_owner).await,
            ServerMessage::GuildDelete { .. }
        ) {
            break;
        }
    }

    handle.abort();
}

#[tokio::test]
async fn guild_branding() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Pretty").await;

    let (mut member, _) = connect_user(&url, &member_id, "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut member).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    owner
        .send(&ClientMessage::SetGuildProfile {
            guild_id,
            name: None,
            description: Some("The prettiest guild".into()),
            icon_image: None,
            banner: Some("data:image/png;base64,iVBORw0KGgo=".into()),
        })
        .await
        .unwrap();
    let updated = loop {
        if let ServerMessage::GuildUpdate(g) = next_timeout(&mut member).await
            && g.id == guild_id
        {
            break g;
        }
    };
    assert_eq!(updated.description.as_deref(), Some("The prettiest guild"));
    assert!(updated.banner.is_some());
    assert_eq!(updated.name, "Pretty", "an absent name leaves the old one");

    owner
        .send(&ClientMessage::SetGuildProfile {
            guild_id,
            name: Some("  Prettier  ".into()),
            description: Some("The prettiest guild".into()),
            icon_image: None,
            banner: updated.banner.clone(),
        })
        .await
        .unwrap();
    let renamed = loop {
        if let ServerMessage::GuildUpdate(g) = next_timeout(&mut member).await
            && g.id == guild_id
        {
            break g;
        }
    };
    assert_eq!(renamed.name, "Prettier");
    assert!(renamed.banner.is_some(), "a rename keeps the banner");

    owner
        .send(&ClientMessage::SetGuildProfile {
            guild_id,
            name: Some("   ".into()),
            description: None,
            icon_image: None,
            banner: None,
        })
        .await
        .unwrap();
    let err = next_error(&mut owner).await;
    assert!(err.contains("guild name"), "got: {err}");

    let huge = format!("data:image/png;base64,{}", "A".repeat(3_100_000));
    owner
        .send(&ClientMessage::SetGuildProfile {
            guild_id,
            name: None,
            description: None,
            icon_image: Some(huge),
            banner: None,
        })
        .await
        .unwrap();
    let err = next_error(&mut owner).await;
    assert!(err.contains("MB"), "got: {err}");

    member
        .send(&ClientMessage::SetGuildProfile {
            guild_id,
            name: None,
            description: Some("mine now".into()),
            icon_image: None,
            banner: None,
        })
        .await
        .unwrap();
    let err = next_error(&mut member).await;
    assert!(err.contains("manage_guild"), "got: {err}");

    handle.abort();
}

async fn spawn_with_operator(operator: &str) -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:19400".parse().unwrap();
    let ops = std::collections::HashSet::from([operator.to_string()]);
    let handle = dioxusfun_server::spawn(preferred, 100, test_config(ops))
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

#[tokio::test]
async fn operator_can_moderate_system_guild() {
    let op_id = BotIdentity::generate();
    let (url, handle) = spawn_with_operator(op_id.pubkey()).await;

    let (mut op, guilds) = connect_user(&url, &op_id, "Operator").await;
    let lobby = guilds
        .iter()
        .find(|g| g.owner_pubkey.is_empty())
        .expect("seeded system guild")
        .id;

    let rando_id = BotIdentity::generate();
    let (mut rando, _) = connect_user(&url, &rando_id, "Rando").await;
    rando
        .send(&ClientMessage::SetGuildAccent {
            guild_id: lobby,
            accent: Some("#abcdef".into()),
        })
        .await
        .unwrap();
    let err = next_error(&mut rando).await;
    assert!(err.contains("manage_guild"), "got: {err}");

    op.send(&ClientMessage::SetGuildAccent {
        guild_id: lobby,
        accent: Some("#abcdef".into()),
    })
    .await
    .unwrap();
    let updated = loop {
        if let ServerMessage::GuildUpdate(g) = next_timeout(&mut op).await
            && g.id == lobby
        {
            break g;
        }
    };
    assert_eq!(updated.accent.as_deref(), Some("#abcdef"));

    op.send(&ClientMessage::CreateRole {
        guild_id: lobby,
        name: "Lobby Mod".into(),
        color: None,
        permissions: vec![Permission::KickMembers],
    })
    .await
    .unwrap();
    let roles = loop {
        if let ServerMessage::GuildRoles { guild_id, roles } = next_timeout(&mut op).await
            && guild_id == lobby
        {
            break roles;
        }
    };
    assert!(roles.iter().any(|r| r.name == "Lobby Mod"));

    op.send(&ClientMessage::DeleteGuild { guild_id: lobby })
        .await
        .unwrap();
    let err = next_error(&mut op).await;
    assert!(err.contains("owner"), "delete should fail; got: {err}");

    op.send(&ClientMessage::TransferOwnership {
        guild_id: lobby,
        new_owner_pubkey: rando_id.pubkey().to_string(),
    })
    .await
    .unwrap();
    let err = next_error(&mut op).await;
    assert!(
        err.contains("system guild"),
        "transfer should fail; got: {err}"
    );

    handle.abort();
}

#[tokio::test]
async fn message_xp_is_per_guild_and_not_a_message_count() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Grinder").await;
    let (guild_id, text) = create_guild(&mut owner, "XP Farm").await;

    // Ten messages in a burst are worth one point, not ten: the cooldown is
    // what stops the ranking from measuring how fast somebody can type.
    for i in 0..10 {
        owner.send_message(text, &format!("msg {i}")).await.unwrap();
    }

    let member = loop {
        if let ServerMessage::MemberUpdate(m) = next_timeout(&mut owner).await
            && m.user.pubkey == owner_id.pubkey()
            && m.guild_id == guild_id
        {
            break m;
        }
    };
    assert_eq!(member.xp, 1, "a burst inside the cooldown earns once");

    let (guild2, _) = create_guild(&mut owner, "Fresh Start").await;
    let mut second = Bot::connect_as_user(&url, &owner_id, "Grinder")
        .await
        .unwrap();
    let members = loop {
        if let ServerMessage::Ready { members, .. } = next_timeout(&mut second).await {
            break members;
        }
    };
    let in_farm = members
        .iter()
        .find(|m| m.guild_id == guild_id && m.user.pubkey == owner_id.pubkey())
        .expect("member of XP Farm");
    let in_fresh = members
        .iter()
        .find(|m| m.guild_id == guild2 && m.user.pubkey == owner_id.pubkey())
        .expect("member of Fresh Start");
    assert_eq!(in_farm.xp, 1, "farm xp persisted on the member row");
    assert_eq!(in_fresh.xp, 0, "new guild starts at level 1 / 0 xp");

    handle.abort();
}

use dioxusfun_server::protocol::JoinGate;

fn solve_pow(challenge: &str, bits: u32) -> String {
    use sha2::{Digest, Sha256};
    let mut n: u64 = 0;
    loop {
        let nonce = n.to_string();
        let mut h = Sha256::new();
        h.update(challenge.as_bytes());
        h.update(nonce.as_bytes());
        let d = h.finalize();
        let mut seen = 0u32;
        for b in d {
            if b == 0 {
                seen += 8;
                continue;
            }
            seen += b.leading_zeros();
            break;
        }
        if seen >= bits {
            return nonce;
        }
        n += 1;
    }
}

#[tokio::test]
async fn rules_gate_requires_accept() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Gated").await;

    owner
        .send(&ClientMessage::SetJoinGate {
            guild_id,
            gate: JoinGate::Rules,
            rules: Some("Be nice.".into()),
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut owner).await,
            ServerMessage::GuildUpdate(_)
        ) {
            break;
        }
    }

    let joiner_id = BotIdentity::generate();
    let (mut joiner, _) = connect_user(&url, &joiner_id, "Joiner").await;
    joiner
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    let challenge = loop {
        match next_timeout(&mut joiner).await {
            ServerMessage::JoinChallenge { gate, rules, .. } => break (gate, rules),
            ServerMessage::GuildJoined { .. } => panic!("joined without accepting"),
            _ => {}
        }
    };
    assert_eq!(challenge.0, JoinGate::Rules);
    assert_eq!(challenge.1.as_deref(), Some("Be nice."));

    joiner
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut joiner).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }
    handle.abort();
}

#[tokio::test]
async fn pow_gate_requires_valid_nonce() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Worked").await;
    owner
        .send(&ClientMessage::SetJoinGate {
            guild_id,
            gate: JoinGate::Pow,
            rules: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut owner).await,
            ServerMessage::GuildUpdate(_)
        ) {
            break;
        }
    }

    let joiner_id = BotIdentity::generate();
    let (mut joiner, _) = connect_user(&url, &joiner_id, "Grinder").await;
    joiner
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    let (challenge, bits) = loop {
        if let ServerMessage::JoinChallenge {
            pow_challenge,
            pow_difficulty,
            ..
        } = next_timeout(&mut joiner).await
        {
            break (pow_challenge.unwrap(), pow_difficulty.unwrap());
        }
    };
    joiner
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: Some("0".into()),
        })
        .await
        .unwrap();
    match next_timeout(&mut joiner).await {
        ServerMessage::JoinChallenge { .. } => {}
        ServerMessage::GuildJoined { .. } => {
            panic!("a bogus proof of work was admitted — the gate is not gating")
        }
        other => panic!("expected a re-challenge, got {other:?}"),
    }

    let nonce = solve_pow(&challenge, bits);
    joiner
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: Some(nonce),
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut joiner).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }
    handle.abort();
}

#[tokio::test]
async fn panic_mode_blocks_joins() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Bunker").await;
    owner
        .send(&ClientMessage::SetPanicMode { guild_id, on: true })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut owner).await,
            ServerMessage::GuildUpdate(_)
        ) {
            break;
        }
    }

    let joiner_id = BotIdentity::generate();
    let (mut joiner, _) = connect_user(&url, &joiner_id, "Raider").await;
    joiner
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    let err = next_error(&mut joiner).await;
    assert!(err.contains("lockdown"), "got: {err}");
    handle.abort();
}

#[tokio::test]
async fn slowmode_throttles_posting() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, text) = create_guild(&mut owner, "Slow").await;
    let (mut member, _) = connect_user(&url, &member_id, "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut member).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    owner
        .send(&ClientMessage::UpdateChannel {
            channel_id: text,
            name: "general".into(),
            topic: None,
            read_only: false,
            position: 0,
            slowmode_secs: 30,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut member).await,
            ServerMessage::ChannelUpdate(_)
        ) {
            break;
        }
    }

    member.send_message(text, "one").await.unwrap();
    loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut member).await
            && m.content == "one"
        {
            break;
        }
    }
    member.send_message(text, "two").await.unwrap();
    let err = next_error(&mut member).await;
    assert!(err.contains("slowmode"), "got: {err}");

    owner.send_message(text, "mod says hi").await.unwrap();
    owner.send_message(text, "and again").await.unwrap();
    let mut owner_msgs = 0;
    while owner_msgs < 2 {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut owner).await
            && m.author.pubkey == owner_id.pubkey()
        {
            owner_msgs += 1;
        }
    }
    handle.abort();
}

#[tokio::test]
async fn audit_log_records_moderation() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let target_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Logged").await;
    let (mut target, _) = connect_user(&url, &target_id, "Target").await;
    target
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut target).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    owner
        .send(&ClientMessage::BanMember {
            guild_id,
            user_pubkey: target_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut owner).await,
            ServerMessage::GuildBans { .. }
        ) {
            break;
        }
    }

    owner
        .send(&ClientMessage::FetchAuditLog { guild_id })
        .await
        .unwrap();
    let entries = loop {
        if let ServerMessage::AuditLog { entries, .. } = next_timeout(&mut owner).await {
            break entries;
        }
    };
    assert!(
        entries
            .iter()
            .any(|e| e.action == "ban" && e.target == target_id.pubkey()),
        "ban recorded in audit log"
    );
    handle.abort();
}

#[tokio::test]
async fn community_template_seeds_roles_and_channels() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    owner
        .send(&ClientMessage::CreateGuild {
            name: "FOSS Proj".into(),
            template: Some("foss".into()),
        })
        .await
        .unwrap();
    let (channels, roles) = loop {
        if let ServerMessage::GuildJoined {
            channels, roles, ..
        } = next_timeout(&mut owner).await
        {
            break (channels, roles);
        }
    };
    assert!(
        channels
            .iter()
            .any(|c| c.name == "announcements" && c.read_only)
    );
    assert!(channels.iter().any(|c| c.name == "dev"));
    assert!(roles.iter().any(|r| r.name == "Maintainer"));
    handle.abort();
}

#[tokio::test]
async fn fetch_catalog_returns_public_guilds_paginated() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    for n in ["Alpha", "Bravo", "Charlie"] {
        create_guild(&mut owner, n).await;
    }

    owner
        .send(&ClientMessage::FetchCatalog {
            offset: 0,
            limit: 2,
        })
        .await
        .unwrap();
    let (page0, total) = loop {
        if let ServerMessage::GuildCatalog {
            guilds,
            offset,
            total,
        } = next_timeout(&mut owner).await
        {
            assert_eq!(offset, 0);
            break (guilds, total);
        }
    };
    assert_eq!(page0.len(), 2, "page honors the limit");
    assert!(total >= 3, "total counts all public guilds, got {total}");

    owner
        .send(&ClientMessage::FetchCatalog {
            offset: 2,
            limit: 2,
        })
        .await
        .unwrap();
    let page1 = loop {
        if let ServerMessage::GuildCatalog { guilds, offset, .. } = next_timeout(&mut owner).await {
            assert_eq!(offset, 2);
            break guilds;
        }
    };
    let ids0: Vec<_> = page0.iter().map(|g| g.id).collect();
    assert!(
        page1.iter().all(|g| !ids0.contains(&g.id)),
        "pages don't overlap"
    );
    handle.abort();
}

#[tokio::test]
async fn creating_a_guild_no_longer_floods_bystanders_with_catalog() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let bystander_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (mut bystander, _) = connect_user(&url, &bystander_id, "Bystander").await;

    owner
        .send(&ClientMessage::CreateGuild {
            name: "NoStorm".into(),
            template: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut owner).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let got_push = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        bystander.next_event(),
    )
    .await
    .ok()
    .flatten();
    if let Some(ServerMessage::GuildCatalog { .. }) = got_push {
        panic!("bystander got an unsolicited catalog push")
    }
    handle.abort();
}

#[tokio::test]
async fn a_rate_limited_channel_update_is_refused_out_loud() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (_guild_id, text) = create_guild(&mut owner, "Busy").await;

    for i in 0..40 {
        owner
            .send(&ClientMessage::UpdateChannel {
                channel_id: text,
                name: format!("general-{i}"),
                topic: None,
                read_only: false,
                position: 0,
                slowmode_secs: 0,
            })
            .await
            .unwrap();
    }

    let message = next_error(&mut owner).await;
    assert_eq!(
        message,
        dioxusfun_server::gateway::RATE_LIMITED,
        "a dropped update must name why it was dropped"
    );

    handle.abort();
}

async fn mint_invite(
    owner: &mut Bot,
    guild_id: Id,
    expires_in_secs: Option<u64>,
    max_uses: Option<u32>,
) -> (String, Option<i64>, Option<u32>, u32) {
    owner
        .send(&ClientMessage::CreateInvite {
            guild_id,
            rotate: true,
            expires_in_secs,
            max_uses,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildInvite {
            guild_id: gid,
            code,
            expires_at_ms,
            max_uses,
            uses,
        } = next_timeout(owner).await
            && gid == guild_id
        {
            return (code, expires_at_ms, max_uses, uses);
        }
    }
}

#[tokio::test]
async fn an_invite_stops_working_once_its_uses_are_spent() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Capped").await;

    let (code, expires_at, max_uses, uses) = mint_invite(&mut owner, guild_id, None, Some(1)).await;
    assert_eq!(max_uses, Some(1));
    assert_eq!(uses, 0, "a fresh code has spent nothing");
    assert_eq!(expires_at, None, "no TTL was asked for");

    let (mut first, _) = connect_user(&url, &BotIdentity::generate(), "First").await;
    first
        .send(&ClientMessage::JoinByInvite {
            code: code.clone(),
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    let joined = loop {
        match next_timeout(&mut first).await {
            ServerMessage::GuildJoined { guild, .. } => break guild.id,
            ServerMessage::Error { message } => panic!("first join refused: {message}"),
            _ => {}
        }
    };
    assert_eq!(joined, guild_id);

    let (mut second, _) = connect_user(&url, &BotIdentity::generate(), "Second").await;
    second
        .send(&ClientMessage::JoinByInvite {
            code: code.clone(),
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    assert_eq!(
        next_error(&mut second).await,
        "unknown or expired invite code"
    );

    handle.abort();
}

#[tokio::test]
async fn an_expired_invite_is_refused_without_a_challenge() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Fleeting").await;

    let (code, expires_at, _, _) = mint_invite(&mut owner, guild_id, Some(0), None).await;
    assert!(expires_at.is_some(), "a TTL was asked for");

    let (mut guest, _) = connect_user(&url, &BotIdentity::generate(), "Guest").await;
    guest
        .send(&ClientMessage::JoinByInvite {
            code,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    assert_eq!(
        next_error(&mut guest).await,
        "unknown or expired invite code"
    );

    handle.abort();
}

#[tokio::test]
async fn fetching_an_invite_replaces_a_dead_one() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Stale").await;

    let (dead, _, _, _) = mint_invite(&mut owner, guild_id, Some(0), None).await;

    owner
        .send(&ClientMessage::CreateInvite {
            guild_id,
            rotate: false,
            expires_in_secs: None,
            max_uses: None,
        })
        .await
        .unwrap();
    let fresh = loop {
        if let ServerMessage::GuildInvite { code, .. } = next_timeout(&mut owner).await {
            break code;
        }
    };
    assert_ne!(fresh, dead, "an expired code must not be handed back");

    handle.abort();
}

#[tokio::test]
async fn a_reorder_does_not_overwrite_a_concurrent_edit() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, first) = create_guild(&mut owner, "Ordered").await;

    owner
        .send(&ClientMessage::CreateChannel {
            guild_id,
            name: "second".into(),
            kind: ChannelKind::Text,
            topic: None,
        })
        .await
        .unwrap();
    let second = loop {
        if let ServerMessage::ChannelCreate(c) = next_timeout(&mut owner).await {
            break c.id;
        }
    };

    owner
        .send(&ClientMessage::UpdateChannel {
            channel_id: first,
            name: "general".into(),
            topic: Some("the topic somebody just wrote".into()),
            read_only: false,
            position: 0,
            slowmode_secs: 0,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ChannelUpdate(c) = next_timeout(&mut owner).await
            && c.id == first
            && c.topic.is_some()
        {
            break;
        }
    }

    owner
        .send(&ClientMessage::ReorderChannels {
            guild_id,
            positions: vec![(second, 0), (first, 1)],
        })
        .await
        .unwrap();

    let mut moved = 0;
    while moved < 2 {
        if let ServerMessage::ChannelUpdate(c) = next_timeout(&mut owner).await {
            if c.id == first {
                assert_eq!(c.position, 1, "the row moved");
                assert_eq!(
                    c.topic.as_deref(),
                    Some("the topic somebody just wrote"),
                    "a reorder must not carry a stale topic back"
                );
            }
            moved += 1;
        }
    }

    handle.abort();
}

#[tokio::test]
async fn reordering_a_whole_guild_costs_one_rate_limit_hit() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, first) = create_guild(&mut owner, "Wide").await;

    let mut ids = vec![first];
    for i in 0..12 {
        owner
            .send(&ClientMessage::CreateChannel {
                guild_id,
                name: format!("c{i}"),
                kind: ChannelKind::Text,
                topic: None,
            })
            .await
            .unwrap();
        loop {
            if let ServerMessage::ChannelCreate(c) = next_timeout(&mut owner).await {
                ids.push(c.id);
                break;
            }
        }
    }

    let positions: Vec<(Id, u32)> = ids
        .iter()
        .rev()
        .enumerate()
        .map(|(i, id)| (*id, i as u32))
        .collect();
    owner
        .send(&ClientMessage::ReorderChannels {
            guild_id,
            positions: positions.clone(),
        })
        .await
        .unwrap();

    let mut seen = 0;
    while seen < positions.len() {
        match next_timeout(&mut owner).await {
            ServerMessage::ChannelUpdate(_) => seen += 1,
            ServerMessage::Error { message } => {
                panic!("a single reorder was refused: {message}")
            }
            _ => {}
        }
    }

    handle.abort();
}

#[tokio::test]
async fn a_rename_reaches_the_guild_without_a_reconnect() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let friend_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, text) = create_guild(&mut owner, "Renames").await;

    let (mut friend, _) = connect_user(&url, &friend_id, "Bartolo").await;
    friend
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut friend).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    friend
        .send(&ClientMessage::UpdateUsername {
            username: "Bartolomé".into(),
        })
        .await
        .unwrap();

    let updated = loop {
        if let ServerMessage::MemberUpdate(m) = next_timeout(&mut owner).await
            && m.user.pubkey == friend_id.pubkey()
            && m.guild_id == guild_id
        {
            break m;
        }
    };
    assert_eq!(updated.user.username, "Bartolomé");

    friend.send_message(text, "same me").await.unwrap();
    let posted = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut owner).await
            && m.author.pubkey == friend_id.pubkey()
        {
            break m;
        }
    };
    assert_eq!(posted.author.username, "Bartolomé");

    handle.abort();
}

#[tokio::test]
async fn a_bot_cannot_rename_itself() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, text) = create_guild(&mut owner, "Bots").await;

    let bot_id = BotIdentity::generate();
    owner
        .send(&ClientMessage::InstallBot {
            guild_id,
            bot_pubkey: bot_id.pubkey().to_string(),
            name: "PingBot".into(),
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

    let mut bot = dioxusfun_bot::Bot::connect(&url, &bot_id, "PingBot")
        .await
        .unwrap();
    loop {
        if matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {
            break;
        }
    }
    bot.send(&ClientMessage::UpdateUsername {
        username: "Server Admin".into(),
    })
    .await
    .unwrap();

    let refusal = next_error(&mut bot).await;
    assert!(
        refusal.contains("bots may only"),
        "expected the bot allowlist refusal, got: {refusal}"
    );

    bot.send_message(text, "hello").await.unwrap();
    let posted = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut owner).await
            && m.author.pubkey == bot_id.pubkey()
        {
            break m;
        }
    };
    assert_eq!(posted.author.username, "PingBot");

    handle.abort();
}

async fn pow_challenge_on(session: &mut Bot, guild_id: Id) -> (String, u32) {
    session
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::JoinChallenge {
            pow_challenge: Some(c),
            pow_difficulty: Some(bits),
            ..
        } = next_timeout(session).await
        {
            return (c, bits);
        }
    }
}

/// The challenge carries the connection's nonce, so work done on one socket
/// is worthless on the next: a ban and a rejoin on the same key start over.
#[tokio::test]
async fn a_proof_of_work_dies_with_the_connection_it_was_solved_on() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Reworked").await;
    owner
        .send(&ClientMessage::SetJoinGate {
            guild_id,
            gate: JoinGate::Pow,
            rules: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut owner).await,
            ServerMessage::GuildUpdate(_)
        ) {
            break;
        }
    }

    let joiner_id = BotIdentity::generate();
    let (mut first, _) = connect_user(&url, &joiner_id, "Grinder").await;
    let (challenge_a, bits) = pow_challenge_on(&mut first, guild_id).await;
    let solved_on_a = solve_pow(&challenge_a, bits);
    drop(first);

    let (mut second, _) = connect_user(&url, &joiner_id, "Grinder").await;
    let (challenge_b, _) = pow_challenge_on(&mut second, guild_id).await;
    assert_ne!(challenge_a, challenge_b, "a new socket gets new work");

    second
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: Some(solved_on_a),
        })
        .await
        .unwrap();
    match next_timeout(&mut second).await {
        ServerMessage::JoinChallenge { .. } => {}
        ServerMessage::GuildJoined { .. } => panic!("yesterday's work was accepted today"),
        other => panic!("expected a re-challenge, got {other:?}"),
    }

    second
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: Some(solve_pow(&challenge_b, bits)),
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut second).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }
    handle.abort();
}

#[tokio::test]
async fn activity_reaches_guild_peers_and_dies_with_the_socket() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Arcade").await;

    let (mut member, _) = connect_user(&url, &member_id, "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut member).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }
    owner
        .send(&ClientMessage::SetActivity {
            activity: Some(Activity {
                kind: ActivityKind::Playing,
                name: "  Factorio\u{202E}  ".into(),
                details: Some("Seablock".into()),
                state: None,
                started_ms: Some(1_700_000_000_000),
            }),
        })
        .await
        .unwrap();

    let seen = loop {
        if let ServerMessage::ActivityUpdate(u) = next_timeout(&mut member).await {
            break u;
        }
    };
    assert_eq!(seen.pubkey, owner_id.pubkey());
    let activity = seen.activity.expect("a start carries the activity");
    assert_eq!(
        activity.name, "Factorio",
        "trimmed, and the bidi override gone"
    );
    assert_eq!(activity.details.as_deref(), Some("Seablock"));

    // A socket that arrives mid-game is told about it in its own snapshot.
    let mut latecomer = Bot::connect_as_user(&url, &member_id, "Member")
        .await
        .unwrap();
    let activities = loop {
        if let ServerMessage::Ready { activities, .. } = next_timeout(&mut latecomer).await {
            break activities;
        }
    };
    let mine = activities
        .iter()
        .find(|u| u.pubkey == owner_id.pubkey())
        .expect("the snapshot carries a game already in progress");
    assert_eq!(
        mine.activity.as_ref().map(|a| a.name.as_str()),
        Some("Factorio")
    );

    owner
        .send(&ClientMessage::SetActivity { activity: None })
        .await
        .unwrap();
    let cleared = loop {
        if let ServerMessage::ActivityUpdate(u) = next_timeout(&mut member).await {
            break u;
        }
    };
    assert!(cleared.activity.is_none(), "a stop is spelled as None");

    // And a game that never got to say it stopped, because the app was killed.
    owner
        .send(&ClientMessage::SetActivity {
            activity: Some(Activity {
                kind: ActivityKind::Playing,
                name: "Terraria".into(),
                details: None,
                state: None,
                started_ms: None,
            }),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ActivityUpdate(u) = next_timeout(&mut member).await
            && u.activity.is_some()
        {
            break;
        }
    }
    drop(owner);
    let after_drop = loop {
        if let ServerMessage::ActivityUpdate(u) = next_timeout(&mut member).await {
            break u;
        }
    };
    assert!(
        after_drop.activity.is_none(),
        "a dropped socket leaves nobody playing forever"
    );

    handle.abort();
}

#[tokio::test]
async fn experience_is_earned_once_a_minute_by_people_only() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, text_channel) = create_guild(&mut owner, "Grinder").await;

    // Every point is announced, not only the ones that change a level: 0 -> 1
    // leaves everyone at level 1, and the bar under it has nothing else to read.
    owner.send_message(text_channel, "first").await.unwrap();
    let after_first = loop {
        if let ServerMessage::MemberUpdate(m) = next_timeout(&mut owner).await
            && m.user.pubkey == owner_id.pubkey()
        {
            break m;
        }
    };
    assert_eq!(after_first.xp, 1);
    assert_eq!(
        dioxusfun_server::protocol::level_progress(after_first.xp).0,
        1,
        "still level 1"
    );

    // The second message inside the cooldown earns nothing.
    owner.send_message(text_channel, "second").await.unwrap();
    owner
        .send(&ClientMessage::FetchCatalog {
            offset: 0,
            limit: 0,
        })
        .await
        .unwrap();
    let mut earned_twice = false;
    loop {
        match next_timeout(&mut owner).await {
            ServerMessage::MemberUpdate(m) if m.user.pubkey == owner_id.pubkey() => {
                earned_twice = true;
            }
            ServerMessage::GuildCatalog { .. } => break,
            _ => {}
        }
    }
    assert!(
        !earned_twice,
        "a second message inside the cooldown paid out"
    );

    // A bot is installed, not present. It earns nothing at all.
    let bot_id = BotIdentity::generate();
    owner
        .send(&ClientMessage::InstallBot {
            guild_id,
            bot_pubkey: bot_id.pubkey().to_string(),
            name: "Grindbot".into(),
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
    let mut bot = Bot::connect(&url, &bot_id, "Grindbot").await.unwrap();
    loop {
        if matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {
            break;
        }
    }
    bot.send_message(text_channel, "beep").await.unwrap();
    owner
        .send(&ClientMessage::FetchCatalog {
            offset: 0,
            limit: 0,
        })
        .await
        .unwrap();
    let mut bot_earned = false;
    loop {
        match next_timeout(&mut owner).await {
            ServerMessage::MemberUpdate(m) if m.user.pubkey == bot_id.pubkey() => {
                bot_earned = true;
            }
            ServerMessage::GuildCatalog { .. } => break,
            _ => {}
        }
    }
    assert!(!bot_earned, "a bot earned experience");

    handle.abort();
}

#[tokio::test]
async fn a_guild_sets_what_earns_experience_and_where() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, paid) = create_guild(&mut owner, "Ranked").await;

    owner
        .send(&ClientMessage::CreateChannel {
            guild_id,
            name: "unpaid".into(),
            kind: ChannelKind::Text,
            topic: None,
        })
        .await
        .unwrap();
    let unpaid = loop {
        if let ServerMessage::ChannelCreate(c) = next_timeout(&mut owner).await
            && c.name == "unpaid"
        {
            break c.id;
        }
    };

    // Five a message, no cooldown, and only in the one channel.
    owner
        .send(&ClientMessage::SetGuildLeveling {
            guild_id,
            leveling: Leveling {
                per_message: 5,
                cooldown_secs: 0,
                channels: vec![paid],
                tiers: vec![
                    LevelTier {
                        xp: 10,
                        name: "  Regular\u{202E} ".into(),
                        color: Some("#abc".into()),
                    },
                    LevelTier {
                        xp: 0,
                        name: "Newcomer".into(),
                        color: None,
                    },
                ],
                member_sort: MemberSort::Level,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let updated = loop {
        if let ServerMessage::GuildUpdate(g) = next_timeout(&mut owner).await
            && g.id == guild_id
        {
            break g;
        }
    };
    assert_eq!(
        updated
            .leveling
            .tiers
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["Newcomer", "Regular"],
        "sorted by threshold, and the bidi override filtered out"
    );
    assert_eq!(updated.leveling.member_sort, MemberSort::Level);

    owner.send_message(paid, "worth five").await.unwrap();
    let earned = loop {
        if let ServerMessage::MemberUpdate(m) = next_timeout(&mut owner).await
            && m.user.pubkey == owner_id.pubkey()
        {
            break m;
        }
    };
    assert_eq!(earned.xp, 5, "the configured amount, not one");
    assert_eq!(
        updated.leveling.tier_at(earned.xp).map(|t| t.name.as_str()),
        Some("Newcomer"),
        "the tier that starts at zero covers everyone below the next"
    );

    // A second message pays again, because this guild set no cooldown.
    owner.send_message(paid, "worth five more").await.unwrap();
    let again = loop {
        if let ServerMessage::MemberUpdate(m) = next_timeout(&mut owner).await
            && m.user.pubkey == owner_id.pubkey()
        {
            break m;
        }
    };
    assert_eq!(again.xp, 10);
    assert_eq!(
        updated.leveling.tier_at(again.xp).map(|t| t.name.as_str()),
        Some("Regular"),
        "ten reaches the named rank"
    );

    // The channel off the allowlist pays nothing.
    owner.send_message(unpaid, "worth nothing").await.unwrap();
    owner
        .send(&ClientMessage::FetchCatalog {
            offset: 0,
            limit: 0,
        })
        .await
        .unwrap();
    let mut paid_elsewhere = false;
    loop {
        match next_timeout(&mut owner).await {
            ServerMessage::MemberUpdate(m) if m.user.pubkey == owner_id.pubkey() => {
                paid_elsewhere = true;
            }
            ServerMessage::GuildCatalog { .. } => break,
            _ => {}
        }
    }
    assert!(!paid_elsewhere, "a channel off the allowlist paid out");

    // Turning it off stops everything, allowlist or not.
    owner
        .send(&ClientMessage::SetGuildLeveling {
            guild_id,
            leveling: Leveling {
                enabled: false,
                ..updated.leveling.clone()
            },
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildUpdate(g) = next_timeout(&mut owner).await
            && g.id == guild_id
        {
            break;
        }
    }
    owner.send_message(paid, "still nothing").await.unwrap();
    owner
        .send(&ClientMessage::FetchCatalog {
            offset: 0,
            limit: 0,
        })
        .await
        .unwrap();
    let mut paid_while_off = false;
    loop {
        match next_timeout(&mut owner).await {
            ServerMessage::MemberUpdate(m) if m.user.pubkey == owner_id.pubkey() => {
                paid_while_off = true;
            }
            ServerMessage::GuildCatalog { .. } => break,
            _ => {}
        }
    }
    assert!(!paid_while_off, "a disabled system paid out");

    handle.abort();
}

#[tokio::test]
async fn only_a_guild_manager_may_rewrite_the_ranks() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, _) = create_guild(&mut owner, "Ranked").await;

    let (mut member, _) = connect_user(&url, &member_id, "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(&mut member).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }

    member
        .send(&ClientMessage::SetGuildLeveling {
            guild_id,
            leveling: Leveling {
                per_message: 100,
                ..Default::default()
            },
        })
        .await
        .unwrap();
    let err = next_error(&mut member).await;
    assert!(err.contains("manage_guild"), "got: {err}");

    handle.abort();
}
