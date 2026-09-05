//! The limits the gateway puts on what a client may send. They are one-line
//! conditions in the middle of long match arms, which is exactly the shape that
//! gets edited past without anyone noticing the guard went with it.
//!
//! Usernames are canonicalised in `protocol` and tested beside the function.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::{ChannelKind, ClientMessage, Id, Message, ServerMessage, User};
use dioxusfun_server::state::MAX_IMAGE_LEN;
use dioxusfun_server::store::Store;

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
        "dioxusfun-validation-{}-{}-{n}",
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
        livekit: LiveKitConfig::from_env(dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir.to_path_buf(),
    };
    let handle = dioxusfun_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn");
    (format!("ws://{}", handle.addr), handle)
}

async fn spawn_gateway() -> (String, dioxusfun_server::ServerHandle) {
    spawn_on(&temp_data_dir()).await
}

async fn ready(url: &str, identity: &BotIdentity, name: &str) -> Bot {
    let mut bot = Bot::connect_as_user(url, identity, name).await.unwrap();
    loop {
        if matches!(next_timeout(&mut bot).await, ServerMessage::Ready { .. }) {
            return bot;
        }
    }
}

async fn owner_with_channel(url: &str, identity: &BotIdentity) -> (Bot, Id) {
    let mut bot = ready(url, identity, "Owner").await;
    bot.send(&ClientMessage::CreateGuild {
        name: "Checks".into(),
        template: None,
    })
    .await
    .unwrap();
    let channel = loop {
        if let ServerMessage::GuildJoined { channels, .. } = next_timeout(&mut bot).await {
            break channels
                .iter()
                .find(|c| c.kind == ChannelKind::Text)
                .expect("a text channel")
                .id;
        }
    };
    (bot, channel)
}

const TINY_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// Sends an image and says what came back: the message, or the refusal.
async fn send_image(bot: &mut Bot, channel_id: Id, image: &str) -> Result<(), String> {
    bot.send(&ClientMessage::SendMessage {
        channel_id,
        content: "look".into(),
        image: Some(image.to_string()),
        reply_to: None,
    })
    .await
    .unwrap();
    loop {
        match next_timeout(bot).await {
            ServerMessage::MessageCreate(_) => return Ok(()),
            ServerMessage::Error { message } => return Err(message),
            _ => continue,
        }
    }
}

/// Anything that is not a data URL is a fetch the server would be talked into
/// making, and the sentinel written to disk is derived from what arrives here.
#[tokio::test]
async fn an_image_that_is_not_a_data_url_is_refused() {
    let (url, _h) = spawn_gateway().await;
    let (mut owner, channel) = owner_with_channel(&url, &BotIdentity::generate()).await;

    for bogus in [
        "https://example.invalid/cat.png",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "data:text/html;base64,PHNjcmlwdD4=",
        "",
    ] {
        let refused = send_image(&mut owner, channel, bogus)
            .await
            .expect_err(&format!("accepted {bogus:?}"));
        assert!(
            refused.contains("data:image"),
            "refused {bogus:?} for the wrong reason: {refused}"
        );
    }

    send_image(&mut owner, channel, TINY_PNG)
        .await
        .expect("a real data URL still goes through");
}

/// The cap is on the encoded string, and it is checked before anything decodes
/// or writes, so this is the only thing between a socket and the disk.
#[tokio::test]
async fn an_image_over_the_size_limit_is_refused() {
    let (url, _h) = spawn_gateway().await;
    let (mut owner, channel) = owner_with_channel(&url, &BotIdentity::generate()).await;

    let huge = format!("data:image/png;base64,{}", "A".repeat(MAX_IMAGE_LEN));
    assert!(huge.len() > MAX_IMAGE_LEN, "precondition");
    let refused = send_image(&mut owner, channel, &huge)
        .await
        .expect_err("an oversize image was accepted");
    assert!(
        refused.contains("size limit"),
        "refused for the wrong reason: {refused}"
    );
}

