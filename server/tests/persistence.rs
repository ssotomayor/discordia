//! Phase-1 persistence: state survives a full server restart, and message
//! images are offloaded to the content-addressed blob store.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ChannelKind, ClientMessage, Id, ServerMessage};

async fn next_timeout(session: &mut Bot) -> ServerMessage {
    tokio::time::timeout(Duration::from_secs(5), session.next_event())
        .await
        .expect("timed out waiting for a gateway event")
        .expect("connection closed unexpectedly")
}

fn temp_data_dir() -> PathBuf {
    // The counter is what actually guarantees uniqueness. A timestamp alone is
    // not enough: macOS resolves `SystemTime::now()` to about a microsecond, so
    // two tests in this binary starting together can read the same value, share
    // a data directory, and then fail on each other's files — one test's
    // cleanup removing the media directory the other is asserting on.
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "dioxusfun-persist-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

async fn spawn_on(dir: &Path) -> (String, dioxusfun_server::ServerHandle) {
    // Port 0 = let the OS pick. A fixed port made the two tests in this file
    // race each other under `cargo test --workspace`: one binds it, and a
    // client can end up talking to the other test's server on the same port,
    // failing an assertion for reasons that have nothing to do with the code
    // under test. The handle reports the real address, so nothing else cares.
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

/// A 1x1 red PNG, base64 (67 bytes decoded).
const TINY_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

#[tokio::test]
async fn state_survives_restart_and_media_is_offloaded() {
    let dir = temp_data_dir();

    // ---- boot #1: create a guild, post text + an image message -------------
    let owner_id = BotIdentity::generate();
    let (guild_id, text_channel): (Id, Id);
    {
        let (url, handle) = spawn_on(&dir).await;
        let mut owner = Bot::connect_as_user(&url, &owner_id, "Owner")
            .await
            .unwrap();
        loop {
            if matches!(next_timeout(&mut owner).await, ServerMessage::Ready { .. }) {
                break;
            }
        }
        owner
            .send(&ClientMessage::CreateGuild {
                name: "Persistent".into(),
                template: None,
            })
            .await
            .unwrap();
        (guild_id, text_channel) = loop {
            if let ServerMessage::GuildJoined {
                guild, channels, ..
            } = next_timeout(&mut owner).await
            {
                let text = channels
                    .iter()
                    .find(|c| c.kind == ChannelKind::Text)
                    .unwrap()
                    .id;
                break (guild.id, text);
            }
        };
        owner
            .send_message(text_channel, "survives restarts")
            .await
            .unwrap();
        owner
            .send(&ClientMessage::SendMessage {
                channel_id: text_channel,
                content: "with an image".into(),
                image: Some(TINY_PNG.into()),
                reply_to: None,
            })
            .await
            .unwrap();
        // Wait until both messages echo back (persisted before delivery).
        let mut seen = 0;
        while seen < 2 {
            if let ServerMessage::MessageCreate(_) = next_timeout(&mut owner).await {
                seen += 1;
            }
        }
        handle.abort();
    }

    // The blob store must hold the offloaded image (content-addressed file),
    // and the DB file must exist.
    let blobs: Vec<_> = std::fs::read_dir(dir.join("media"))
        .expect("media dir exists")
        .filter_map(|e| e.ok())
        .collect();
    assert!(!blobs.is_empty(), "image was offloaded to the blob store");
    assert!(dir.join("discordia.db").exists(), "sqlite file exists");

    // ---- boot #2: same data dir — everything must still be there -----------
    let (url, handle) = spawn_on(&dir).await;
    let mut owner = Bot::connect_as_user(&url, &owner_id, "Owner")
        .await
        .unwrap();
    let guilds = loop {
        if let ServerMessage::Ready { guilds, .. } = next_timeout(&mut owner).await {
            break guilds;
        }
    };
    assert!(
        guilds.iter().any(|g| g.id == guild_id),
        "owned guild survived the restart (membership rehydrated)"
    );

    owner
        .send(&ClientMessage::FetchMessages {
            channel_id: text_channel,
            limit: 50,
            before_ms: None,
        })
        .await
        .unwrap();
    let history = loop {
        if let ServerMessage::MessageHistory {
            channel_id,
            messages,
        } = next_timeout(&mut owner).await
            && channel_id == text_channel
        {
            break messages;
        }
    };
    assert_eq!(history.len(), 2, "both messages survived");
    assert_eq!(history[0].content, "survives restarts");
    assert_eq!(history[1].content, "with an image");
    // The image round-trips: stored as a blob, inlined back as a data URL.
    let img = history[1].image.as_deref().expect("image survived");
    assert!(
        img.starts_with("data:image/png;base64,"),
        "inlined on serve"
    );

    handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Replies: the server builds the quote from its own row, scopes the lookup to
/// the channel, and the snapshot survives a restart.
///
/// The cross-channel half is the security-relevant one — a reply is the one
/// message shape that can pull another message's *text* along with it, so an id
/// from a channel the sender can't read must not resolve.
#[tokio::test]
async fn replies_are_quoted_server_side_and_survive_restart() {
    let dir = temp_data_dir();
    let author_id = BotIdentity::generate();
    let replier_id = BotIdentity::generate();

    let (guild_id, chan_a, chan_b, parent_id);
    {
        let (url, handle) = spawn_on(&dir).await;
        let mut author = Bot::connect_as_user(&url, &author_id, "Author")
            .await
            .unwrap();
        author
            .send(&ClientMessage::CreateGuild {
                name: "Replies".into(),
                template: None,
            })
            .await
            .unwrap();
        (guild_id, chan_a) = loop {
            if let ServerMessage::GuildJoined {
                guild, channels, ..
            } = next_timeout(&mut author).await
            {
                let text = channels
                    .iter()
                    .find(|c| c.kind == ChannelKind::Text)
                    .unwrap()
                    .id;
                break (guild.id, text);
            }
        };
        // A second text channel, to prove the lookup is channel-scoped.
        author
            .send(&ClientMessage::CreateChannel {
                guild_id,
                name: "other".into(),
                kind: ChannelKind::Text,
                topic: None,
            })
            .await
            .unwrap();
        chan_b = loop {
            if let ServerMessage::ChannelCreate(c) = next_timeout(&mut author).await
                && c.id != chan_a
            {
                break c.id;
            }
        };

        author
            .send_message(chan_a, "the message being answered")
            .await
            .unwrap();
        parent_id = loop {
            if let ServerMessage::MessageCreate(m) = next_timeout(&mut author).await
                && m.channel_id == chan_a
            {
                break m.id;
            }
        };

        // A reply in the same channel gets a quote built from the parent's row.
        let mut replier = Bot::connect_as_user(&url, &replier_id, "Replier")
            .await
            .unwrap();
        replier
            .send(&ClientMessage::JoinGuild {
                guild_id,
                accept: true,
                pow_nonce: None,
            })
            .await
            .unwrap();
        loop {
            if let ServerMessage::GuildJoined { .. } = next_timeout(&mut replier).await {
                break;
            }
        }
        replier
            .reply_message(chan_a, "answering you", parent_id)
            .await
            .unwrap();
        let reply = loop {
            if let ServerMessage::MessageCreate(m) = next_timeout(&mut replier).await
                && m.content == "answering you"
            {
                break m;
            }
        };
        let q = reply.reply_to.expect("reply carries a quote");
        assert_eq!(q.message_id, parent_id);
        assert_eq!(
            q.author_pubkey,
            author_id.pubkey(),
            "quote is attributed to the parent's author"
        );
        assert_eq!(
            q.excerpt, "the message being answered",
            "excerpt comes from the server's row, not the client"
        );

        // The same id from a different channel must not resolve: no quote, but
        // the message still sends.
        replier
            .reply_message(chan_b, "wrong channel", parent_id)
            .await
            .unwrap();
        let stray = loop {
            if let ServerMessage::MessageCreate(m) = next_timeout(&mut replier).await
                && m.content == "wrong channel"
            {
                break m;
            }
        };
        assert!(
            stray.reply_to.is_none(),
            "an id from another channel must not be quotable"
        );

        handle.abort();
    }

    // ---- restart: the snapshot is persisted, not recomputed ----------------
    let (url, handle) = spawn_on(&dir).await;
    let mut author = Bot::connect_as_user(&url, &author_id, "Author")
        .await
        .unwrap();
    loop {
        if let ServerMessage::Ready { .. } = next_timeout(&mut author).await {
            break;
        }
    }
    author
        .send(&ClientMessage::FetchMessages {
            channel_id: chan_a,
            limit: 50,
            before_ms: None,
        })
        .await
        .unwrap();
    let history = loop {
        if let ServerMessage::MessageHistory { messages, .. } = next_timeout(&mut author).await {
            break messages;
        }
    };
    let reply = history
        .iter()
        .find(|m| m.content == "answering you")
        .expect("reply survived the restart");
    let q = reply.reply_to.as_ref().expect("quote survived the restart");
    assert_eq!(q.message_id, parent_id);
    assert_eq!(q.excerpt, "the message being answered");
    handle.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The blob sweep is only as safe as the query that says what is still in use:
/// a table missing from `referenced_media` means the sweep deletes pictures
/// people are still looking at. Both producers are exercised here — a message
/// image (stored under the `media:` sentinel) and a guild emoji (stored as the
/// bare filename) — because they are recorded in *different shapes*, which is
/// exactly how one of them gets forgotten.
#[tokio::test]
async fn every_kind_of_blob_reference_is_found() {
    let dir = temp_data_dir();
    let (url, handle) = spawn_on(&dir).await;

    let owner = BotIdentity::generate();
    let mut session = Bot::connect_as_user(&url, &owner, "Owner").await.unwrap();
    loop {
        if matches!(
            next_timeout(&mut session).await,
            ServerMessage::Ready { .. }
        ) {
            break;
        }
    }
    session
        .send(&ClientMessage::CreateGuild {
            name: "Blobs".into(),
            template: None,
        })
        .await
        .unwrap();
    let (guild_id, channel_id) = loop {
        if let ServerMessage::GuildJoined {
            guild, channels, ..
        } = next_timeout(&mut session).await
        {
            let text = channels
                .iter()
                .find(|c| c.kind == ChannelKind::Text)
                .expect("text channel")
                .id;
            break (guild.id, text);
        }
    };

    session
        .send(&ClientMessage::SendMessage {
            channel_id,
            content: "picture".into(),
            image: Some(TINY_PNG.into()),
            reply_to: None,
        })
        .await
        .unwrap();
    // A *different* picture on purpose. Blobs are content-addressed, so
    // reusing TINY_PNG here would give the emoji the same file the message
    // already referenced — and the test would pass with the emoji table
    // missing from the query entirely. It did, until this line changed.
    const OTHER_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    session
        .send(&ClientMessage::CreateGuildEmoji {
            guild_id,
            shortcode: "party".into(),
            image: OTHER_PNG.into(),
        })
        .await
        .unwrap();

    // Let both writes land.
    tokio::time::sleep(Duration::from_millis(400)).await;

    let store = dioxusfun_server::store::Store::open(&dir.join("discordia.db"))
        .await
        .expect("open the store the server just wrote");
    let referenced = store.referenced_media().await.expect("scan references");

    let on_disk: Vec<String> = std::fs::read_dir(dir.join("media"))
        .expect("media dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();

    assert!(!on_disk.is_empty(), "something was written");
    for name in &on_disk {
        assert!(
            referenced.contains(name),
            "blob {name} is on disk and unreferenced — the sweep would delete it.\n\
             referenced: {referenced:?}"
        );
    }

    handle.abort();
}

/// Two redemptions of the same code must both be counted, in either order.
///
/// The in-memory cap is enforced under the entry lock, but the lock is released
/// before the write is awaited — so two joins can reach the store carrying
/// counts 4 and 5 and land in either order across the pool's connections. An
/// absolute `SET uses = <snapshot>` leaves the row at whichever wrote last, and
/// `load_or_seed` rehydrates `uses` from that column, so a restart would hand
/// the code back a use it had already spent. The write is relative for exactly
/// this reason, and relative writes commute.
#[tokio::test]
async fn concurrent_redemptions_cannot_lose_each_other() {
    let dir = temp_data_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let store = dioxusfun_server::store::Store::open(&dir.join("discordia.db"))
        .await
        .expect("open store");

    let guild = Id::new_v4();
    store
        .set_invite(guild, "code12345678", None, Some(5), "owner")
        .await
        .expect("mint");

    // Both in flight at once, which is the case the absolute write lost.
    let (a, b) = tokio::join!(
        store.bump_invite_uses("code12345678"),
        store.bump_invite_uses("code12345678"),
    );
    a.expect("first bump");
    b.expect("second bump");

    let loaded = store.load_all().await.expect("reload");
    let invite = loaded
        .invites
        .iter()
        .find(|i| i.code == "code12345678")
        .expect("the invite survived");
    assert_eq!(
        invite.uses, 2,
        "both redemptions must be counted — a lost one hands the code back a use"
    );
    assert_eq!(invite.max_uses, Some(5), "the cap round-trips");
}
