//! Events: only `ManageEvents` schedules them, one hosted where someone can't
//! see goes nowhere near them, and each run is announced once as it starts.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::time::Duration;

use chrono::Utc;
use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::media::MediaStore;
use discordia_server::protocol::{
    ChannelAccess, ChannelKind, ClientMessage, EventDraft, EventRepeat, GuildEvent, Id,
    ServerMessage, User,
};
use discordia_server::state::AppState;
use discordia_server::store::Store;

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

async fn next_matching<T>(
    session: &mut Bot,
    mut pick: impl FnMut(ServerMessage) -> Option<T>,
) -> T {
    loop {
        if let Some(found) = pick(next_timeout(session).await) {
            return found;
        }
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "discordia-events-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ))
}

async fn spawn_gateway() -> (String, discordia_server::ServerHandle) {
    let dir = temp_dir("gateway");
    let cfg = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        launch: Default::default(),
        data_dir: dir,
        admission: Default::default(),
    };
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn server");
    (format!("ws://{}", handle.addr), handle)
}

async fn connect(url: &str, name: &str) -> (Bot, String) {
    let identity = BotIdentity::generate();
    let mut bot = Bot::connect_as_user(url, &identity, name).await.unwrap();
    next_matching(&mut bot, |m| {
        matches!(m, ServerMessage::Ready { events: true, .. }).then_some(())
    })
    .await;
    (bot, identity.pubkey().to_string())
}

fn draft(name: &str, channel_id: Option<Id>, starts_in_minutes: i64) -> EventDraft {
    let starts_at = Utc::now() + chrono::Duration::minutes(starts_in_minutes);
    EventDraft {
        name: name.into(),
        description: "bring snacks".into(),
        starts_at,
        ends_at: starts_at + chrono::Duration::hours(2),
        channel_id,
        repeat: EventRepeat::Weekly,
        cover: None,
    }
}

async fn next_events(session: &mut Bot, len: usize) -> Vec<GuildEvent> {
    next_matching(session, |m| match m {
        ServerMessage::GuildEvents { events, .. } if events.len() == len => Some(events),
        _ => None,
    })
    .await
}

#[tokio::test]
async fn only_manage_events_schedules_and_a_hidden_host_hides_the_event() {
    let (url, _server) = spawn_gateway().await;
    let (mut owner, owner_key) = connect(&url, "Owner").await;
    owner
        .send(&ClientMessage::CreateGuild {
            name: "Events".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild_id, voice) = next_matching(&mut owner, |m| match m {
        ServerMessage::GuildJoined {
            guild, channels, ..
        } => channels
            .into_iter()
            .find(|c| c.kind == ChannelKind::Voice)
            .map(|c| (guild.id, c.id)),
        _ => None,
    })
    .await;
    let (mut member, _) = connect(&url, "Member").await;
    member
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    next_matching(&mut member, |m| {
        matches!(m, ServerMessage::GuildJoined { .. }).then_some(())
    })
    .await;

    member
        .send(&ClientMessage::SaveGuildEvent {
            guild_id,
            event_id: None,
            draft: draft("Raid", None, 60),
        })
        .await
        .unwrap();
    let refused = next_matching(&mut member, |m| match m {
        ServerMessage::Error { message } => Some(message),
        _ => None,
    })
    .await;
    assert!(refused.contains("manage_events"), "{refused}");

    owner
        .send(&ClientMessage::SetChannelAccess {
            channel_id: voice,
            access: Some(ChannelAccess {
                roles: Vec::new(),
                users: vec![owner_key],
            }),
        })
        .await
        .unwrap();
    owner
        .send(&ClientMessage::SaveGuildEvent {
            guild_id,
            event_id: None,
            draft: draft("Secret raid", Some(voice), 60),
        })
        .await
        .unwrap();
    let shown = next_events(&mut owner, 1).await;
    assert_eq!(shown[0].name, "Secret raid");
    assert_eq!(shown[0].channel_id, Some(voice));

    member
        .send(&ClientMessage::FetchGuildEvents { guild_id })
        .await
        .unwrap();
    let seen = next_matching(&mut member, |m| match m {
        ServerMessage::GuildEvents { events, .. } => Some(events),
        ServerMessage::Error { message } => panic!("{message}"),
        _ => None,
    })
    .await;
    assert!(
        seen.is_empty(),
        "an event in a channel they can't see: {seen:?}"
    );

    owner
        .send(&ClientMessage::SetChannelAccess {
            channel_id: voice,
            access: None,
        })
        .await
        .unwrap();
    let opened = next_events(&mut member, 1).await;
    assert_eq!(
        opened[0].name, "Secret raid",
        "opening the channel shows its event"
    );

    owner
        .send(&ClientMessage::DeleteGuildEvent {
            guild_id,
            event_id: opened[0].id,
        })
        .await
        .unwrap();
    assert!(next_events(&mut member, 0).await.is_empty());
}

async fn fresh_state() -> AppState {
    let dir = temp_dir("state");
    let store = Store::open_in(&dir).await.expect("open store");
    let media = MediaStore::open(
        dir.join("media"),
        discordia_server::media::DEFAULT_MAX_BYTES,
    )
    .expect("open media");
    AppState::load_or_seed(store, media, HashSet::new())
        .await
        .expect("load_or_seed")
}

#[tokio::test]
async fn a_started_run_is_announced_once_in_its_text_channel() {
    let state = fresh_state().await;
    let owner = User {
        pubkey: "11".repeat(32),
        username: "Owner".into(),
    };
    let (guild, channels, ..) = state.create_guild("Events", None, &owner).await.unwrap();
    let texts: Vec<Id> = channels
        .iter()
        .filter(|c| c.kind == ChannelKind::Text)
        .map(|c| c.id)
        .collect();
    let save = |draft| ClientMessage::SaveGuildEvent {
        guild_id: guild.id,
        event_id: None,
        draft,
    };
    state
        .handle_event_command(&owner, save(draft("Movie night", None, -5)))
        .await
        .unwrap();
    state
        .handle_event_command(&owner, save(draft("Later", None, 120)))
        .await
        .unwrap();

    state.announce_events().await;
    state.announce_events().await;
    let mut notices = Vec::new();
    for text in texts {
        notices.extend(
            state
                .store
                .history(text, 50, None, None)
                .await
                .unwrap()
                .into_iter()
                .filter(|m| m.event_notice.is_some()),
        );
    }
    assert_eq!(
        notices.len(),
        1,
        "one notice per run, and none before a run starts"
    );
    assert_eq!(notices[0].content, "Movie night");
    assert_eq!(notices[0].author.pubkey, owner.pubkey);
}
