//! Stopping a host has to take the sockets with it.
//!
//! Dropping the listener does not: axum spawns a task per connection and
//! `on_upgrade` spawns another, neither of them a child of the accept loop, so
//! before `GatewayShutdown` a stopped self-host went on serving everyone
//! already connected until their TCP or QUIC connection happened to fail.

use std::net::SocketAddr;
use std::time::Duration;

use dioxusfun_bot::{Bot, BotIdentity};
use dioxusfun_server::livekit::LiveKitConfig;
use dioxusfun_server::protocol::ServerMessage;
use futures_util::StreamExt;

/// Generous next to the close, which is immediate, and short enough that a
/// socket left running fails the test rather than hanging the suite.
const CLOSE_WITHIN: Duration = Duration::from_secs(3);

fn test_config() -> dioxusfun_server::ServerConfig {
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-shutdown-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    dioxusfun_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    }
}

async fn spawn_gateway() -> (
    String,
    dioxusfun_server::ServerHandle,
    dioxusfun_server::GatewayShutdown,
) {
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = dioxusfun_server::bind_with_fallback(preferred, 100)
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let mut cfg = test_config();
    cfg.identities
        .extend(dioxusfun_server::local_identities(addr.port()));
    let (router, shutdown) = dioxusfun_server::build_gateway(cfg)
        .await
        .expect("build gateway");
    let handle = dioxusfun_server::serve_router(listener, router);
    (format!("ws://{addr}"), handle, shutdown)
}

async fn connect_user(url: &str, name: &str) -> Bot {
    let id = BotIdentity::generate();
    let mut session = Bot::connect_as_user(url, &id, name).await.unwrap();
    loop {
        match tokio::time::timeout(Duration::from_secs(5), session.next_event())
            .await
            .expect("timed out waiting for Ready")
        {
            Some(ServerMessage::Ready { .. }) => break,
            Some(_) => continue,
            None => panic!("connection closed before Ready"),
        }
    }
    session
}

#[tokio::test]
async fn stopping_the_host_closes_every_open_socket() {
    let (url, handle, shutdown) = spawn_gateway().await;
    let mut alice = connect_user(&url, "alice").await;
    let mut bob = connect_user(&url, "bob").await;

    // The listener goes first, exactly as a stopped self-host drops it. On its
    // own this leaves both sessions being served.
    handle.abort();
    shutdown.close_all();

    for (who, session) in [("alice", &mut alice), ("bob", &mut bob)] {
        let ended = tokio::time::timeout(CLOSE_WITHIN, async {
            while session.next_event().await.is_some() {}
        })
        .await;
        assert!(ended.is_ok(), "{who}'s socket was still open");
    }
}

/// The client names the reason it went; "connection closed" would be true and
/// useless. It travels in the close frame, which is why the frame carries one.
#[tokio::test]
async fn the_close_frame_says_the_host_stopped_it() {
    let (url, _handle, shutdown) = spawn_gateway().await;
    let (mut socket, _) = tokio_tungstenite::connect_async(format!("{url}/gateway"))
        .await
        .expect("connect");

    // Wait for Hello, so the connection is in the loop and not still upgrading.
    let hello = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("timed out waiting for Hello");
    assert!(hello.is_some(), "the gateway closed before Hello");

    shutdown.close_all();

    let closed = tokio::time::timeout(CLOSE_WITHIN, async {
        while let Some(Ok(frame)) = socket.next().await {
            if let tokio_tungstenite::tungstenite::Message::Close(f) = frame {
                return f.map(|f| f.reason.to_string());
            }
        }
        None
    })
    .await
    .expect("timed out waiting for the close frame");

    assert_eq!(
        closed.as_deref(),
        Some(dioxusfun_server::gateway::HOST_STOPPED)
    );
}

/// A socket that arrives after the switch was thrown must not sleep through
/// it: a watch fires on a *change*, and for this one the change is past.
#[tokio::test]
async fn a_socket_that_arrives_after_the_stop_is_closed_too() {
    let (url, _handle, shutdown) = spawn_gateway().await;
    shutdown.close_all();

    let (mut socket, _) = tokio_tungstenite::connect_async(format!("{url}/gateway"))
        .await
        .expect("connect");

    // Stop on the first error too: a tungstenite client that has already
    // answered the close keeps reporting one rather than ending the stream.
    let ended = tokio::time::timeout(CLOSE_WITHIN, async {
        while let Some(Ok(_)) = socket.next().await {}
    })
    .await;
    assert!(ended.is_ok(), "the socket was still open");
}