/// History is paged and the page size arrives from the client. Unclamped it is
/// a way to ask one socket to pull a whole channel out of SQLite.
///
/// The messages go in through the store rather than the gateway: filling a
/// channel past the clamp would take 200 sends, and the write limiter is thirty
/// every ten seconds. A test that only sends a dozen cannot fail whether the
/// clamp is there or not, which is worse than not testing it.
#[tokio::test]
async fn a_history_page_is_clamped_however_much_is_asked_for() {
    let dir = temp_data_dir();
    let identity = BotIdentity::generate();

    let channel = {
        let (url, handle) = spawn_on(&dir).await;
        let (_owner, channel) = owner_with_channel(&url, &identity).await;
        handle.abort();
        channel
    };

    {
        let store = Store::open(&dir.join("discordia.db")).await.expect("store");
        for i in 0..250 {
            store
                .insert_message(&Message {
                    id: Id::new_v4(),
                    channel_id: channel,
                    author: User {
                        pubkey: identity.pubkey().to_string(),
                        username: "Owner".into(),
                    },
                    content: format!("message {i}"),
                    image: None,
                    reactions: Vec::new(),
                    reply_to: None,
                    created_at: chrono::DateTime::from_timestamp_millis(
                        1_700_000_000_000 + i as i64,
                    )
                    .expect("a real timestamp"),
                })
                .await
                .expect("insert");
        }
    }

    let (url, _h) = spawn_on(&dir).await;
    let mut owner = ready(&url, &identity, "Owner").await;
    owner
        .send(&ClientMessage::FetchMessages {
            channel_id: channel,
            limit: u32::MAX,
            before_ms: None,
        })
        .await
        .unwrap();
    let page = loop {
        if let ServerMessage::MessageHistory { messages, .. } = next_timeout(&mut owner).await {
            break messages;
        }
    };
    assert!(
        page.len() <= 200,
        "a page of {} came back for a limit of u32::MAX",
        page.len()
    );
    assert!(
        page.len() > 12,
        "precondition: the channel is past the clamp"
    );
}

