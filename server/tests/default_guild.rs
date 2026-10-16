//! The guild a server sends newcomers to. Only an operator picks it, it
//! replaces the Lobby's auto-join, and it is offered once per person.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{
    ClientMessage, Guild, GuildVisibility, Id, JoinGate, ServerMessage,
};

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
        "discordia-default-guild-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

async fn spawn_with(operator: &BotIdentity) -> (String, discordia_server::ServerHandle) {
    let dir = temp_data_dir();
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: HashSet::from([operator.pubkey().to_string()]),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    };
    let handle = discordia_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn");
    (format!("ws://{}", handle.addr), handle)
}

/// The Ready's guilds and default, and the next frame if it is a join challenge.
async fn arrive(
    url: &str,
    identity: &BotIdentity,
    name: &str,
) -> (Bot, Vec<Guild>, Option<Id>, Option<JoinGate>) {
    let mut bot = Bot::connect_as_user(url, identity, name).await.unwrap();
    let (guilds, default_guild) = loop {
        if let ServerMessage::Ready {
            guilds,
            default_guild,
            ..
        } = next_timeout(&mut bot).await
        {
            break (guilds, default_guild);
        }
    };
    let challenge = match tokio::time::timeout(Duration::from_millis(300), bot.next_event()).await {
        Ok(Some(ServerMessage::JoinChallenge { gate, .. })) => Some(gate),
        _ => None,
    };
    (bot, guilds, default_guild, challenge)
}

