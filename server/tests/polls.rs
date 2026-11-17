//! Polls: one vote per person, anonymous ones name nobody, and only their
//! author or a moderator ends them early.

use std::net::SocketAddr;
use std::time::Duration;

use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{ChannelKind, ClientMessage, Id, Message, Poll, ServerMessage};

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

async fn spawn_gateway() -> (String, discordia_server::ServerHandle) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "discordia-polls-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
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

struct Person {
    bot: Bot,
    key: String,
}

async fn connect(url: &str, name: &str) -> Person {
    let identity = BotIdentity::generate();
    let mut bot = Bot::connect_as_user(url, &identity, name).await.unwrap();
    next_matching(&mut bot, |m| {
        matches!(m, ServerMessage::Ready { polls: true, .. }).then_some(())
    })
    .await;
    Person {
        bot,
        key: identity.pubkey().to_string(),
    }
}

/// A guild with one text channel, and a second member in it.
async fn setup(url: &str) -> (Person, Person, Id) {
    let mut owner = connect(url, "Owner").await;
    owner
        .bot
        .send(&ClientMessage::CreateGuild {
            name: "Polls".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild_id, channel) = next_matching(&mut owner.bot, |m| match m {
        ServerMessage::GuildJoined {
            guild, channels, ..
        } => channels
            .into_iter()
            .find(|c| c.kind == ChannelKind::Text)
            .map(|c| (guild.id, c.id)),
        _ => None,
    })
    .await;
    let mut member = connect(url, "Member").await;
    member
        .bot
        .send(&ClientMessage::JoinGuild {
            guild_id,
            accept: true,
            pow_nonce: None,
        })
        .await
        .unwrap();
    next_matching(&mut member.bot, |m| {
        matches!(m, ServerMessage::GuildJoined { .. }).then_some(())
    })
    .await;
    (owner, member, channel)
}

async fn create(
    author: &mut Bot,
    channel_id: Id,
    anonymous: bool,
    multiple: bool,
    changeable: bool,
) {
    author
        .send(&ClientMessage::CreatePoll {
            channel_id,
            question: "  Lunch?  ".into(),
            options: vec!["Pizza".into(), "Tacos".into(), "Soup".into()],
            anonymous,
            multiple,
            changeable,
            duration_secs: Some(3600),
        })
        .await
        .unwrap();
}

async fn next_poll_message(session: &mut Bot) -> Message {
    next_matching(session, |m| match m {
        ServerMessage::MessageCreate(m) if m.poll.is_some() => Some(m),
        _ => None,
    })
    .await
}

struct Update {
    counts: Vec<u32>,
    voters: Vec<Vec<String>>,
    closed: bool,
    mine: Option<Vec<u8>>,
}

async fn next_update(session: &mut Bot) -> Update {
    next_matching(session, |m| match m {
        ServerMessage::PollUpdate {
            counts,
            voters,
            closed_at,
            mine,
            ..
        } => Some(Update {
            counts,
            voters,
            closed: closed_at.is_some(),
            mine,
        }),
        _ => None,
    })
    .await
}

async fn next_error(session: &mut Bot) -> String {
    next_matching(session, |m| match m {
        ServerMessage::Error { message } => Some(message),
        _ => None,
    })
    .await
}

async fn vote(session: &mut Bot, channel_id: Id, message_id: Id, choices: Vec<u8>) {
    session
        .send(&ClientMessage::VotePoll {
            channel_id,
            message_id,
            choices,
        })
        .await
        .unwrap();
}

async fn history_poll(session: &mut Bot, channel_id: Id) -> (Poll, String) {
    session.fetch_messages(channel_id, 50, None).await.unwrap();
    next_matching(session, |m| match m {
        ServerMessage::MessageHistory { messages, .. } => {
            let raw = serde_json::to_string(&messages).unwrap();
            messages
                .into_iter()
                .find_map(|m| m.poll)
                .map(|poll| (poll, raw))
        }
        _ => None,
    })
    .await
}

#[tokio::test]
async fn an_anonymous_poll_counts_votes_and_tells_only_the_voter_their_choice() {
    let (url, _server) = spawn_gateway().await;
    let (mut owner, mut member, channel) = setup(&url).await;
    create(&mut owner.bot, channel, true, false, false).await;
    let posted = next_poll_message(&mut owner.bot).await;
    assert_eq!(posted.content, "Lunch?", "the question is the message text");
    let message_id = next_poll_message(&mut member.bot).await.id;
    assert_eq!(posted.id, message_id);

    vote(&mut member.bot, channel, message_id, vec![1]).await;
    let own = next_update(&mut member.bot).await;
    assert_eq!(own.mine, Some(vec![1]));
    assert_eq!(own.counts, vec![0, 1, 0]);
    let seen = next_update(&mut owner.bot).await;
    assert_eq!(seen.counts, vec![0, 1, 0]);
    assert_eq!(seen.mine, None);
    assert!(seen.voters.is_empty(), "an anonymous poll names nobody");

    let (poll, raw) = history_poll(&mut owner.bot, channel).await;
    assert_eq!(poll.counts, vec![0, 1, 0]);
    assert!(poll.mine.is_empty() && poll.voters.is_empty());
    assert!(!raw.contains(&member.key), "history leaks the voter: {raw}");
    let (poll, _) = history_poll(&mut member.bot, channel).await;
    assert_eq!(poll.mine, vec![1], "history shows a voter their own choice");

    vote(&mut member.bot, channel, message_id, vec![0]).await;
    assert!(next_error(&mut member.bot).await.contains("changing"));
}

#[tokio::test]
async fn a_public_poll_names_voters_and_a_changeable_vote_can_move_or_be_withdrawn() {
    let (url, _server) = spawn_gateway().await;
    let (mut owner, mut member, channel) = setup(&url).await;
    create(&mut owner.bot, channel, false, true, true).await;
    let message_id = next_poll_message(&mut member.bot).await.id;
    next_poll_message(&mut owner.bot).await;

    vote(&mut member.bot, channel, message_id, vec![2, 0, 2]).await;
    assert_eq!(next_update(&mut member.bot).await.mine, Some(vec![0, 2]));
    let seen = next_update(&mut owner.bot).await;
    assert_eq!(seen.counts, vec![1, 0, 1]);
    assert_eq!(seen.voters[0], vec![member.key.clone()]);

    vote(&mut member.bot, channel, message_id, vec![]).await;
    assert_eq!(next_update(&mut member.bot).await.mine, Some(vec![]));
    assert_eq!(next_update(&mut owner.bot).await.counts, vec![0, 0, 0]);
}

#[tokio::test]
async fn only_the_author_or_a_moderator_closes_a_poll_and_a_closed_poll_takes_no_votes() {
    let (url, _server) = spawn_gateway().await;
    let (mut owner, mut member, channel) = setup(&url).await;
    create(&mut member.bot, channel, false, false, true).await;
    let message_id = next_poll_message(&mut member.bot).await.id;
    next_poll_message(&mut owner.bot).await;

    create(&mut owner.bot, channel, false, false, true).await;
    let owners_poll = next_poll_message(&mut owner.bot).await.id;
    next_poll_message(&mut member.bot).await;
    member
        .bot
        .send(&ClientMessage::ClosePoll {
            channel_id: channel,
            message_id: owners_poll,
        })
        .await
        .unwrap();
    assert!(
        next_error(&mut member.bot)
            .await
            .contains("manage_messages")
    );

    owner
        .bot
        .send(&ClientMessage::ClosePoll {
            channel_id: channel,
            message_id,
        })
        .await
        .unwrap();
    assert!(
        next_update(&mut member.bot).await.closed,
        "the owner moderates"
    );
    vote(&mut member.bot, channel, message_id, vec![0]).await;
    assert!(next_error(&mut member.bot).await.contains("closed"));
}

#[tokio::test]
async fn a_poll_is_validated_and_never_edited() {
    let (url, _server) = spawn_gateway().await;
    let (mut owner, _member, channel) = setup(&url).await;
    owner
        .bot
        .send(&ClientMessage::CreatePoll {
            channel_id: channel,
            question: "One?".into(),
            options: vec!["Only".into()],
            anonymous: false,
            multiple: false,
            changeable: false,
            duration_secs: None,
        })
        .await
        .unwrap();
    assert!(next_error(&mut owner.bot).await.contains("answers"));

    create(&mut owner.bot, channel, false, false, false).await;
    let message_id = next_poll_message(&mut owner.bot).await.id;
    owner
        .bot
        .edit_message(channel, message_id, "Dinner?")
        .await
        .unwrap();
    let (_, raw) = history_poll(&mut owner.bot, channel).await;
    assert!(raw.contains("Lunch?") && !raw.contains("Dinner?"), "{raw}");
}