/// The unit tests pin `protocol`'s filters; this one pins that the gateway
/// actually calls them, which is the half a refactor drops.
#[tokio::test]
async fn names_and_free_text_come_back_clean_over_the_wire() {
    let (url, _h) = spawn_gateway().await;
    let mut owner = ready(&url, &BotIdentity::generate(), "Owner").await;

    owner
        .send(&ClientMessage::CreateGuild {
            name: "Ac\u{202E}me\nINFO forged".into(),
            template: None,
        })
        .await
        .unwrap();
    let guild_id = loop {
        if let ServerMessage::GuildJoined { guild, .. } = next_timeout(&mut owner).await {
            assert_eq!(
                guild.name, "AcmeINFO forged",
                "the guild name reached storage unfiltered"
            );
            break guild.id;
        }
    };

    owner
        .send(&ClientMessage::CreateChannel {
            guild_id,
            name: "gen\u{0}eral".into(),
            kind: ChannelKind::Text,
            topic: Some("what\u{2069}ever".into()),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ChannelCreate(channel) = next_timeout(&mut owner).await {
            assert_eq!(channel.name, "general");
            assert_eq!(channel.topic.as_deref(), Some("whatever"));
            break;
        }
    }

    owner
        .send(&ClientMessage::SetProfile {
            avatar: None,
            banner: None,
            bio: Some("line one\r\nli\u{202E}ne two".into()),
            status: Some("onl\nine".into()),
            custom_status: Some("busy\u{2028}now".into()),
        })
        .await
        .unwrap();
    loop {
        if let ServerMessage::ProfileUpdate(profile) = next_timeout(&mut owner).await {
            assert_eq!(
                profile.bio.as_deref(),
                Some("line one\nline two"),
                "a bio keeps the break the person typed and loses the rest"
            );
            assert_eq!(profile.status.as_deref(), Some("online"));
            assert_eq!(profile.custom_status.as_deref(), Some("busynow"));
            break;
        }
    }
}

/// A name that is nothing but junk is empty once filtered, and an empty name
/// is a rejection rather than a guild called "".
#[tokio::test]
async fn a_name_of_pure_junk_is_refused_not_stored_blank() {
    let (url, _h) = spawn_gateway().await;
    let mut owner = ready(&url, &BotIdentity::generate(), "Owner").await;

    owner
        .send(&ClientMessage::CreateGuild {
            name: "\u{202E}\u{0}\n".into(),
            template: None,
        })
        .await
        .unwrap();
    loop {
        match next_timeout(&mut owner).await {
            ServerMessage::Error { message } => {
                assert!(message.contains("guild name"), "wrong refusal: {message}");
                break;
            }
            ServerMessage::GuildJoined { guild, .. } => {
                panic!("stored a guild named {:?}", guild.name)
            }
            _ => continue,
        }
    }
}

/// The label is checked against the bytes, so a PNG signature cannot be parked
/// on disk under `.jpg`, and neither can a web page under any picture name.
#[tokio::test]
async fn an_image_whose_bytes_do_not_match_its_label_is_refused() {
    let (url, _h) = spawn_gateway().await;
    let (mut owner, channel) = owner_with_channel(&url, &BotIdentity::generate()).await;

    let png_payload = TINY_PNG.split_once(";base64,").unwrap().1;
    let mislabeled = format!("data:image/jpeg;base64,{png_payload}");
    let refused = send_image(&mut owner, channel, &mislabeled)
        .await
        .expect_err("PNG bytes were accepted as a JPEG");
    assert!(
        refused.contains("unsupported image format"),
        "refused for the wrong reason: {refused}"
    );

    let refused = send_image(&mut owner, channel, "data:image/svg+xml;base64,PHN2Zz4=")
        .await
        .expect_err("an SVG was accepted");
    assert!(refused.contains("unsupported image format"), "{refused}");
}

/// A link would make every member's webview fetch it, handing their addresses
/// to whoever set it; the server stores pictures itself now, so it takes none.
#[tokio::test]
async fn a_profile_picture_may_not_be_a_link() {
    let (url, _h) = spawn_gateway().await;
    let mut bot = ready(&url, &BotIdentity::generate(), "Linky").await;

    bot.send(&ClientMessage::SetProfile {
        avatar: Some("https://example.invalid/me.png".into()),
        banner: None,
        bio: None,
        status: None,
        custom_status: None,
    })
    .await
    .unwrap();
    let refused = loop {
        match next_timeout(&mut bot).await {
            ServerMessage::Error { message } => break message,
            ServerMessage::ProfileUpdate(_) => panic!("a link was accepted as an avatar"),
            _ => continue,
        }
    };
    assert!(refused.contains("not a link"), "{refused}");

    bot.send(&ClientMessage::SetProfile {
        avatar: Some(TINY_PNG.into()),
        banner: None,
        bio: None,
        status: None,
        custom_status: None,
    })
    .await
    .unwrap();
    let stored = loop {
        if let ServerMessage::ProfileUpdate(p) = next_timeout(&mut bot).await {
            break p.avatar.expect("avatar kept");
        }
    };
    assert!(
        stored.starts_with("media:"),
        "a real picture is stored and addressed: {stored}"
    );
}

/// Guilds are persisted and held in memory for good, so one key gets a fixed
/// number of them; the refusal has to arrive as an error, not a silent drop.
#[tokio::test]
async fn one_key_cannot_own_more_than_its_share_of_guilds() {
    use dioxusfun_server::state::MAX_GUILDS_PER_OWNER;

    let (url, _h) = spawn_gateway().await;
    let mut bot = ready(&url, &BotIdentity::generate(), "Founder").await;

    for i in 0..MAX_GUILDS_PER_OWNER {
        bot.send(&ClientMessage::CreateGuild {
            name: format!("Guild {i}"),
            template: None,
        })
        .await
        .unwrap();
        loop {
            match next_timeout(&mut bot).await {
                ServerMessage::GuildJoined { .. } => break,
                ServerMessage::Error { message } => panic!("guild {i} refused: {message}"),
                _ => continue,
            }
        }
        // The write limiter admits 30 per window; stay well under it.
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    bot.send(&ClientMessage::CreateGuild {
        name: "One too many".into(),
        template: None,
    })
    .await
    .unwrap();
    let refused = loop {
        match next_timeout(&mut bot).await {
            ServerMessage::Error { message } => break message,
            ServerMessage::GuildJoined { .. } => panic!("the guild past the cap was created"),
            _ => continue,
        }
    };
    assert!(refused.contains("already own"), "{refused}");
}

/// Three buttons pick a presence; the wire would take any string, and a
/// presence nothing can draw is a blank badge on every member list.
fn presence(status: &str) -> ClientMessage {
    ClientMessage::SetProfile {
        avatar: None,
        banner: None,
        bio: None,
        status: Some(status.to_string()),
        custom_status: None,
    }
}

#[tokio::test]
async fn a_presence_outside_the_set_is_refused() {
    let (url, _h) = spawn_gateway().await;
    let mut bot = ready(&url, &BotIdentity::generate(), "Moody").await;

    bot.send(&presence("invisible")).await.unwrap();
    let refused = loop {
        match next_timeout(&mut bot).await {
            ServerMessage::Error { message } => break message,
            ServerMessage::ProfileUpdate(_) => panic!("an unknown presence was stored"),
            _ => continue,
        }
    };
    assert!(refused.contains("status must be one of"), "{refused}");

    bot.send(&presence("away")).await.unwrap();
    let stored = loop {
        if let ServerMessage::ProfileUpdate(p) = next_timeout(&mut bot).await {
            break p.status;
        }
    };
    assert_eq!(stored.as_deref(), Some("away"));
}
