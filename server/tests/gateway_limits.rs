//! What an unauthenticated socket is allowed to cost the server.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::ClientMessage;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as WsMessage;

fn temp_data_dir() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "discordia-limits-{}-{}-{n}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

async fn spawn_gateway() -> (String, discordia_server::ServerHandle) {
    spawn_gateway_with(Default::default()).await
}

async fn spawn_gateway_with(
    admission: discordia_server::admission::Admission,
) -> (String, discordia_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let dir = temp_data_dir();
    let cfg = discordia_server::ServerConfig {
        livekit: LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
        admission,
    };
    let handle = discordia_server::spawn(preferred, 100, cfg)
        .await
        .expect("spawn");
    (format!("ws://{}/gateway", handle.addr), handle)
}

fn bad_identify() -> String {
    serde_json::to_string(&ClientMessage::Identify {
        username: "mallory".into(),
        pubkey: "00".repeat(32),
        signature: "11".repeat(64),
        origin: "127.0.0.1:1".into(),
        bot: false,
        client_version: String::new(),
    })
    .expect("serialize")
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A raw socket past Identify and Ready, so what follows is held to the
/// identified cap rather than the pre-Identify one.
async fn identified(url: &str) -> Socket {
    use discordia_server::protocol::ServerMessage;
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("connect");
    let nonce = loop {
        if let Some(Ok(WsMessage::Text(t))) = ws.next().await
            && let Ok(ServerMessage::Hello { nonce }) = serde_json::from_str(&t)
        {
            break nonce;
        }
    };
    let id = discordia_bot::BotIdentity::generate();
    let origin = discordia_server::protocol::dial_origin(url).expect("origin");
    let identify = ClientMessage::Identify {
        username: "big".into(),
        signature: id.sign_identify(&nonce, &origin, "big"),
        pubkey: id.pubkey().to_string(),
        origin,
        bot: false,
        client_version: String::new(),
    };
    ws.send(WsMessage::Text(serde_json::to_string(&identify).unwrap()))
        .await
        .unwrap();
    loop {
        if let Some(Ok(WsMessage::Text(t))) = ws.next().await
            && t.contains("\"op\":\"ready\"")
        {
            return ws;
        }
    }
}

/// Reads until the socket goes away, and says whether it did. Anything the
/// server chooses to say on the way out is fine; being able to keep asking
/// forever is not.
async fn closed_within<S>(ws: &mut S, budget: Duration) -> bool
where
    S: futures_util::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
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

/// Garbage never reaches the parser, so a bar that counts parsed messages does
/// not count this at all — which is the one case the bar exists for.
///
/// Split, because the server answers every frame: sending without draining
/// fills both directions and the two ends wait on each other instead.
#[tokio::test]
async fn a_flood_of_frames_that_do_not_parse_still_ends_the_connection() {
    let (url, _h) = spawn_gateway().await;
    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    let (mut tx, mut rx) = ws.split();

    tokio::spawn(async move {
        for _ in 0..500 {
            if tx
                .send(WsMessage::Text("not json at all".into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    assert!(
        closed_within(&mut rx, Duration::from_secs(5)).await,
        "the gateway answered garbage forever"
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

    let huge = "x".repeat(discordia_server::gateway::MAX_FRAME_BYTES + 1);
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
    let mut ws = identified(&url).await;

    let big = "x".repeat(discordia_server::state::MAX_IMAGE_LEN);
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

#[tokio::test]
async fn a_combined_profile_upload_reaches_the_handler() {
    let (url, _h) = spawn_gateway().await;
    let mut ws = identified(&url).await;
    let image = "x".repeat(discordia_server::state::MAX_IMAGE_LEN);
    let profile = ClientMessage::SetProfile {
        avatar: Some(image.clone()),
        banner: Some(image),
        bio: None,
        status: None,
        custom_status: None,
        hide_guild_owner: false,
        hide_operator: false,
        updated_at: 0,
    };
    ws.send(WsMessage::Text(serde_json::to_string(&profile).unwrap()))
        .await
        .expect("send profile");
    let answered = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = ws.next().await {
            if let WsMessage::Text(text) = msg
                && text.contains("avatar")
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
        "two image fields must reach admission and image validation without closing the socket"
    );
}

/// A socket dropped for a full queue leaves through a different door than one
/// that closed; both have to give the address its slot back, or a LAN behind
/// one NAT is locked out a socket at a time.
#[tokio::test]
async fn a_socket_dropped_for_a_full_queue_gives_its_address_the_slot_back() {
    use discordia_server::media::{DEFAULT_MAX_BYTES, MediaStore};
    use discordia_server::protocol::ServerMessage;
    use discordia_server::state::{AppState, MAX_CONNECTIONS_PER_IP};
    use discordia_server::store::Store;

    let dir = temp_data_dir();
    let store = Store::open_in(&dir).await.expect("store");
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).expect("media");
    let state = AppState::load_or_seed(store, media, Default::default())
        .await
        .expect("state");
    let ip: std::net::IpAddr = "203.0.113.7".parse().unwrap();

    let mut held = Vec::new();
    for _ in 0..MAX_CONNECTIONS_PER_IP {
        held.push(state.register_conn(Some(ip)).expect("within the cap"));
    }
    assert!(state.register_conn(Some(ip)).is_err(), "the cap holds");

    // Nobody reads this one, so the next few thousand frames fill its queue
    // and the router drops it.
    let (dropped_id, _unread_rx) = held.pop().unwrap();
    for _ in 0..4096 {
        state.broadcast(ServerMessage::Error {
            message: "flood".into(),
        });
    }
    state.unregister_conn(dropped_id, None);

    assert!(
        state.register_conn(Some(ip)).is_ok(),
        "the dropped socket's slot was never returned to its address"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// One address is one machine at most, and a machine at rest holds one socket;
/// past the cap the newcomer is told why and closed before it costs a queue.
#[tokio::test]
async fn one_address_cannot_hold_more_than_its_share_of_sockets() {
    use discordia_server::state::MAX_CONNECTIONS_PER_IP;

    let (url, _h) = spawn_gateway().await;
    let mut held = Vec::new();
    for _ in 0..MAX_CONNECTIONS_PER_IP {
        let (ws, _) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("connect within the cap");
        held.push(ws);
    }

    let (mut extra, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("the tcp accept itself still happens");
    let verdict = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(msg)) = extra.next().await {
            if let WsMessage::Text(t) = msg
                && t.contains("too many connections")
            {
                return true;
            }
        }
        false
    })
    .await;
    assert_eq!(verdict, Ok(true), "the socket past the cap was admitted");
    assert!(
        closed_within(&mut extra, Duration::from_secs(5)).await,
        "the refused socket stayed open"
    );

    drop(held.pop());
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (mut again, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    let hello = tokio::time::timeout(Duration::from_secs(5), again.next())
        .await
        .expect("a frame")
        .expect("stream open")
        .expect("frame");
    assert!(
        matches!(hello, WsMessage::Text(t) if t.contains("hello")),
        "closing a socket gives the address its slot back"
    );
}

/// A browser always sends `Origin`; the desktop client and the bot SDK never
/// do. Refusing the header is what keeps a web page from driving the gateway
/// on a user's own machine or LAN — CORS does not apply to WebSockets.
#[tokio::test]
async fn an_upgrade_carrying_an_origin_header_is_refused() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let (url, _h) = spawn_gateway().await;

    let mut from_a_page = url.as_str().into_client_request().expect("request");
    from_a_page.headers_mut().insert(
        "Origin",
        "https://evil.example".parse().expect("header value"),
    );
    match tokio_tungstenite::connect_async(from_a_page).await {
        Ok(_) => panic!("a browser-originated upgrade was accepted"),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), 403, "refused with the wrong status");
        }
        Err(other) => panic!("refused, but not as an HTTP status: {other}"),
    }

    let (mut plain, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("a native client still connects");
    let hello = tokio::time::timeout(Duration::from_secs(5), plain.next())
        .await
        .expect("a frame")
        .expect("stream open")
        .expect("frame");
    assert!(matches!(hello, WsMessage::Text(t) if t.contains("hello")));
}

#[tokio::test]
async fn a_ten_mb_image_message_reaches_the_handler() {
    use base64::Engine as _;
    let (url, _handle) = spawn_gateway().await;
    let mut ws = identified(&url).await;
    let image = format!(
        "data:image/gif;base64,{}",
        base64::engine::general_purpose::STANDARD
            .encode(vec![0; discordia_server::protocol::MAX_GUILD_UPLOAD_BYTES])
    );
    let message = ClientMessage::SendMessage {
        channel_id: discordia_server::protocol::Id::new_v4(),
        content: String::new(),
        image: Some(image),
        image_name: None,
        reply_to: None,
    };
    let json = serde_json::to_string(&message).unwrap();
    assert!(json.len() < discordia_server::gateway::MAX_FRAME_BYTES);
    ws.send(WsMessage::Text(json)).await.unwrap();
    let answered = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(Ok(message)) = ws.next().await {
            if let WsMessage::Text(text) = message
                && text.contains("\"op\":\"error\"")
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
        "the configured maximum must reach validation rather than disconnect at the transport limit"
    );
}

/// An Identify is small, and a frame parsed before one costs the server many
/// times its size; past the pre-Identify cap the socket is closed unparsed.
#[tokio::test]
async fn a_large_frame_before_identify_ends_the_connection_unparsed() {
    let (url, _h) = spawn_gateway().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    let content_first = format!(
        "{{\"d\":[{}],\"op\":\"identify\"}}",
        vec!["[]"; discordia_server::gateway::PRE_IDENTIFY_FRAME_BYTES / 2].join(",")
    );
    ws.send(WsMessage::Text(content_first)).await.unwrap();
    assert!(
        closed_within(&mut ws, Duration::from_secs(5)).await,
        "an unidentified socket kept going after a frame no Identify needs"
    );

    let (mut small, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    small.send(WsMessage::Text(bad_identify())).await.unwrap();
    assert!(
        !closed_within(&mut small, Duration::from_millis(300)).await,
        "an Identify-sized frame is still read"
    );
}

/// The socket a full queue gave up on keeps its slot until its task has ended,
/// and is told to end: a slot handed back while the task ran was uncounted.
#[tokio::test]
async fn a_socket_dropped_for_a_full_queue_is_closed_before_its_slot_returns() {
    use discordia_server::media::{DEFAULT_MAX_BYTES, MediaStore};
    use discordia_server::protocol::ServerMessage;
    use discordia_server::state::{AppState, MAX_CONNECTIONS_PER_IP};
    use discordia_server::store::Store;

    let dir = temp_data_dir();
    let store = Store::open_in(&dir).await.expect("store");
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).expect("media");
    let state = AppState::load_or_seed(store, media, Default::default())
        .await
        .expect("state");
    let ip: std::net::IpAddr = "203.0.113.8".parse().unwrap();
    let origin = Some(discordia_server::admission::Origin::ip(ip));
    let mut held = Vec::new();
    for _ in 0..MAX_CONNECTIONS_PER_IP {
        held.push(
            state
                .admit(Some(ip), origin.clone())
                .expect("within the cap"),
        );
    }
    let (dropped, _unread, close) = held.pop().unwrap();
    for _ in 0..4096 {
        state.broadcast(ServerMessage::Error {
            message: "flood".into(),
        });
    }
    assert!(state.is_closing(dropped));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), close.notified())
            .await
            .is_ok(),
        "the socket's task was not told to end"
    );
    assert!(
        state.admit(Some(ip), origin.clone()).is_err(),
        "the slot came back while the socket's task still ran"
    );
    state.unregister_conn(dropped, None);
    assert!(state.admit(Some(ip), origin).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

fn limited(
    f: impl FnOnce(&mut discordia_server::admission::Limits),
) -> discordia_server::admission::Admission {
    let mut admission = discordia_server::admission::Admission::default();
    f(&mut admission.limits);
    admission
}

/// What the gateway says to a socket on `url` claiming to come from `client`.
async fn first_word(url: &str, client: &str) -> String {
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url)
            .unwrap();
    request
        .headers_mut()
        .insert("x-forwarded-for", client.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("connect");
    let word = match ws.next().await {
        Some(Ok(WsMessage::Text(t))) => t,
        other => format!("{other:?}"),
    };
    // Held open so it keeps its slot; the server's end is what is counted.
    tokio::spawn(async move { while ws.next().await.is_some() {} });
    word
}

/// Behind a TLS proxy every client arrives from the proxy's address; only a
/// proxy the operator names may say who is really there, and nobody else can.
#[tokio::test]
async fn a_trusted_proxy_names_the_client_and_nobody_else_can() {
    let mut admission = limited(|l| l.sockets_per_origin = 2);
    admission.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
    let (url, _h) = spawn_gateway_with(admission).await;
    for _ in 0..2 {
        assert!(first_word(&url, "198.51.100.1").await.contains("hello"));
    }
    assert!(
        first_word(&url, "198.51.100.1")
            .await
            .contains("too many connections"),
        "one client behind the proxy is held to its own share"
    );
    assert!(
        first_word(&url, "198.51.100.2").await.contains("hello"),
        "and another client behind it is not charged for the first"
    );

    let (url, _h) = spawn_gateway_with(limited(|l| l.sockets_per_origin = 2)).await;
    for client in ["198.51.100.1", "198.51.100.2"] {
        assert!(first_word(&url, client).await.contains("hello"));
    }
    assert!(
        first_word(&url, "198.51.100.3")
            .await
            .contains("too many connections"),
        "an untrusted peer's header is ignored"
    );
}

/// A connection that never finishes a request head is closed at the deadline,
/// and one address can hold only a few of them meanwhile.
#[tokio::test]
async fn an_unfinished_request_is_cut_off_and_counted() {
    use tokio::io::AsyncReadExt;
    let admission = limited(|l| {
        l.header_read = Duration::from_millis(300);
        l.pending_per_origin = 2;
    });
    let (url, _h) = spawn_gateway_with(admission).await;
    let addr = url
        .trim_start_matches("ws://")
        .trim_end_matches("/gateway")
        .to_string();
    let read_eof = |mut tcp: tokio::net::TcpStream, within: Duration| async move {
        let mut buf = [0u8; 64];
        matches!(
            tokio::time::timeout(within, tcp.read(&mut buf)).await,
            Ok(Ok(0)) | Ok(Err(_))
        )
    };

    let held: Vec<_> = futures_util::future::join_all(
        (0..2).map(|_| tokio::net::TcpStream::connect(addr.clone())),
    )
    .await
    .into_iter()
    .map(Result::unwrap)
    .collect();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let third = tokio::net::TcpStream::connect(addr.clone()).await.unwrap();
    assert!(
        read_eof(third, Duration::from_millis(200)).await,
        "a third unfinished request from one address was held"
    );
    for tcp in held {
        assert!(
            read_eof(tcp, Duration::from_secs(2)).await,
            "a silent connection outlived the header deadline"
        );
    }
    let (_ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("the slots came back");
}

/// Relayed QUIC has no address (trap 15), so it is charged to the relaying
/// peer's key, and relayed peers together hold only a share of the server.
#[tokio::test]
async fn relayed_sockets_count_per_peer_and_in_all() {
    use discordia_server::admission::{Limits, Origin};
    use discordia_server::media::{DEFAULT_MAX_BYTES, MediaStore};
    use discordia_server::state::AppState;
    use discordia_server::store::Store;

    let dir = temp_data_dir();
    let store = Store::open_in(&dir).await.expect("store");
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).expect("media");
    let state = AppState::load_or_seed(store, media, Default::default())
        .await
        .expect("state");
    state.set_limits(Limits {
        sockets_per_origin: 2,
        relayed_sockets: 3,
        ..Default::default()
    });
    let relayed = |n: &str| Some(Origin::Relayed(n.into()));
    let a1 = state.admit(None, relayed("a")).unwrap();
    state.admit(None, relayed("a")).unwrap();
    assert!(
        state.admit(None, relayed("a")).is_err(),
        "per relaying peer"
    );
    state.admit(None, relayed("b")).unwrap();
    assert!(
        state.admit(None, relayed("c")).is_err(),
        "relayed peers in all"
    );
    assert!(
        state
            .admit(
                Some("192.0.2.9".parse().unwrap()),
                Some(Origin::ip("192.0.2.9".parse().unwrap()))
            )
            .is_ok(),
        "an addressed client is not charged for them"
    );
    state.unregister_conn(a1.0, None);
    assert!(state.admit(None, relayed("c")).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}
