//! End-to-end rendezvous name-claim handshake over a real WebSocket:
//! Challenge → signed Register → Registered, and the rejection paths (bad
//! signature, name already owned by a different key).

use std::sync::Arc;

use dioxusfun_rendezvous::{AppCtx, Config, registry::Registry, router};
use futures_util::{SinkExt, StreamExt};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;

/// Spawn the rendezvous router on an ephemeral port; return its ws base URL.
async fn spawn() -> String {
    spawn_with(Config::default()).await.0
}

/// Spawn with a specific config, handing back the registry so a test can see
/// what the rendezvous thinks is live.
async fn spawn_with(config: Config) -> (String, Arc<Registry>) {
    let registry = Arc::new(Registry::new());
    let ctx = AppCtx {
        registry: registry.clone(),
        config: Arc::new(config),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(ctx)).await.unwrap();
    });
    (format!("ws://{addr}"), registry)
}

/// A test identity: (secret, 64-char x-only pubkey hex).
fn identity(seed: u8) -> (SecretKey, String) {
    let secp = Secp256k1::new();
    let secret = SecretKey::from_slice(&[seed; 32]).unwrap();
    let keypair = Keypair::from_secret_key(&secp, &secret);
    let (xonly, _) = keypair.x_only_public_key();
    (secret, hex::encode(xonly.serialize()))
}

fn sign(secret: &SecretKey, nonce: &str, pubkey: &str, name: &str) -> String {
    let secp = Secp256k1::new();
    let keypair = Keypair::from_secret_key(&secp, secret);
    let mut msg = Vec::new();
    msg.extend_from_slice(nonce.as_bytes());
    msg.extend_from_slice(pubkey.as_bytes());
    msg.extend_from_slice(name.as_bytes());
    let digest: [u8; 32] = Sha256::digest(&msg).into();
    let m = secp256k1::Message::from_digest(digest);
    hex::encode(secp.sign_schnorr_no_aux_rand(&m, &keypair).serialize())
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn next_json(ws: &mut Ws) -> serde_json::Value {
    loop {
        match ws.next().await.unwrap().unwrap() {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            _ => continue,
        }
    }
}

/// Connect to /control, read the challenge nonce.
async fn connect_control(base: &str) -> (Ws, String) {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("{base}/control"))
        .await
        .unwrap();
    let challenge = next_json(&mut ws).await;
    assert_eq!(challenge["op"], "challenge");
    let nonce = challenge["d"]["nonce"].as_str().unwrap().to_string();
    (ws, nonce)
}

async fn send_register(ws: &mut Ws, name: &str, pubkey: &str, signature: &str) {
    let frame = serde_json::json!({
        "op": "register",
        "d": { "name": name, "pubkey": pubkey, "signature": signature, "publish_public": false }
    });
    ws.send(Message::Text(frame.to_string())).await.unwrap();
}

#[tokio::test]
async fn signed_name_claim_succeeds_and_becomes_the_shortcode() {
    let base = spawn().await;
    let (secret, pubkey) = identity(1);

    let (mut ws, nonce) = connect_control(&base).await;
    let sig = sign(&secret, &nonce, &pubkey, "Acme");
    send_register(&mut ws, "Acme", &pubkey, &sig).await;

    let reply = next_json(&mut ws).await;
    assert_eq!(reply["op"], "registered", "got {reply}");
    // The slug (lowercased name) is the join shortcode.
    assert_eq!(reply["d"]["shortcode"], "acme");
}

#[tokio::test]
async fn bad_signature_is_rejected() {
    let base = spawn().await;
    let (_secret, pubkey) = identity(2);
    let (mut ws, _nonce) = connect_control(&base).await;
    // Sign the wrong thing.
    send_register(&mut ws, "acme", &pubkey, &"00".repeat(64)).await;
    let reply = next_json(&mut ws).await;
    assert_eq!(reply["op"], "error", "got {reply}");
}

