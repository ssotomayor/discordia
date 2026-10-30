//! The gateway's `/sfu/*` is the bundled SFU's signaling for callers who can
//! reach the gateway and nothing else. A stand-in SFU here answers like
//! LiveKit does: echoes on `/rtc`, 401 for a token it refuses, a word on
//! `rtc/validate`. The gateway itself admits only a token its SFU keys signed
//! for a call the caller is in now.

use std::net::SocketAddr;

use axum::Router;
use axum::extract::Query;
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use discordia_bot::{Bot, BotIdentity};
use discordia_server::livekit::LiveKitConfig;
use discordia_server::protocol::{ChannelKind, ClientMessage, Id, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use livekit_api::access_token::{AccessToken, TokenVerifier, VideoGrants};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

const KEY: &str = "devkey";
const SECRET: &str = "secret-long-enough-for-hs256-signing";

#[derive(serde::Deserialize)]
struct Token {
    access_token: Option<String>,
}

async fn rtc(ws: WebSocketUpgrade, Query(token): Query<Token>, headers: HeaderMap) -> Response {
    let presented = match headers.get(header::AUTHORIZATION) {
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(String::from),
        None => token.access_token,
    };
    let authorized = presented
        .and_then(|t| TokenVerifier::with_api_key(KEY, SECRET).verify(&t).ok())
        .is_some_and(|claims| claims.name != "refused");
    if !authorized {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(|mut socket| async move {
        while let Some(Ok(message)) = socket.recv().await {
            let reply = match message {
                Message::Text(t) => Message::Text(format!("sfu:{t}")),
                Message::Binary(b) => Message::Binary(b),
                Message::Close(_) => break,
                other => other,
            };
            if socket.send(reply).await.is_err() {
                break;
            }
        }
    })
}

async fn validate() -> &'static str {
    "success"
}

async fn fake_sfu() -> (u16, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/rtc", get(rtc))
        .route("/rtc/v1", get(rtc))
        .route("/rtc/validate", get(validate));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (port, task)
}

fn test_config(livekit: LiveKitConfig) -> discordia_server::ServerConfig {
    let dir = std::env::temp_dir().join(format!(
        "discordia-sfu-proxy-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    discordia_server::ServerConfig {
        livekit,
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: discordia_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
        admission: Default::default(),
    }
}

async fn gateway_with(
    cfg: discordia_server::ServerConfig,
) -> (SocketAddr, discordia_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:19500".parse().unwrap();
    let handle = discordia_server::spawn(preferred, 200, cfg)
        .await
        .expect("spawn server");
    (handle.addr, handle)
}

async fn gateway(livekit: LiveKitConfig) -> (SocketAddr, discordia_server::ServerHandle) {
    gateway_with(test_config(livekit)).await
}

fn bundled(port: u16) -> LiveKitConfig {
    LiveKitConfig {
        explicit_url: None,
        port,
        api_key: KEY.into(),
        api_secret: SECRET.into(),
        ..LiveKitConfig::from_env(&std::env::temp_dir())
    }
}

async fn next_event(bot: &mut Bot) -> ServerMessage {
    tokio::time::timeout(std::time::Duration::from_secs(5), bot.next_event())
        .await
        .expect("a gateway event in time")
        .expect("gateway open")
}

/// Someone sitting in a voice channel of their own guild, so a tunnel has a
/// call to belong to. The session is returned: dropping it ends the call.
async fn in_call(gateway: SocketAddr) -> (Bot, String, Id) {
    let id = BotIdentity::generate();
    let mut bot = Bot::connect_as_user(&format!("ws://{gateway}/gateway"), &id, "Caller")
        .await
        .unwrap();
    bot.send(&ClientMessage::CreateGuild {
        name: "Call".into(),
        template: None,
    })
    .await
    .unwrap();
    let guild_id = loop {
        if let ServerMessage::GuildJoined { guild, .. } = next_event(&mut bot).await {
            break guild.id;
        }
    };
    bot.send(&ClientMessage::CreateChannel {
        divider: Default::default(),
        guild_id,
        name: "Room".into(),
        kind: ChannelKind::Voice,
        topic: None,
    })
    .await
    .unwrap();
    let channel = loop {
        if let ServerMessage::ChannelCreate(ch) = next_event(&mut bot).await
            && ch.name == "Room"
        {
            break ch.id;
        }
    };
    bot.send(&ClientMessage::JoinVoice {
        channel_id: channel,
        preferences: None,
    })
    .await
    .unwrap();
    loop {
        if let ServerMessage::VoiceStateUpdate(vs) = next_event(&mut bot).await
            && vs.channel_id == Some(channel)
        {
            break;
        }
    }
    (bot, id.pubkey().to_string(), channel)
}

fn token(identity: &str, name: &str, channel: Id) -> String {
    AccessToken::with_api_key(KEY, SECRET)
        .with_identity(identity)
        .with_name(name)
        .with_grants(VideoGrants {
            room_join: true,
            room: format!("voice-{channel}"),
            ..Default::default()
        })
        .to_jwt()
        .unwrap()
}

#[tokio::test]
async fn sfu_signaling_roundtrips_over_an_authenticated_quic_stream() {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        use discordia_server::quic;
        use iroh::endpoint::presets;
        use iroh::{Endpoint, EndpointAddr, RelayMode, TransportAddr};

        let (sfu_port, sfu) = fake_sfu().await;
        let mut cfg = test_config(bundled(sfu_port));
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = tcp.local_addr().unwrap().port();
        cfg.identities = discordia_server::local_identities(port);
        let router = discordia_server::build_router(cfg).await.unwrap();
        let web = discordia_server::serve_router(tcp, router.clone());
        let (_caller, key, channel) = in_call(web.addr).await;
        let gateway = quic::serve_quic(router, None).await.unwrap();
        let endpoint = Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let addr = EndpointAddr::new(gateway.endpoint_id).with_addrs(
            quic::dialable_addrs(&gateway.sockets)
                .into_iter()
                .map(TransportAddr::Ip),
        );
        let conn = endpoint.connect(addr, quic::GATEWAY_ALPN).await.unwrap();
        let (send, recv) = conn.open_bi().await.unwrap();
        let (mut ws, _) = tokio_tungstenite::client_async(
            format!(
                "ws://gateway.invalid/sfu/rtc?access_token={}",
                token(&key, "Caller", channel)
            ),
            tokio::io::join(recv, send),
        )
        .await
        .unwrap();
        ws.send(tungstenite::Message::Text("quic signaling".into()))
            .await
            .unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert_eq!(reply.into_text().unwrap(), "sfu:quic signaling");
        ws.close(None).await.unwrap();
        endpoint.close().await;
        gateway.shutdown().await;
        sfu.abort();
    })
    .await
    .expect("SFU signaling over QUIC exceeded 20 seconds");
}

