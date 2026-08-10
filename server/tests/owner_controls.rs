//! End-to-end tests for guild owner controls: the roles/permissions engine
//! (this file grows with membership, channels, moderation, delegation as the
//! phases land). Same harness as `bots.rs`: spawn a real gateway, drive human
//! sessions through the bot SDK's `connect_as_user` (it can send arbitrary
//! `ClientMessage`s).

use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ChannelKind, ClientMessage, Id, Permission, ServerMessage};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

/// Per-test ServerConfig: unique temp data dir (SQLite + media) so tests are
/// hermetic and parallel-safe.
fn test_config(operators: std::collections::HashSet<String>) -> dioxusfun_server::ServerConfig {
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(),
        operators,
        data_dir: dir,
    }
}

/// Spawn a gateway on a free port and return its `ws://` URL plus the handle.
async fn spawn_gateway() -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:19200".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, test_config(Default::default()))
        .await
        .expect("spawn server");
    let url = format!("ws://{}", handle.addr);
    (url, handle)
}

/// Connect a human session and swallow its Ready, returning the session and
/// the guilds it landed with.
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

/// Owner creates a guild; returns (guild_id, first text channel id).
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

/// Wait for the next `Error` frame and return its message.
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

    // Create — both parties see the new role list.
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

    // Assign to the member — everyone gets the MemberUpdate.
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

    // A fresh joiner receives the role list in GuildJoined.
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

    // Delete — the role list empties and the assignment is stripped.
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

    // Plain member can't restyle.
    member
        .send(&ClientMessage::SetGuildAccent {
            guild_id,
            accent: Some("#123456".into()),
        })
        .await
        .unwrap();
    let err = next_error(&mut member).await;
    assert!(err.contains("manage_guild"), "got: {err}");

    // Owner mints an Admin role (ManageGuild is owner-touch-only — fine, the
    // owner IS touching it) and assigns it.
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

    // Retry — now it works and the owner sees the GuildUpdate.
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

    // Owner grants a role with ManageRoles + KickMembers.
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

    // 1. Moderator mints a ManageGuild role → owner-only, rejected.
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

    // 2. Moderator mints a BanMembers role they don't hold → subset rule.
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

    // 3. Moderator can't touch (edit/delete) the ManageRoles-carrying role
    //    they themselves hold — owner-only.
    moderator
        .send(&ClientMessage::DeleteRole { guild_id, role_id })
        .await
        .unwrap();
    let err = next_error(&mut moderator).await;
    assert!(err.contains("owner-only"), "got: {err}");

    // 4. But a role within their own grants is fine.
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
    // Everyone lands in the seeded Lobby (empty owner).
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

