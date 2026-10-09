//! The gateway's `/sfu/*` is the bundled SFU's signaling for callers who can
//! reach the gateway and nothing else. A stand-in SFU here answers like
//! LiveKit does: echoes on `/rtc`, 401 for a bad token, a word on
//! `rtc/validate`.

use std::net::SocketAddr;

use axum::Router;
use axum::extract::Query;
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use dioxusfun_server::livekit::LiveKitConfig;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

#[derive(serde::Deserialize)]
struct Token {
    access_token: Option<String>,
}

async fn rtc(ws: WebSocketUpgrade, Query(token): Query<Token>, headers: HeaderMap) -> Response {
    let authorized = match headers.get(header::AUTHORIZATION) {
        Some(value) => value == "Bearer good",
        None => token.access_token.as_deref() == Some("good"),
    };
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

fn test_config(livekit: LiveKitConfig) -> dioxusfun_server::ServerConfig {
    let dir = std::env::temp_dir().join(format!(
        "dioxusfun-sfu-proxy-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    dioxusfun_server::ServerConfig {
        livekit,
        operators: Default::default(),
        identities: Default::default(),
        media_max_bytes: dioxusfun_server::media::DEFAULT_MAX_BYTES,
        data_dir: dir,
    }
}

async fn gateway(livekit: LiveKitConfig) -> (SocketAddr, dioxusfun_server::ServerHandle) {
    let preferred: SocketAddr = "127.0.0.1:19500".parse().unwrap();
    let handle = dioxusfun_server::spawn(preferred, 200, test_config(livekit))
        .await
        .expect("spawn server");
    (handle.addr, handle)
}

fn bundled(port: u16) -> LiveKitConfig {
    LiveKitConfig {
        explicit_url: None,
        port,
        ..LiveKitConfig::from_env(&std::env::temp_dir())
    }
}

#[tokio::test]
async fn signaling_reaches_the_bundled_sfu_through_the_gateway() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let (addr, _gateway) = gateway(bundled(sfu_port)).await;

    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://{addr}/sfu/rtc?access_token=good"))
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

    for path in ["rtc", "rtc/v1"] {
        let mut request = format!("ws://{addr}/sfu/{path}")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(header::AUTHORIZATION, "Bearer good".parse().unwrap());
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
async fn forged_bearer_authorization_is_rejected_by_the_sfu() {
    let (sfu_port, _sfu) = fake_sfu().await;
    let (addr, _gateway) = gateway(bundled(sfu_port)).await;
    let mut request = format!("ws://{addr}/sfu/rtc?access_token=good")
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

    let refused =
        tokio_tungstenite::connect_async(format!("ws://{addr}/sfu/rtc?access_token=forged"))
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
