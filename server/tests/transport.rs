//! Phase 5a — transport bus correctness.
//!
//! The routing table replaced broadcast-everything: `deliver()` reaches only
//! the target user's connections, `broadcast()` reaches all. These tests pin
//! the behaviour that matters — a guild frame never reaches a non-member, it
//! reaches ALL of a member's devices, a DM reaches only its participants, and a
//! disconnect cleanly removes a connection from the table.
//!
//! NOTE: these validate *correctness* only. The 2k-connection load / restart-
//! recovery checkpoint is a deliberate follow-up (see docs/ROADMAP.md P5a).

use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ChannelKind, ClientMessage, Id, Message, ServerMessage};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn test_config() -> dioxusfun_server::ServerConfig {
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-transport-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(),
        operators: Default::default(),
        data_dir: dir,
    }
}

async fn spawn_gateway() -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:19400".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 200, test_config())
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn connect_user(url: &str, id: &BotIdentity, name: &str) -> Bot {
    let mut session = Bot::connect_as_user(url, id, name).await.unwrap();
    loop {
        if let ServerMessage::Ready { .. } = next_timeout(&mut session).await {
            break;
        }
    }
    session
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
                .unwrap()
                .id;
            return (guild.id, text);
        }
    }
}

async fn join(session: &mut Bot, guild_id: Id) {
    session
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: false,
            pow_nonce: None,
        })
        .await
        .unwrap();
    loop {
        if matches!(
            next_timeout(session).await,
            ServerMessage::GuildJoined { .. }
        ) {
            break;
        }
    }
}

/// Drain a session for up to `ms` and return whether a `MessageCreate` with
/// `content` ever arrives. Used for both positive and negative assertions.
async fn saw_message(session: &mut Bot, content: &str, ms: u64) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, session.next_event()).await {
            Ok(Some(ServerMessage::MessageCreate(m))) if m.content == content => return true,
            Ok(Some(_)) => continue,
            _ => return false,
        }
    }
}

#[tokio::test]
async fn guild_message_never_reaches_a_non_member() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();
    let stranger_id = BotIdentity::generate();

    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, text) = create_guild(&mut owner, "Private club").await;

    let mut member = connect_user(&url, &member_id, "Member").await;
    join(&mut member, guild_id).await;

    // Identified, live, but NOT a member of the guild.
    let mut stranger = connect_user(&url, &stranger_id, "Stranger").await;

    owner.send_message(text, "members only").await.unwrap();

    // The member receives it; the stranger never does.
    assert!(
        saw_message(&mut member, "members only", 2000).await,
        "member got the message"
    );
    assert!(
        !saw_message(&mut stranger, "members only", 800).await,
        "stranger must not receive a guild frame they aren't a member for"
    );
    handle.abort();
}

#[tokio::test]
async fn guild_message_reaches_every_device_of_a_member() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let member_id = BotIdentity::generate();

    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, text) = create_guild(&mut owner, "Two devices").await;

    // The member joins on device 1, then opens a second connection with the
    // SAME identity — both should route.
    let mut device1 = connect_user(&url, &member_id, "Member").await;
    join(&mut device1, guild_id).await;
    let mut device2 = connect_user(&url, &member_id, "Member").await;

    owner.send_message(text, "ping all devices").await.unwrap();

    assert!(
        saw_message(&mut device1, "ping all devices", 2000).await,
        "device 1 got it"
    );
    assert!(
        saw_message(&mut device2, "ping all devices", 2000).await,
        "device 2 got it"
    );
    handle.abort();
}

#[tokio::test]
async fn dm_reaches_only_its_participants() {
    let (url, handle) = spawn_gateway().await;
    let a_id = BotIdentity::generate();
    let b_id = BotIdentity::generate();
    let c_id = BotIdentity::generate();

    let mut a = connect_user(&url, &a_id, "Ana").await;
    let mut b = connect_user(&url, &b_id, "Bo").await;
    let mut c = connect_user(&url, &c_id, "Cy").await;

    // A opens a DM with B and sends a line.
    a.send(&ClientMessage::OpenDm {
        user_pubkey: b_id.pubkey().to_string(),
    })
    .await
    .unwrap();
    let dm_channel = loop {
        if let ServerMessage::DmReady { channel_id, .. } = next_timeout(&mut a).await {
            break channel_id;
        }
    };
    a.send_message(dm_channel, "just between us").await.unwrap();

    assert!(
        saw_message(&mut b, "just between us", 2000).await,
        "the DM partner receives it"
    );
    assert!(
        !saw_message(&mut c, "just between us", 800).await,
        "a third party must never receive someone else's DM"
    );
    handle.abort();
}

#[tokio::test]
async fn disconnect_removes_connection_from_routing() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let a_id = BotIdentity::generate();
    let b_id = BotIdentity::generate();

    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let (guild_id, text) = create_guild(&mut owner, "Comes and goes").await;

    let mut a = connect_user(&url, &a_id, "A").await;
    join(&mut a, guild_id).await;
    let mut b = connect_user(&url, &b_id, "B").await;
    join(&mut b, guild_id).await;

    // A disconnects; routing must shed it and keep delivering to B without error.
    drop(a);
    tokio::time::sleep(Duration::from_millis(200)).await;

    owner.send_message(text, "still flowing").await.unwrap();
    assert!(
        saw_message(&mut b, "still flowing", 2000).await,
        "remaining member still receives"
    );
    handle.abort();
}

/// Send a FetchMessages and return the resulting page (loops past any live
/// MessageCreate frames queued ahead of the reply).
async fn fetch_history(
    session: &mut Bot,
    channel_id: Id,
    limit: u32,
    before_ms: Option<i64>,
) -> Vec<Message> {
    session
        .send(&ClientMessage::FetchMessages {
            channel_id,
            limit,
            before_ms,
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::MessageHistory {
            channel_id: cid,
            messages,
        } = next_timeout(session).await
        {
            if cid == channel_id {
                return messages;
            }
        }
    }
}

#[tokio::test]
async fn message_history_pages_backward_with_before_ms() {
    let (url, handle) = spawn_gateway().await;
    let owner_id = BotIdentity::generate();
    let mut owner = connect_user(&url, &owner_id, "Owner").await;
    let (_guild_id, text) = create_guild(&mut owner, "History").await;

    // Post 5 messages with small gaps so their timestamps are distinct.
    for i in 0..5 {
        owner.send_message(text, &format!("m{i}")).await.unwrap();
        tokio::time::sleep(Duration::from_millis(8)).await;
    }

    // Newest page.
    let page1 = fetch_history(&mut owner, text, 2, None).await;
    assert_eq!(page1.len(), 2, "first page honors the limit");
    let p1: std::collections::HashSet<_> = page1.iter().map(|m| m.content.clone()).collect();
    assert!(
        p1.contains("m4") && p1.contains("m3"),
        "newest two, got {p1:?}"
    );

    // Page backward from the oldest message we hold.
    let oldest_ms = page1
        .iter()
        .map(|m| m.created_at.timestamp_millis())
        .min()
        .unwrap();
    let page2 = fetch_history(&mut owner, text, 2, Some(oldest_ms)).await;
    assert_eq!(page2.len(), 2, "second page honors the limit");
    let p2: std::collections::HashSet<_> = page2.iter().map(|m| m.content.clone()).collect();
    assert!(p2.is_disjoint(&p1), "pages don't overlap: {p1:?} vs {p2:?}");
    assert!(
        p2.contains("m2") && p2.contains("m1"),
        "next two older, got {p2:?}"
    );
    handle.abort();
}