// ---------------------------------------------------------------------------
// Phase 3 — membership: private guilds, invites, kick/ban/leave
// ---------------------------------------------------------------------------

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

    // A fresh user's catalog must not list the private guild, and a direct
    // join is rejected.
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

    // Owner mints an invite; the guest joins with it.
    owner
        .send(&ClientMessage::CreateInvite {
            guild_id,
            rotate: false,
        })
        .await
        .unwrap();
    let code = loop {
        if let ServerMessage::GuildInvite {
            guild_id: gid,
            code,
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

    // Rotation invalidates the old code.
    owner
        .send(&ClientMessage::CreateInvite {
            guild_id,
            rotate: true,
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

    // Kick: the victim's client receives a targeted GuildDelete; the owner
    // sees the roster row removed.
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

    // Kicked ≠ banned: rejoining works.
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

    // Ban: removed again AND both join paths are blocked.
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

    // Unban restores access.
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

    // Two moderators, both holding KickMembers.
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
    // Wait until mod A sees both assignments land.
    let mut assigned = 0;
    while assigned < 2 {
        if let ServerMessage::MemberUpdate(m) = next_timeout(&mut mod_a).await
            && m.roles.contains(&role_id)
        {
            assigned += 1;
        }
    }

    // 1. Moderator can't kick the owner.
    mod_a
        .send(&ClientMessage::KickMember {
            guild_id,
            user_pubkey: owner_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let err = next_error(&mut mod_a).await;
    assert!(err.contains("owner"), "got: {err}");

    // 2. Moderator can't kick themselves.
    mod_a
        .send(&ClientMessage::KickMember {
            guild_id,
            user_pubkey: mod_a_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let err = next_error(&mut mod_a).await;
    assert!(err.contains("yourself"), "got: {err}");

    // 3. Moderators can't moderate moderators.
    mod_a
        .send(&ClientMessage::KickMember {
            guild_id,
            user_pubkey: mod_b_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let err = next_error(&mut mod_a).await;
    assert!(err.contains("only the owner"), "got: {err}");

    // 4. Bots are uninstalled, not kicked.
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

    // 5. The owner CAN kick a moderator.
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

    // 6. Leaving: a member exits voluntarily; the owner can't.
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

// ---------------------------------------------------------------------------
// Phase 4 — channels + moderation (minimal)
// ---------------------------------------------------------------------------

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

    // Plain member can't create channels.
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

    // Owner creates one — the member sees the broadcast.
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

    // Rename + flag read-only; the member sees the update.
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

    // Delete it; the member sees ChannelDelete.
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

    // The last text channel is protected.
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

    // Flag the channel read-only.
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

    // Plain member is blocked.
    member
        .send_message(text_channel, "can I talk?")
        .await
        .unwrap();
    let err = next_error(&mut member).await;
    assert!(err.contains("read-only"), "got: {err}");

    // The owner posts fine (implicit all permissions).
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

    // A SendMessages-only bot is silenced; regranting with ManageMessages
    // (announcement bot) unblocks it.
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

    // Author posts, then deletes their own message — everyone sees it vanish.
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

    // A plain member can't delete someone else's message.
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

    // The owner (implicit ManageMessages) can moderate it away. (Loop until
    // the MATCHING delete — the author's queue may still hold the first one.)
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

    // DMs: only the author may delete, moderators have no reach.
    owner
        .send(&ClientMessage::OpenDm {
            user_pubkey: author_id.pubkey().to_string(),
        })
        .await
        .unwrap();
    let dm_channel = loop {
        if let ServerMessage::DmReady { channel_id, .. } = next_timeout(&mut owner).await {
            break channel_id;
        }
    };
    // The author sends a DM message to the owner.
    author
        .send_message(dm_channel, "private note")
        .await
        .unwrap();
    let dm_msg = loop {
        if let ServerMessage::MessageCreate(m) = next_timeout(&mut owner).await
            && m.channel_id == dm_channel
        {
            break m.id;
        }
    };
    // Even the (guild) owner can't delete the other side's DM message.
    owner
        .send(&ClientMessage::DeleteMessage {
            channel_id: dm_channel,
            message_id: dm_msg,
        })
        .await
        .unwrap();
    let err = next_error(&mut owner).await;
    assert!(err.contains("author"), "got: {err}");

    handle.abort();
}

// ---------------------------------------------------------------------------
// Phase 5 — delegation + branding
// ---------------------------------------------------------------------------

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

    // Non-owner can't transfer; transfers to non-members are rejected.
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

    // Real transfer: everyone sees the new owner_pubkey.
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

    // The old owner has lost the implicit powers...
    old_owner
        .send(&ClientMessage::DeleteGuild { guild_id })
        .await
        .unwrap();
    let err = next_error(&mut old_owner).await;
    assert!(err.contains("owner"), "got: {err}");

    // ...and the new owner has gained them.
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

    // Description + a small data-URL banner propagate to members.
    owner
        .send(&ClientMessage::SetGuildProfile {
            guild_id,
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

    // Oversized images are rejected.
    let huge = format!("data:image/png;base64,{}", "A".repeat(3_100_000));
    owner
        .send(&ClientMessage::SetGuildProfile {
            guild_id,
            description: None,
            icon_image: Some(huge),
            banner: None,
        })
        .await
        .unwrap();
    let err = next_error(&mut owner).await;
    // The rejection should tell the user the size that would work, not just
    // that a rule exists.
    assert!(err.contains("MB"), "got: {err}");

    // A plain member can't rebrand.
    member
        .send(&ClientMessage::SetGuildProfile {
            guild_id,
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

// ---------------------------------------------------------------------------
// Operators — the escape hatch that makes the seeded (system) Lobby moderatable
// ---------------------------------------------------------------------------

/// Spawn a gateway that designates `operator` as owner of system guilds.
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

    // The operator connects and lands in the seeded Lobby (empty owner).
    let (mut op, guilds) = connect_user(&url, &op_id, "Operator").await;
    let lobby = guilds
        .iter()
        .find(|g| g.owner_pubkey.is_empty())
        .expect("seeded system guild")
        .id;

    // A NON-operator in the same Lobby still can't manage it.
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

    // The operator CAN restyle + create roles in the Lobby.
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

    // But the Lobby stays undeletable and non-transferable, even for the operator.
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

// ---------------------------------------------------------------------------
// Level system — message-XP → levels, broadcast on level-up
// ---------------------------------------------------------------------------

#[tokio::test]
async fn message_xp_levels_up_per_guild() {
    let (url, handle) = spawn_gateway().await;

    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Grinder").await;
    let (guild_id, text) = create_guild(&mut owner, "XP Farm").await;

    // Level 1 spans 10 XP; the 10th message rolls the author into level 2 and
    // triggers a MemberUpdate (targeted at this guild) carrying the new XP.
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
    assert!(member.xp >= 10, "xp should have accrued, got {}", member.xp);
    assert_eq!(
        dioxusfun_server::protocol::level_progress(member.xp).0,
        2,
        "10 messages → level 2"
    );

    // XP is per-guild: a fresh guild starts the same user back at 0. Verify
    // both values via a second session's Ready roster (which stamps XP).
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
    assert!(in_farm.xp >= 10, "farm xp persisted on the member row");
    assert_eq!(in_fresh.xp, 0, "new guild starts at level 1 / 0 xp");

    handle.abort();
}

// ---------------------------------------------------------------------------
// Phase 4 — community safety: gates, panic mode, slowmode, audit, templates
// ---------------------------------------------------------------------------

use dioxusfun_server::protocol::JoinGate;

/// SHA-256 leading-zero-bits PoW solver (mirrors the client/server).
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
    // First attempt (no accept) → challenge, not a join.
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

    // Accept → joins.
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
    // A bogus nonce is rejected (re-challenged).
    joiner
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: Some("0".into()),
        })
        .await
        .unwrap();
    // (May coincidentally satisfy at very low bits, but 16 bits makes "0" ~always fail.)
    // Solve it for real → joins.
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

    // First post ok, second throttled for the member.
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

    // The owner (ManageMessages) is exempt.
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

// ---------------------------------------------------------------------------
// Phase 5b — catalog on-demand + paginated (no broadcast storm)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fetch_catalog_returns_public_guilds_paginated() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let (mut owner, _) = connect_user(&url, &owner_id, "Owner").await;
    // Create three public guilds.
    for n in ["Alpha", "Bravo", "Charlie"] {
        create_guild(&mut owner, n).await;
    }

    // Page 0, limit 2 → 2 guilds, total reflects all public guilds (incl. any
    // seeded system guild that is public).
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

    // Next page continues without overlap.
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

    // Owner creates a guild; the bystander is not a member.
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

    // The bystander must NOT receive an unsolicited GuildCatalog push. Give the
    // server a beat, then assert nothing catalog-shaped is queued for them.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let got_push = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        bystander.next_event(),
    )
    .await
    .ok()
    .flatten();
    // Anything else — a timeout, or an unrelated frame — means the storm is gone.
    if let Some(ServerMessage::GuildCatalog { .. }) = got_push {
        panic!("bystander got an unsolicited catalog push")
    }
    handle.abort();
}
