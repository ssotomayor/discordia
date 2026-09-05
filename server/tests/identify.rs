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
    let dir = temp_data_dir();
    let cfg = dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    };
    let handle = dioxusfun_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn");
    (format!("ws://{}", handle.addr), handle)
}

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

    let accepted = identify_as(&url, &"🙂".repeat(80))
        .await
        .expect("a long multi-byte name must not be refused");
    assert_eq!(accepted.chars().count(), 32);
}

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

/// Speaks the wire by hand so the test can sign for whatever address it likes.
async fn raw_identify(url: &str, identity: &BotIdentity, signed_for: &str, sent: &str) -> String {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("{url}/gateway"))
        .await
        .expect("connect");
    let nonce = loop {
        if let Some(Ok(WsMessage::Text(t))) = ws.next().await
            && let Ok(ServerMessage::Hello { nonce }) = serde_json::from_str::<ServerMessage>(&t)
        {
            break nonce;
        }
    };
    let identify = dioxusfun_server::protocol::ClientMessage::Identify {
        username: "alice".into(),
        pubkey: identity.pubkey().to_string(),
        signature: identity.sign_identify(&nonce, signed_for, "alice"),
        origin: sent.to_string(),
        bot: false,
        client_version: String::new(),
    };
    ws.send(WsMessage::Text(serde_json::to_string(&identify).unwrap()))
        .await
        .unwrap();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out waiting for the verdict")
        {
            Some(Ok(WsMessage::Text(t))) => match serde_json::from_str::<ServerMessage>(&t) {
                Ok(ServerMessage::Ready { .. }) => return "ready".into(),
                Ok(ServerMessage::Error { message }) => return message,
                _ => continue,
            },
            other => panic!("socket ended before a verdict: {other:?}"),
        }
    }
}

fn origin_of(url: &str) -> String {
    dioxusfun_server::protocol::dial_origin(&format!("{url}/gateway")).unwrap()
}

#[tokio::test]
async fn a_login_is_bound_to_the_address_that_was_dialed() {
    let (url, _h) = spawn_gateway().await;
    let id = BotIdentity::generate();
    let dialed = origin_of(&url);

    assert_eq!(raw_identify(&url, &id, &dialed, &dialed).await, "ready");

    let alias = dialed.replace("127.0.0.1", "localhost");
    assert_eq!(
        raw_identify(&url, &id, &alias, &alias).await,
        "ready",
        "every name the machine answers to is accepted"
    );

    let refused = raw_identify(&url, &id, "evil.example:9000", "evil.example:9000").await;
    assert!(
        refused.contains("does not answer to"),
        "a signature for a stranger's address is refused: {refused}"
    );

    let refused = raw_identify(&url, &id, "", "").await;
    assert!(
        refused.contains("too old"),
        "an unbound login is refused, not accepted for compatibility: {refused}"
    );
}

/// The relay attack: a server you dialed takes another server's nonce, has you
/// sign it, and forwards the result. Your signature names the address *you*
/// dialed, which is not one the other server answers to.
#[tokio::test]
async fn a_challenge_relayed_from_another_server_does_not_log_in_there() {
    let (evil, _e) = spawn_gateway().await;
    let (target, _t) = spawn_gateway().await;
    let victim = BotIdentity::generate();

    let dialed = origin_of(&evil);
    let forwarded = raw_identify(&target, &victim, &dialed, &dialed).await;
    assert!(
        forwarded.contains("does not answer to"),
        "the target refused the forwarded login: {forwarded}"
    );

    let lied = raw_identify(&target, &victim, &dialed, &origin_of(&target)).await;
    assert!(
        lied.contains("did not verify"),
        "renaming the origin in transit breaks the signature: {lied}"
    );
}
