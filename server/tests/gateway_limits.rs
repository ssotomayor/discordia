//! What an unauthenticated socket is allowed to cost the server.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::ClientMessage;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

fn temp_data_dir() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "dioxusfun-limits-{}-{}-{n}",
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
    (format!("ws://{}/gateway", handle.addr), handle)
}

fn bad_identify() -> String {
    serde_json::to_string(&ClientMessage::Identify {
        username: "mallory".into(),
        pubkey: "00".repeat(32),
        signature: "11".repeat(64),
        bot: false,
        client_version: String::new(),
    })
    .expect("serialize")
}

/// Reads until the socket goes away, and says whether it did. Anything the
/// server chooses to say on the way out is fine; being able to keep asking
/// forever is not.
async fn closed_within(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    budget: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(None) | Ok(Some(Err(_))) => return true,
            Ok(Some(Ok(WsMessage::Close(_)))) => return true,
            Ok(Some(Ok(_))) => continue,
            Err(_) => return false,
        }
    }
}

/// Every attempt is a Schnorr verification the server pays for, so the run has
/// to end on its own rather than whenever the caller gets bored.
#[tokio::test]
async fn a_run_of_bad_signatures_ends_the_connection() {
    let (url, _h) = spawn_gateway().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");

    for _ in 0..20 {
        if ws.send(WsMessage::Text(bad_identify())).await.is_err() {
            break;
        }
    }

    assert!(
        closed_within(&mut ws, Duration::from_secs(5)).await,
        "the gateway kept verifying signatures for an unauthenticated peer"
    );
}

/// Without a cap the frame is buffered whole before anything looks at it, and
/// tungstenite's own default is 64 MiB per connection.
#[tokio::test]
async fn an_oversized_frame_ends_the_connection() {
    let (url, _h) = spawn_gateway().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");

    let huge = "x".repeat(dioxusfun_server::gateway::MAX_FRAME_BYTES + 1);
    let _ = ws.send(WsMessage::Text(huge)).await;

    assert!(
        closed_within(&mut ws, Duration::from_secs(5)).await,
        "a frame past the cap was accepted"
    );
}

/// The cap has to admit what the server itself accepts, or the socket dies
/// before the handler can explain why.
#[tokio::test]
async fn a_frame_under_the_cap_is_still_read() {
    let (url, _h) = spawn_gateway().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");

    let big = "x".repeat(dioxusfun_server::state::MAX_IMAGE_LEN);
    ws.send(WsMessage::Text(big))
        .await
        .expect("send a legal-sized frame");

    let answered = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = ws.next().await {
            if let WsMessage::Text(t) = msg
                && t.contains("invalid frame")
            {
                return true;
            }
        }
        false
    })
    .await;

    assert_eq!(
        answered,
        Ok(true),
        "a frame the size of a legal image must reach the parser, not the socket's limit"
    );
}