async fn make_guild(owner: &mut Bot, name: &str) -> Id {
    owner
        .send(&ClientMessage::CreateGuild {
            name: name.into(),
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

async fn set_default(operator: &mut Bot, guild_id: Option<Id>) -> Result<(), String> {
    operator
        .send(&ClientMessage::SetDefaultGuild { guild_id })
        .await
        .unwrap();
    loop {
        match next_timeout(operator).await {
            ServerMessage::DefaultGuild { guild_id: now } if now == guild_id => return Ok(()),
            ServerMessage::Error { message } => return Err(message),
            _ => continue,
        }
    }
}

fn names(guilds: &[Guild]) -> Vec<&str> {
    guilds.iter().map(|g| g.name.as_str()).collect()
}

#[tokio::test]
async fn a_newcomer_lands_in_the_default_instead_of_the_lobby() {
    let operator_id = BotIdentity::generate();
    let (url, _h) = spawn_with(&operator_id).await;
    let (mut operator, guilds, default_guild, _) = arrive(&url, &operator_id, "Op").await;
    assert_eq!(names(&guilds), ["Lobby"]);
    assert_eq!(default_guild, None);

    let home = make_guild(&mut operator, "Home").await;
    set_default(&mut operator, Some(home)).await.unwrap();

    let (_, guilds, default_guild, challenge) = arrive(&url, &BotIdentity::generate(), "New").await;
    assert_eq!(names(&guilds), ["Home"], "the Lobby no longer auto-joins");
    assert_eq!(default_guild, Some(home));
    assert_eq!(challenge, None);

    set_default(&mut operator, None).await.unwrap();
    let (_, guilds, default_guild, _) = arrive(&url, &BotIdentity::generate(), "Later").await;
    assert_eq!(
        names(&guilds),
        ["Lobby"],
        "clearing it brings the Lobby back"
    );
    assert_eq!(default_guild, None);
}

#[tokio::test]
async fn only_an_operator_may_choose_and_only_a_public_guild() {
    let operator_id = BotIdentity::generate();
    let (url, _h) = spawn_with(&operator_id).await;
    let (mut operator, ..) = arrive(&url, &operator_id, "Op").await;
    let (mut member, ..) = arrive(&url, &BotIdentity::generate(), "Member").await;

    let theirs = make_guild(&mut member, "Theirs").await;
    let refused = set_default(&mut member, Some(theirs))
        .await
        .expect_err("a guild owner is not a server operator");
    assert!(refused.contains("operator"), "{refused}");

    member
        .send(&ClientMessage::SetGuildVisibility {
            guild_id: theirs,
            visibility: GuildVisibility::Private,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let refused = set_default(&mut operator, Some(theirs))
        .await
        .expect_err("auto-joining would walk everyone past its invite");
    assert!(refused.contains("public"), "{refused}");
}

#[tokio::test]
async fn leaving_the_default_sticks_across_reconnects() {
    let operator_id = BotIdentity::generate();
    let (url, _h) = spawn_with(&operator_id).await;
    let (mut operator, ..) = arrive(&url, &operator_id, "Op").await;
    let home = make_guild(&mut operator, "Home").await;
    set_default(&mut operator, Some(home)).await.unwrap();

    let visitor_id = BotIdentity::generate();
    let (mut visitor, guilds, ..) = arrive(&url, &visitor_id, "Visitor").await;
    assert_eq!(names(&guilds), ["Home"]);
    visitor
        .send(&ClientMessage::LeaveGuild { guild_id: home })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    drop(visitor);

    let (_, guilds, default_guild, _) = arrive(&url, &visitor_id, "Visitor").await;
    assert!(
        guilds.is_empty(),
        "left, and not pulled back in: {:?}",
        names(&guilds)
    );
    assert_eq!(default_guild, Some(home));
}

#[tokio::test]
async fn a_gated_default_is_offered_through_its_gate() {
    let operator_id = BotIdentity::generate();
    let (url, _h) = spawn_with(&operator_id).await;
    let (mut operator, ..) = arrive(&url, &operator_id, "Op").await;
    let home = make_guild(&mut operator, "Home").await;
    operator
        .send(&ClientMessage::SetJoinGate {
            guild_id: home,
            gate: JoinGate::Rules,
            rules: Some("Be kind.".into()),
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    set_default(&mut operator, Some(home)).await.unwrap();

    let visitor_id = BotIdentity::generate();
    let (_, guilds, _, challenge) = arrive(&url, &visitor_id, "Visitor").await;
    assert!(
        guilds.is_empty(),
        "not a member until the rules are accepted"
    );
    assert_eq!(challenge, Some(JoinGate::Rules));

    let (_, _, _, challenge) = arrive(&url, &visitor_id, "Visitor").await;
    assert_eq!(challenge, None, "offered once, not on every connect");
}

#[tokio::test]
async fn deleting_the_default_returns_newcomers_to_the_lobby() {
    let operator_id = BotIdentity::generate();
    let (url, _h) = spawn_with(&operator_id).await;
    let (mut operator, ..) = arrive(&url, &operator_id, "Op").await;
    let home = make_guild(&mut operator, "Home").await;
    set_default(&mut operator, Some(home)).await.unwrap();

    operator
        .send(&ClientMessage::DeleteGuild { guild_id: home })
        .await
        .unwrap();
    loop {
        if let ServerMessage::DefaultGuild { guild_id } = next_timeout(&mut operator).await {
            assert_eq!(guild_id, None);
            break;
        }
    }
    let (_, guilds, default_guild, _) = arrive(&url, &BotIdentity::generate(), "New").await;
    assert_eq!(names(&guilds), ["Lobby"]);
    assert_eq!(default_guild, None);
}

#[tokio::test]
async fn a_lockdown_defers_the_offer_until_it_lifts() {
    let operator_id = BotIdentity::generate();
    let (url, _h) = spawn_with(&operator_id).await;
    let (mut operator, ..) = arrive(&url, &operator_id, "Op").await;
    let home = make_guild(&mut operator, "Home").await;
    set_default(&mut operator, Some(home)).await.unwrap();
    operator
        .send(&ClientMessage::SetPanicMode {
            guild_id: home,
            on: true,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let visitor_id = BotIdentity::generate();
    let (visitor, guilds, ..) = arrive(&url, &visitor_id, "Visitor").await;
    assert!(guilds.is_empty(), "auto-join must not walk past a lockdown");
    drop(visitor);

    operator
        .send(&ClientMessage::SetPanicMode {
            guild_id: home,
            on: false,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (_, guilds, ..) = arrive(&url, &visitor_id, "Visitor").await;
    assert_eq!(
        names(&guilds),
        ["Home"],
        "the deferred offer is made once it lifts"
    );
}
