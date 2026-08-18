//! The `Identify` handshake, which had no end-to-end test of its own.
//!
//! It exists because of a bug that was invisible from either side alone: the
//! server canonicalised the username *before* verifying the signature, while
//! both clients signed the string the user typed. Any name canonicalisation
//! altered — anything past 32 characters — was therefore signed over different
//! bytes than the server checked, and the handshake failed as
//! "signature did not verify". The identity is written to disk before the first
//! connect, so a user who picked a long display name was locked out of every
//! server, with an error pointing at their key rather than their name.
//!
//! `protocol::canonical_username` is now the single definition and both ends
//! call it. These tests drive the real gateway through the bot SDK, which signs
//! exactly the way the desktop client does.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::ServerMessage;

fn temp_data_dir() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "dioxusfun-identify-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

async fn spawn_gateway() -> (String, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let cfg = dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(),
        operators: Default::default(),
        data_dir: temp_data_dir(),
    };
    let handle = dioxusfun_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn");
    (format!("ws://{}", handle.addr), handle)
}

/// Connect with `username` and return the username the gateway accepted, or the
/// error it refused with. `connect_as_user` returns as soon as the frame is
/// sent, so the verdict is the *first event*, not the connect result.
async fn identify_as(url: &str, username: &str) -> Result<String, String> {
    let identity = BotIdentity::generate();
    let mut session = Bot::connect_as_user(url, &identity, username)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    match tokio::time::timeout(Duration::from_secs(5), session.next_event()).await {
        Ok(Some(ServerMessage::Ready { user, .. })) => Ok(user.username),
        Ok(Some(ServerMessage::Error { message })) => Err(message),
        Ok(other) => Err(format!("unexpected first frame: {other:?}")),
        Err(_) => Err("timed out waiting for the gateway's verdict".into()),
    }
}

/// The regression. Before the shared canonicalisation this was an `Error`
/// blaming the signature.
#[tokio::test]
async fn a_username_past_the_limit_still_identifies() {
    let (url, _h) = spawn_gateway().await;

    let accepted = identify_as(&url, &"a".repeat(33))
        .await
        .expect("a 33-character name must not be refused");
    assert_eq!(
        accepted,
        "a".repeat(32),
        "the server stores the truncated name, and that is what was signed"
    );

    // Far past the limit, and multi-byte — truncation counts characters, so a
    // byte-wise cut would not even be valid UTF-8 here.
    let accepted = identify_as(&url, &"🙂".repeat(80))
        .await
        .expect("a long multi-byte name must not be refused");
    assert_eq!(accepted.chars().count(), 32);
}

/// Whitespace was the other shape of the same bug. The desktop client trims
/// before it gets here, but the bot SDK is a public API and nothing stops a
/// caller passing a padded string.
#[tokio::test]
async fn surrounding_whitespace_does_not_break_the_handshake() {
    let (url, _h) = spawn_gateway().await;

    assert_eq!(identify_as(&url, "  bob  ").await.unwrap(), "bob");
    assert_eq!(
        identify_as(&url, "   ").await.unwrap(),
        "anonymous",
        "an all-whitespace name lands on the placeholder rather than empty"
    );
}

/// The case that hides the bug: a name needing no canonicalisation signs and
/// verifies identically either way, which is why this went unnoticed.
#[tokio::test]
async fn an_ordinary_username_is_unaffected() {
    let (url, _h) = spawn_gateway().await;

    assert_eq!(identify_as(&url, "alice").await.unwrap(), "alice");
    assert_eq!(
        identify_as(&url, &"b".repeat(32)).await.unwrap(),
        "b".repeat(32),
        "exactly at the limit is untouched"
    );
}

// Skipping wrong-key signature test: requires manual handshake (no `tokio-
// tungstenite` dep) and duplicates `auth.rs` coverage.