#[tokio::test]
async fn a_second_key_cannot_take_a_claimed_name() {
    let base = spawn().await;
    let (secret_a, pubkey_a) = identity(3);
    let (secret_b, pubkey_b) = identity(4);

    // Owner A claims "shared" and stays connected (holding the live slot).
    let (mut ws_a, nonce_a) = connect_control(&base).await;
    let sig_a = sign(&secret_a, &nonce_a, &pubkey_a, "shared");
    send_register(&mut ws_a, "shared", &pubkey_a, &sig_a).await;
    assert_eq!(next_json(&mut ws_a).await["op"], "registered");

    // B, a different key, presents a perfectly valid signature for its OWN key
    // but is refused — the name is owned by A.
    let (mut ws_b, nonce_b) = connect_control(&base).await;
    let sig_b = sign(&secret_b, &nonce_b, &pubkey_b, "shared");
    send_register(&mut ws_b, "shared", &pubkey_b, &sig_b).await;
    let reply = next_json(&mut ws_b).await;
    assert_eq!(reply["op"], "error", "got {reply}");
    assert!(reply["d"]["message"].as_str().unwrap().contains("taken"));
}

/// A host that stops answering must lose its listing.
///
/// This is the case that left dead hosts in the browse list: the process is
/// gone (or asleep, or behind a dropped link) but the TCP connection was never
/// closed, so the rendezvous' read side simply never yields and nothing
/// unregisters it. Reproduced here by registering and then never polling the
/// socket again — tungstenite answers pings while it is being polled, so a
/// client that stops reading stops ponging, exactly like a host that died.
#[tokio::test]
async fn silent_host_is_dropped_from_the_listing() {
    let (base, registry) = spawn_with(Config {
        heartbeat_interval: std::time::Duration::from_millis(50),
        host_timeout: std::time::Duration::from_millis(200),
        ..Config::default()
    })
    .await;
    let (secret, pubkey) = identity(9);

    let (mut ws, nonce) = connect_control(&base).await;
    let sig = sign(&secret, &nonce, &pubkey, "Ghost");
    let frame = serde_json::json!({
        "op": "register",
        "d": { "name": "Ghost", "pubkey": pubkey, "signature": sig, "publish_public": true }
    });
    ws.send(Message::Text(frame.to_string())).await.unwrap();
    let reply = next_json(&mut ws).await;
    assert_eq!(reply["op"], "registered", "got {reply}");
    assert_eq!(
        registry.discover().len(),
        1,
        "host should be listed while alive"
    );

    // Hold the socket open but never poll it again: no Pong will be sent, while
    // the kernel keeps ACKing so the rendezvous' writes still succeed.
    let _held = ws;
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;

    assert!(
        registry.discover().is_empty(),
        "a host that stopped answering should not still be advertised"
    );
}

/// The converse: a host that keeps answering must NOT be dropped, or the
/// heartbeat would be worse than the bug it fixes.
#[tokio::test]
async fn responsive_host_keeps_its_listing() {
    let (base, registry) = spawn_with(Config {
        heartbeat_interval: std::time::Duration::from_millis(50),
        host_timeout: std::time::Duration::from_millis(200),
        ..Config::default()
    })
    .await;
    let (secret, pubkey) = identity(10);

    let (mut ws, nonce) = connect_control(&base).await;
    let sig = sign(&secret, &nonce, &pubkey, "Steady");
    let frame = serde_json::json!({
        "op": "register",
        "d": { "name": "Steady", "pubkey": pubkey, "signature": sig, "publish_public": true }
    });
    ws.send(Message::Text(frame.to_string())).await.unwrap();
    assert_eq!(next_json(&mut ws).await["op"], "registered");

    // Keep polling, which is what makes tungstenite answer the pings.
    let pump = tokio::spawn(async move { while ws.next().await.is_some() {} });
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert_eq!(
        registry.discover().len(),
        1,
        "a host answering heartbeats must stay listed"
    );

    // And its reported idle time should be small, so the UI shows it as live.
    assert!(registry.discover()[0].idle_secs < 1);
    pump.abort();
}
