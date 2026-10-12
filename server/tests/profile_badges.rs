use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ClientMessage, Profile, ServerMessage};

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
        "dioxusfun-test-badges-{}-{}",
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
    let handle = dioxusfun_server::spawn(
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