#[tokio::test]
async fn signaling_reaches_the_bundled_sfu_through_the_gateway() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let (addr, _gateway) = gateway(bundled(sfu_port)).await;
    let (_caller, key, channel) = in_call(addr).await;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/sfu/rtc?access_token={}",
        token(&key, "Caller", channel)
    ))
    .await
    .expect("tunnelled handshake");
    ws.send(tungstenite::Message::Text("hello".into()))
        .await
        .unwrap();
    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("a reply in time")
        .expect("stream open")
        .expect("a frame");
    assert_eq!(reply, tungstenite::Message::Text("sfu:hello".into()));

    ws.send(tungstenite::Message::Binary(vec![1, 2, 3]))
        .await
        .unwrap();
    let reply = ws.next().await.unwrap().unwrap();
    assert_eq!(reply, tungstenite::Message::Binary(vec![1, 2, 3]));
}

#[tokio::test]
async fn native_bearer_authorization_reaches_the_sfu() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let (addr, _gateway) = gateway(bundled(sfu_port)).await;
    let (_caller, key, channel) = in_call(addr).await;
    let bearer = format!("Bearer {}", token(&key, "Caller", channel));

    for path in ["rtc", "rtc/v1"] {
        let mut request = format!("ws://{addr}/sfu/{path}")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, bearer.parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        ws.send(tungstenite::Message::Text("native".into()))
            .await
            .unwrap();
        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(reply, tungstenite::Message::Text("sfu:native".into()));
        ws.close(None).await.unwrap();
    }
}

#[tokio::test]
async fn forged_bearer_authorization_is_refused() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let (addr, _gateway) = gateway(bundled(sfu_port)).await;
    let (_caller, key, channel) = in_call(addr).await;
    let good = token(&key, "Caller", channel);
    let mut request = format!("ws://{addr}/sfu/rtc?access_token={good}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(header::AUTHORIZATION, "Bearer forged".parse().unwrap());
    match tokio_tungstenite::connect_async(request).await.unwrap_err() {
        tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        other => panic!("expected an HTTP refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn the_sfu_verdict_on_a_bad_token_comes_back_unchanged() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let (addr, _gateway) = gateway(bundled(sfu_port)).await;
    let (_caller, key, channel) = in_call(addr).await;

    let refused = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/sfu/rtc?access_token={}",
        token(&key, "refused", channel)
    ))
    .await
    .expect_err("the handshake is refused");
    match refused {
        tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED)
        }
        other => panic!("expected an HTTP refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn the_validate_probe_is_forwarded_as_plain_http() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let (addr, _gateway) = gateway(bundled(sfu_port)).await;

    let (status, body) = http_get(addr, "/sfu/rtc/validate?access_token=good").await;
    assert_eq!(status, 200);
    assert_eq!(body, "success");
}

#[tokio::test]
async fn a_server_on_a_shared_sfu_has_no_tunnel() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let shared = LiveKitConfig {
        explicit_url: Some("ws://151.243.137.35:7880".into()),
        ..bundled(sfu_port)
    };
    let (addr, _gateway) = gateway(shared).await;

    let refused =
        tokio_tungstenite::connect_async(format!("ws://{addr}/sfu/rtc?access_token=good"))
            .await
            .expect_err("no tunnel is served");
    match refused {
        tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), StatusCode::NOT_FOUND)
        }
        other => panic!("expected 404, got {other:?}"),
    }
}

