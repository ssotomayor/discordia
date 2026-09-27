use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ClientMessage, Id, Permission, Role, ServerMessage};

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
        "dioxusfun-role-order-{}-{}",
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
    let preferred: SocketAddr = "127.0.0.1:19280".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn connect_user(url: &str, name: &str) -> Bot {
    let mut session = Bot::connect_as_user(url, &BotIdentity::generate(), name)
        .await
        .unwrap();
    loop {
        if let ServerMessage::Ready { .. } = next_timeout(&mut session).await {
            return session;
        }
    }
}

async fn create_guild(owner: &mut Bot) -> Id {
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Ranks".into(),
            template: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { guild, .. } = next_timeout(owner).await {
            return guild.id;
        }
    }
}

async fn next_roles(session: &mut Bot) -> Vec<Role> {
    loop {
        if let ServerMessage::GuildRoles { roles, .. } = next_timeout(session).await {
            return roles;
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

async fn add_role(owner: &mut Bot, guild_id: Id, name: &str) -> Vec<Role> {
    owner
        .send(&ClientMessage::CreateRole {
            guild_id,
            name: name.into(),
            color: None,
            permissions: vec![Permission::SendMessages],
        })
        .await
        .unwrap();
    next_roles(owner).await
}

fn names_by_position(roles: &[Role]) -> Vec<String> {
    let mut sorted = roles.to_vec();
    sorted.sort_by_key(|r| r.position);
    sorted.into_iter().map(|r| r.name).collect()
}

#[tokio::test]
async fn a_manager_reorders_roles_and_members_see_the_order() {
    let (url, handle) = spawn_gateway().await;
    let mut owner = connect_user(&url, "Owner").await;
    let guild_id = create_guild(&mut owner).await;
    let before = names_by_position(&add_role(&mut owner, guild_id, "zz-first").await);
    add_role(&mut owner, guild_id, "zz-second").await;
    let roles = add_role(&mut owner, guild_id, "zz-third").await;

    let mut order: Vec<Id> = {
        let mut sorted = roles.clone();
        sorted.sort_by_key(|r| r.position);
        sorted.into_iter().map(|r| r.id).collect()
    };
    let third = order.pop().unwrap();
    order.insert(0, third);
    owner
        .send(&ClientMessage::ReorderRoles {
            guild_id,
            order: order.clone(),
        })
        .await
        .unwrap();
    let after = next_roles(&mut owner).await;
    let named = names_by_position(&after);
    assert_eq!(named[0], "zz-third", "the moved role leads: {named:?}");
    assert_eq!(named.len(), before.len() + 2);
    let mut positions: Vec<u32> = after.iter().map(|r| r.position).collect();
    positions.sort_unstable();
    positions.dedup();
    assert_eq!(
        positions.len(),
        after.len(),
        "no two roles share a position"
    );

    handle.abort();
}

#[tokio::test]
async fn an_order_that_is_not_the_whole_list_is_refused() {
    let (url, handle) = spawn_gateway().await;
    let mut owner = connect_user(&url, "Owner").await;
    let guild_id = create_guild(&mut owner).await;
    add_role(&mut owner, guild_id, "zz-one").await;
    let roles = add_role(&mut owner, guild_id, "zz-two").await;

    let partial: Vec<Id> = roles.iter().skip(1).map(|r| r.id).collect();
    owner
        .send(&ClientMessage::ReorderRoles {
            guild_id,
            order: partial,
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("role list changed"));

    let mut doubled: Vec<Id> = roles.iter().map(|r| r.id).collect();
    doubled[0] = doubled[doubled.len() - 1];
    owner
        .send(&ClientMessage::ReorderRoles {
            guild_id,
            order: doubled,
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner).await.contains("role list changed"));

    handle.abort();
}

#[tokio::test]
async fn a_member_without_manage_roles_cannot_reorder() {
    let (url, handle) = spawn_gateway().await;
    let mut owner = connect_user(&url, "Owner").await;
    let guild_id = create_guild(&mut owner).await;
    let roles = add_role(&mut owner, guild_id, "zz-mine").await;

    let mut member = connect_user(&url, "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::GuildJoined { .. } = next_timeout(&mut member).await {
            break;
        }
    }
    member
        .send(&ClientMessage::ReorderRoles {
            guild_id,
            order: roles.iter().rev().map(|r| r.id).collect(),
        })
        .await
        .unwrap();
    assert!(next_error(&mut member).await.contains("manage_roles"));

    handle.abort();
}

#[tokio::test]
async fn a_role_made_after_a_deletion_does_not_share_a_position() {
    let (url, handle) = spawn_gateway().await;
    let mut owner = connect_user(&url, "Owner").await;
    let guild_id = create_guild(&mut owner).await;
    add_role(&mut owner, guild_id, "zz-a").await;
    let roles = add_role(&mut owner, guild_id, "zz-b").await;
    let first = roles.iter().min_by_key(|r| r.position).unwrap().id;
    owner
        .send(&ClientMessage::DeleteRole {
            guild_id,
            role_id: first,
        })
        .await
        .unwrap();
    next_roles(&mut owner).await;
    let roles = add_role(&mut owner, guild_id, "zz-c").await;
    let mut positions: Vec<u32> = roles.iter().map(|r| r.position).collect();
    positions.sort_unstable();
    positions.dedup();
    assert_eq!(positions.len(), roles.len(), "{roles:?}");

    handle.abort();
}
