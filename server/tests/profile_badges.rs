use std::net::SocketAddr;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{ClientMessage, Profile, ServerMessage};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn test_config(operators: std::collections::HashSet<String>) -> discordia_server::ServerConfig {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "discordia-test-badges-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators,
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    }
}

async fn connect_user(url: &str, id: &BotIdentity, name: &str) -> (Bot, Vec<Profile>) {
    let mut session = Bot::connect_as_user(url, id, name).await.unwrap();
    loop {
        if let ServerMessage::Ready { profiles, .. } = next_timeout(&mut session).await {
            return (session, profiles);
        }
    }
}

fn badges(hide_guild_owner: bool, hide_operator: bool) -> ClientMessage {
    ClientMessage::SetProfile {
        avatar: None,
        banner: None,
        bio: None,
        status: None,
        custom_status: None,
        hide_guild_owner,
        hide_operator,
        updated_at: 0,
    }
}

async fn next_profile_of(session: &mut Bot, pubkey: &str) -> Profile {
    loop {
        if let ServerMessage::ProfileUpdate(p) = next_timeout(session).await
            && p.pubkey == pubkey
        {
            return p;
        }
    }
}

#[tokio::test]
async fn the_operator_badge_is_the_servers_to_stamp_and_the_operators_to_hide() {
    let op = BotIdentity::generate();
    let preferred: SocketAddr = "127.0.0.1:19260".parse().unwrap();
    let handle = discordia_server::spawn(
        preferred,
        100,
        test_config([op.pubkey().to_string()].into()),
    )
    .await
    .expect("spawn server");
    let url = format!("ws://{}", handle.addr);

    let (mut host, _) = connect_user(&url, &op, "Host").await;
    let guest_id = BotIdentity::generate();
    let (mut guest, profiles) = connect_user(&url, &guest_id, "Guest").await;
    let stub = profiles
        .iter()
        .find(|p| p.pubkey == op.pubkey())
        .expect("an operator who never edited a profile is still listed");
    assert!(stub.operator);

    guest.send(&badges(false, false)).await.unwrap();
    let theirs = next_profile_of(&mut host, guest_id.pubkey()).await;
    assert!(!theirs.operator, "nobody becomes an operator by saying so");

    host.send(&badges(false, true)).await.unwrap();
    let hidden = next_profile_of(&mut guest, op.pubkey()).await;
    assert!(hidden.hide_operator);
    assert!(!hidden.operator, "a hidden operator is not one on the wire");

    drop(guest);
    let (_guest, profiles) = connect_user(&url, &guest_id, "Guest").await;
    let op_profile = profiles
        .iter()
        .find(|p| p.pubkey == op.pubkey())
        .expect("the operator's profile");
    assert!(!op_profile.operator, "the choice outlives the broadcast");

    host.send(&badges(true, false)).await.unwrap();
    let shown = loop {
        let p = next_profile_of(&mut host, op.pubkey()).await;
        if p.hide_guild_owner {
            break p;
        }
    };
    assert!(
        shown.operator,
        "hiding the crown leaves the operator badge alone"
    );
}

fn bio(text: &str, updated_at: i64) -> ClientMessage {
    ClientMessage::SetProfile {
        avatar: None,
        banner: None,
        bio: Some(text.into()),
        status: None,
        custom_status: None,
        hide_guild_owner: false,
        hide_operator: false,
        updated_at,
    }
}

#[tokio::test]
async fn a_second_device_with_an_older_profile_cannot_put_it_back() {
    let preferred: SocketAddr = "127.0.0.1:19261".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 100, test_config(Default::default()))
        .await
        .expect("spawn server");
    let url = format!("ws://{}", handle.addr);
    let me = BotIdentity::generate();

    let (mut laptop, _) = connect_user(&url, &me, "Me").await;
    laptop.send(&bio("new", 2_000)).await.unwrap();
    assert_eq!(
        next_profile_of(&mut laptop, me.pubkey())
            .await
            .bio
            .as_deref(),
        Some("new")
    );

    let (mut desktop, _) = connect_user(&url, &me, "Me").await;
    desktop.send(&bio("old", 1_000)).await.unwrap();
    let answer = next_profile_of(&mut desktop, me.pubkey()).await;
    assert_eq!(
        answer.bio.as_deref(),
        Some("new"),
        "the stale device is told what is held"
    );
    assert_eq!(answer.updated_at, 2_000);

    drop(desktop);
    let (_again, profiles) = connect_user(&url, &me, "Me").await;
    let held = profiles.iter().find(|p| p.pubkey == me.pubkey()).unwrap();
    assert_eq!(held.bio.as_deref(), Some("new"));
}
