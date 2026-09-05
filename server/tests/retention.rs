//! Retention deletes on a timer and reports a count nobody reads, so the two
//! ways it can quietly stop working — the wrong side of the cutoff, and the
//! wrong guild — are the ones worth pinning down.

use std::path::PathBuf;

use dioxusfun_server::protocol::{Channel, ChannelKind, Guild, GuildVisibility, Id, Message, User};
use dioxusfun_server::store::Store;

fn temp_db() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-retention-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir.join("store.sqlite")
}

fn guild(name: &str) -> Guild {
    Guild {
        id: Id::new_v4(),
        name: name.into(),
        icon: None,
        owner_pubkey: "00".repeat(32),
        accent: None,
        visibility: GuildVisibility::Public,
        description: None,
        icon_image: None,
        banner: None,
        retention_days: None,
        join_gate: Default::default(),
        rules: None,
        panic_mode: false,
        leveling: Default::default(),
    }
}

fn channel(guild_id: Id) -> Channel {
    Channel {
        id: Id::new_v4(),
        guild_id,
        name: "general".into(),
        kind: ChannelKind::Text,
        topic: None,
        read_only: false,
        slowmode_secs: 0,
        position: 0,
    }
}

fn message(channel_id: Id, at_ms: i64, body: &str) -> Message {
    Message {
        id: Id::new_v4(),
        channel_id,
        author: User {
            pubkey: "11".repeat(32),
            username: "alice".into(),
        },
        content: body.into(),
        image: None,
        reactions: Vec::new(),
        reply_to: None,
        created_at: chrono::DateTime::from_timestamp_millis(at_ms).expect("a real timestamp"),
    }
}

async fn contents(store: &Store, channel_id: Id) -> Vec<String> {
    let mut got: Vec<String> = store
        .history(channel_id, u32::MAX, None)
        .await
        .expect("history")
        .into_iter()
        .map(|m| m.content)
        .collect();
    got.sort();
    got
}

/// The cutoff is in milliseconds and the column is too. A unit mismatch here
/// deletes everything or nothing, and both look like a working sweep from the
/// outside: the count it returns goes to a log line.
#[tokio::test]
async fn the_sweep_takes_what_is_older_than_the_cutoff_and_nothing_else() {
    let store = Store::open(&temp_db()).await.expect("open");
    let g = guild("keep");
    store.upsert_guild(&g).await.expect("guild");
    let ch = channel(g.id);
    store.upsert_channel(&ch).await.expect("channel");

    let cutoff = 1_700_000_000_000i64;
    for (at, body) in [
        (cutoff - 1_000, "older"),
        (cutoff, "exactly at the cutoff"),
        (cutoff + 1_000, "newer"),
    ] {
        store
            .insert_message(&message(ch.id, at, body))
            .await
            .expect("insert");
    }

    let deleted = store
        .sweep_guild_messages(g.id, cutoff)
        .await
        .expect("sweep");

    assert_eq!(deleted, 1, "only the one strictly older should go");
    assert_eq!(
        contents(&store, ch.id).await,
        vec!["exactly at the cutoff".to_string(), "newer".to_string()],
        "the message on the cutoff is kept, so the boundary is not off by one"
    );
}

/// The delete reaches through a subquery on `channels.guild_id`. Get that wrong
/// and a guild with retention set starts eating its neighbours' history, which
/// nothing in the app would report.
#[tokio::test]
async fn a_sweep_stops_at_the_guild_it_was_asked_about() {
    let store = Store::open(&temp_db()).await.expect("open");
    let mine = guild("mine");
    let theirs = guild("theirs");
    store.upsert_guild(&mine).await.expect("guild");
    store.upsert_guild(&theirs).await.expect("guild");
    let my_ch = channel(mine.id);
    let their_ch = channel(theirs.id);
    store.upsert_channel(&my_ch).await.expect("channel");
    store.upsert_channel(&their_ch).await.expect("channel");

    let cutoff = 1_700_000_000_000i64;
    store
        .insert_message(&message(my_ch.id, cutoff - 1, "mine, old"))
        .await
        .expect("insert");
    store
        .insert_message(&message(their_ch.id, cutoff - 1, "theirs, old"))
        .await
        .expect("insert");

    let deleted = store
        .sweep_guild_messages(mine.id, cutoff)
        .await
        .expect("sweep");

    assert_eq!(deleted, 1);
    assert!(contents(&store, my_ch.id).await.is_empty());
    assert_eq!(
        contents(&store, their_ch.id).await,
        vec!["theirs, old".to_string()],
        "the other guild's history is not this sweep's to take"
    );
}

/// A guild with no retention set is never handed to the sweep at all, so the
/// interesting case is that asking for one still deletes nothing surprising.
#[tokio::test]
async fn a_cutoff_before_everything_deletes_nothing() {
    let store = Store::open(&temp_db()).await.expect("open");
    let g = guild("young");
    store.upsert_guild(&g).await.expect("guild");
    let ch = channel(g.id);
    store.upsert_channel(&ch).await.expect("channel");
    store
        .insert_message(&message(ch.id, 1_700_000_000_000, "recent"))
        .await
        .expect("insert");

    let deleted = store.sweep_guild_messages(g.id, 0).await.expect("sweep");

    assert_eq!(deleted, 0);
    assert_eq!(contents(&store, ch.id).await, vec!["recent".to_string()]);
}
