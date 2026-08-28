//! Anti-raid lockdown. It is a switch an owner throws, not a detector, and the
//! only thing standing behind it is one branch in `check_join_gate`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ClientMessage, Id, ServerMessage};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn temp_data_dir() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "dioxusfun-joingate-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

async fn spawn_on(dir: &Path) -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(),
        operators: Default::default(),
        data_dir: dir.to_path_buf(),
    };
    let handle = dioxusfun_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn");
    (format!("ws://{}", handle.addr), handle)
}

async fn ready(url: &str, identity: &BotIdentity, name: &str) -> Bot {
    let mut bot = Bot::connect_as_user(url, identity, name).await.unwrap();
    loop {
        if matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {
            return bot;
        }
    }
}

async fn make_guild(owner: &mut Bot) -> Id {
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Under siege".into(),
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

/// Whether the join landed. A rejection arrives as an `Error`, and joining is
/// the one thing the switch is supposed to stop.
async fn joins(bot: &mut Bot, guild_id: Id) -> Result<(), String> {
    bot.send(&ClientMessage::JoinGuild {
        guild_id,
        accept: true,
        pow_nonce: None,
    })
    .await
    .unwrap();
    loop {
        match next_timeout(bot).await {
            ServerMessage::GuildJoined { guild, .. } if guild.id == guild_id => return Ok(()),
            ServerMessage::Error { message } => return Err(message),
            _ => continue,
        }
    }
}

#[tokio::test]
async fn lockdown_turns_a_stranger_away_and_leaves_a_member_alone() {
    let (url, _h) = spawn_on(&temp_data_dir()).await;
    let owner_id = BotIdentity::generate();
    let mut owner = ready(&url, &owner_id, "Owner").await;
    let guild_id = make_guild(&mut owner).await;

    // Before the switch, so the refusal afterwards is the switch and not the
    // gate refusing everyone all along.
    let early_id = BotIdentity::generate();
    let mut early = ready(&url, &early_id, "Early").await;
    joins(&mut early, guild_id).await.expect("open guild");

    owner
        .send(&ClientMessage::SetPanicMode { guild_id, on: true })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let late_id = BotIdentity::generate();
    let mut late = ready(&url, &late_id, "Late").await;
    let refused = joins(&mut late, guild_id)
        .await
        .expect_err("a stranger must not get in during a lockdown");
    assert!(
        refused.contains("lockdown"),
        "refused for the wrong reason: {refused}"
    );

    assert!(
        joins(&mut early, guild_id).await.is_ok(),
        "someone already inside is not a raider, and the gate returns before \
         the lockdown branch for them"
    );
}

/// A raid does not stop because the host restarted. The switch lives in the
/// guild row, and this is the only thing that says the row is written.
#[tokio::test]
async fn lockdown_survives_a_restart() {
    let dir = temp_data_dir();
    let owner_id = BotIdentity::generate();

    let guild_id = {
        let (url, handle) = spawn_on(&dir).await;
        let mut owner = ready(&url, &owner_id, "Owner").await;
        let guild_id = make_guild(&mut owner).await;
        owner
            .send(&ClientMessage::SetPanicMode { guild_id, on: true })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        handle.abort();
        guild_id
    };

    let (url, _h) = spawn_on(&dir).await;
    let mut stranger = ready(&url, &BotIdentity::generate(), "Stranger").await;
    let refused = joins(&mut stranger, guild_id)
        .await
        .expect_err("the lockdown was forgotten across the restart");
    assert!(
        refused.contains("lockdown"),
        "refused for the wrong reason: {refused}"
    );
}