fn refusal(err: tungstenite::Error) -> StatusCode {
    match err {
        tungstenite::Error::Http(response) => response.status(),
        other => panic!("expected an HTTP refusal, got {other:?}"),
    }
}

/// Only a caller in a call needs the tunnel, so a token for any other key,
/// however well signed, holds none.
#[tokio::test]
async fn a_key_in_no_call_gets_no_tunnel() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let (addr, _gateway) = gateway(bundled(sfu_port)).await;
    let (caller, key, channel) = in_call(addr).await;
    let stranger = BotIdentity::generate();
    let url = |identity: &str| {
        format!(
            "ws://{addr}/sfu/rtc?access_token={}",
            token(identity, "x", channel)
        )
    };
    let refused = tokio_tungstenite::connect_async(url(stranger.pubkey()))
        .await
        .expect_err("a stranger's token");
    assert_eq!(refusal(refused), StatusCode::UNAUTHORIZED);
    let suffixed = format!("{key}#video");
    assert!(
        tokio_tungstenite::connect_async(url(&suffixed))
            .await
            .is_ok(),
        "a caller's second identity in the same call (trap 10)"
    );
    drop(caller);
    tokio::time::sleep(std::time::Duration::from_millis(3_500)).await;
    let refused = tokio_tungstenite::connect_async(url(&key))
        .await
        .expect_err("the call is over");
    assert_eq!(refusal(refused), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn one_key_and_one_address_hold_only_their_share_of_tunnels() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let mut cfg = test_config(bundled(sfu_port));
    cfg.admission.limits.tunnels_per_key = 2;
    cfg.admission.limits.tunnels_per_origin = 3;
    let (addr, _gateway) = gateway_with(cfg).await;
    let (_a, key_a, channel_a) = in_call(addr).await;
    let (_b, key_b, channel_b) = in_call(addr).await;
    let open = |key: &str, channel: Id| {
        let url = format!(
            "ws://{addr}/sfu/rtc?access_token={}",
            token(key, "Caller", channel)
        );
        async move { tokio_tungstenite::connect_async(url).await }
    };

    let mut held = vec![
        open(&key_a, channel_a).await.unwrap(),
        open(&key_a, channel_a).await.unwrap(),
    ];
    let refused = open(&key_a, channel_a)
        .await
        .expect_err("past the key's share");
    assert_eq!(refusal(refused), StatusCode::SERVICE_UNAVAILABLE);

    held.push(open(&key_b, channel_b).await.unwrap());
    let refused = open(&key_b, channel_b)
        .await
        .expect_err("past the address's share");
    assert_eq!(refusal(refused), StatusCode::SERVICE_UNAVAILABLE);

    let (mut first, _) = held.remove(0);
    first.close(None).await.unwrap();
    while first.next().await.is_some() {}
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        open(&key_a, channel_a).await.is_ok(),
        "a closed tunnel gives its slot back"
    );
}

/// Signaling is small; tungstenite's default let one message hold 64 MiB per tunnel.
#[tokio::test]
async fn a_signaling_message_past_its_cap_ends_the_tunnel() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let (addr, _gateway) = gateway(bundled(sfu_port)).await;
    let (_caller, key, channel) = in_call(addr).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!(
        "ws://{addr}/sfu/rtc?access_token={}",
        token(&key, "Caller", channel)
    ))
    .await
    .unwrap();
    let big = "x".repeat(discordia_server::sfu_proxy::MAX_SIGNAL_MESSAGE + 1);
    let _ = ws.send(tungstenite::Message::Text(big)).await;
    let ended = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(tungstenite::Message::Close(_))) => return true,
                Some(Ok(tungstenite::Message::Text(t))) if t.starts_with("sfu:x") => return false,
                _ => {}
            }
        }
    })
    .await;
    assert_eq!(ended, Ok(true), "the oversized message was forwarded");
}

/// One request, no client crate: the status line and the body after the blank line.
async fn http_get(addr: SocketAddr, path: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    tcp.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut raw = String::new();
    tcp.read_to_string(&mut raw).await.unwrap();
    let status: u16 = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("a status line");
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    (status, body.to_string())
}

/// A bundled SFU that failed to start leaves its port to whoever takes it, so
/// the gateway opens no tunnel there, however good the caller's token.
#[tokio::test]
async fn no_tunnel_goes_to_a_local_sfu_that_is_not_ours() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let livekit = bundled(sfu_port);
    livekit.local_sfu.set(false);
    let (addr, _gateway) = gateway(livekit).await;
    let refused = tokio_tungstenite::connect_async(format!("ws://{addr}/sfu/rtc?access_token=x"))
        .await
        .expect_err("no tunnel");
    assert_eq!(refusal(refused), StatusCode::NOT_FOUND);
}
