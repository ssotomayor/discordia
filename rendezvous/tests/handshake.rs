use std::sync::Arc;
use std::time::Duration;

use dioxusfun_protocol::rendezvous::DiscoverEntry;
use dioxusfun_rendezvous::{AppCtx, Config, registry::Registry, router};
use futures_util::{SinkExt, StreamExt};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use sha2::{Digest, Sha256};
use tokio_tungstenite::tungstenite::Message;

async fn spawn() -> String {
    spawn_with(Config::default()).await.0
}

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
async fn resolve_answers_an_unlisted_host_by_code_and_nobody_else() {
    let (base, registry) = spawn_with(Config::default()).await;
    let (secret, pubkey) = identity(11);

    let (mut ws, nonce) = connect_control(&base).await;
    let sig = sign(&secret, &nonce, &pubkey, "Casa");
    send_register(&mut ws, "Casa", &pubkey, &sig).await;
    assert_eq!(next_json(&mut ws).await["op"], "registered");
    assert!(
        registry.discover().is_empty(),
        "unlisted hosts stay unlisted"
    );

    let http = base.replace("ws://", "http://");
    let fetched: DiscoverEntry = reqwest::get(format!("{http}/resolve/CASA"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched.shortcode, "casa");
    assert_eq!(fetched.name.as_deref(), Some("Casa"));
    assert!(fetched.transport_key.is_none());

    let missing = reqwest::get(format!("{http}/resolve/nobody-here-01"))
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_transport_key_is_published_only_when_it_is_signed_for() {
    let (base, registry) = spawn_with(Config::default()).await;
    let (secret, pubkey) = identity(13);
    let transport_key = "k51qzi5uqu5dl".to_string();

    let (mut ws, nonce) = connect_control(&base).await;
    let name_sig = sign(&secret, &nonce, &pubkey, "Keyed");
    let transport_sig = sign(&secret, &nonce, &pubkey, &transport_key);
    let frame = serde_json::json!({
        "op": "register",
        "d": {
            "name": "Keyed", "pubkey": pubkey, "signature": name_sig,
            "publish_public": false,
            "transport_key": transport_key,
            "transport_signature": transport_sig,
            "transport_addrs": ["203.0.113.5:41234", "192.168.0.61:41234"],
        }
    });
    ws.send(Message::Text(frame.to_string())).await.unwrap();
    assert_eq!(next_json(&mut ws).await["op"], "registered");

    let entry = registry.lookup("keyed").expect("host is live");
    assert_eq!(entry.transport_key.as_deref(), Some(transport_key.as_str()));
    assert_eq!(entry.transport_addrs.len(), 2);
}

#[tokio::test]
async fn an_unattested_transport_key_is_dropped_but_the_host_still_registers() {
    let (base, registry) = spawn_with(Config::default()).await;
    let (secret, pubkey) = identity(14);

    let (mut ws, nonce) = connect_control(&base).await;
    let name_sig = sign(&secret, &nonce, &pubkey, "Unsigned");
    let frame = serde_json::json!({
        "op": "register",
        "d": {
            "name": "Unsigned", "pubkey": pubkey, "signature": name_sig,
            "publish_public": false,
            "transport_key": "k51qzi5uqu5dl",
            "transport_signature": name_sig,
            "transport_addrs": ["203.0.113.5:41234"],
        }
    });
    ws.send(Message::Text(frame.to_string())).await.unwrap();
    assert_eq!(next_json(&mut ws).await["op"], "registered");

    let entry = registry.lookup("unsigned").expect("host is live");
    assert!(
        entry.transport_key.is_none(),
        "unattested key was published"
    );
    assert!(entry.transport_addrs.is_empty());
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
    assert_eq!(reply["d"]["shortcode"], "acme");
}

#[tokio::test]
async fn bad_signature_is_rejected() {
    let base = spawn().await;
    let (_secret, pubkey) = identity(2);
    let (mut ws, _nonce) = connect_control(&base).await;
    send_register(&mut ws, "acme", &pubkey, &"00".repeat(64)).await;
    let reply = next_json(&mut ws).await;
    assert_eq!(reply["op"], "error", "got {reply}");
}

#[tokio::test]
async fn a_second_key_cannot_take_a_claimed_name() {
    let base = spawn().await;
    let (secret_a, pubkey_a) = identity(3);
    let (secret_b, pubkey_b) = identity(4);

    let (mut ws_a, nonce_a) = connect_control(&base).await;
    let sig_a = sign(&secret_a, &nonce_a, &pubkey_a, "shared");
    send_register(&mut ws_a, "shared", &pubkey_a, &sig_a).await;
    assert_eq!(next_json(&mut ws_a).await["op"], "registered");

    let (mut ws_b, nonce_b) = connect_control(&base).await;
    let sig_b = sign(&secret_b, &nonce_b, &pubkey_b, "shared");
    send_register(&mut ws_b, "shared", &pubkey_b, &sig_b).await;
    let reply = next_json(&mut ws_b).await;
    assert_eq!(reply["op"], "error", "got {reply}");
    assert!(reply["d"]["message"].as_str().unwrap().contains("taken"));
}

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

    let _held = ws;
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;

    assert!(
        registry.discover().is_empty(),
        "a host that stopped answering should not still be advertised"
    );
}

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

    let pump = tokio::spawn(async move { while ws.next().await.is_some() {} });
    tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    assert_eq!(
        registry.discover().len(),
        1,
        "a host answering heartbeats must stay listed"
    );

    assert!(registry.discover()[0].idle_secs < 1);
    pump.abort();
}

async fn send_release(ws: &mut Ws, name: &str, pubkey: &str, signature: &str) -> serde_json::Value {
    let frame = serde_json::json!({
        "op": "release_name",
        "d": { "name": name, "pubkey": pubkey, "signature": signature }
    });
    ws.send(Message::Text(frame.to_string())).await.unwrap();
    next_json(ws).await
}

#[tokio::test]
async fn an_owner_can_release_a_name_and_another_key_can_then_take_it() {
    let (base, registry) = spawn_with(Config::default()).await;
    let (owner_secret, owner) = identity(21);
    let (other_secret, other) = identity(22);

    {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&owner_secret, &nonce, &owner, "Mudanza");
        send_register(&mut ws, "Mudanza", &owner, &sig).await;
        assert_eq!(next_json(&mut ws).await["op"], "registered");
    }
    for _ in 0..50 {
        if registry.reservation_owner("mudanza").is_some() && registry.lookup("mudanza").is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        registry.reservation_owner("mudanza").as_deref(),
        Some(&owner[..])
    );

    {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&other_secret, &nonce, &other, "Mudanza");
        send_register(&mut ws, "Mudanza", &other, &sig).await;
        assert_eq!(next_json(&mut ws).await["op"], "error");
    }

    {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&owner_secret, &nonce, &owner, "Mudanza");
        let reply = send_release(&mut ws, "Mudanza", &owner, &sig).await;
        assert_eq!(reply["op"], "released", "got {reply}");
        assert_eq!(reply["d"]["name"], "mudanza", "the slug, lowercased");
    }
    assert!(registry.reservation_owner("mudanza").is_none());

    {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&other_secret, &nonce, &other, "Mudanza");
        send_register(&mut ws, "Mudanza", &other, &sig).await;
        assert_eq!(next_json(&mut ws).await["op"], "registered");
    }
}

#[tokio::test]
async fn a_stranger_cannot_release_a_name_and_learns_nothing_from_trying() {
    let (base, registry) = spawn_with(Config::default()).await;
    let (owner_secret, owner) = identity(31);
    let (thief_secret, thief) = identity(32);

    {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&owner_secret, &nonce, &owner, "Fortaleza");
        send_register(&mut ws, "Fortaleza", &owner, &sig).await;
        assert_eq!(next_json(&mut ws).await["op"], "registered");
    }

    let taken = {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&thief_secret, &nonce, &thief, "Fortaleza");
        send_release(&mut ws, "Fortaleza", &thief, &sig).await
    };
    let free = {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&thief_secret, &nonce, &thief, "Vacante");
        send_release(&mut ws, "Vacante", &thief, &sig).await
    };

    assert_eq!(taken["op"], "error");
    let blank = |v: &serde_json::Value, slug: &str| {
        v["d"]["message"].as_str().unwrap().replace(slug, "<name>")
    };
    assert_eq!(
        blank(&taken, "fortaleza"),
        blank(&free, "vacante"),
        "a claimed name and a free one must answer a stranger identically"
    );
    assert_eq!(
        registry.reservation_owner("fortaleza").as_deref(),
        Some(&owner[..]),
        "the reservation survived the attempt"
    );
}

#[tokio::test]
async fn a_release_with_a_bad_signature_is_refused() {
    let (base, registry) = spawn_with(Config::default()).await;
    let (owner_secret, owner) = identity(41);

    {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&owner_secret, &nonce, &owner, "Sellada");
        send_register(&mut ws, "Sellada", &owner, &sig).await;
        assert_eq!(next_json(&mut ws).await["op"], "registered");
    }

    for _ in 0..50 {
        if registry.lookup("sellada").is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let (mut ws, _nonce) = connect_control(&base).await;
    let stale = sign(&owner_secret, "some-other-nonce", &owner, "Sellada");
    let reply = send_release(&mut ws, "Sellada", &owner, &stale).await;

    assert_eq!(reply["op"], "error");
    let message = reply["d"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("ownership rejected"),
        "the refusal has to be about the signature, not about something else \
         that happens to also refuse — got: {message}"
    );
    assert_eq!(
        registry.reservation_owner("sellada").as_deref(),
        Some(&owner[..])
    );
}

#[tokio::test]
async fn a_host_that_never_sends_register_is_cut_off() {
    let (base, _registry) = spawn_with(Config {
        register_timeout: Duration::from_millis(50),
        ..Config::default()
    })
    .await;
    let (mut ws, _nonce) = connect_control(&base).await;
    let reply = tokio::time::timeout(Duration::from_secs(2), next_json(&mut ws))
        .await
        .expect("the socket must be answered, not held open");
    assert_eq!(reply["op"], "error", "got {reply}");
    assert!(
        reply["d"]["message"].as_str().unwrap().contains("in time"),
        "got {reply}"
    );
}

#[tokio::test]
async fn an_oversized_control_frame_is_dropped_unread() {
    let base = spawn().await;
    let (mut ws, _nonce) = connect_control(&base).await;
    ws.send(Message::Text("x".repeat(65 * 1024))).await.unwrap();
    let next = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("the server must react to the frame");
    assert!(
        !matches!(next, Some(Ok(Message::Text(_)))),
        "an oversized frame was parsed and answered: {next:?}"
    );
}

#[tokio::test]
async fn resolve_is_throttled_per_address() {
    let base = spawn().await;
    let http = base.replace("ws://", "http://");
    let client = reqwest::Client::new();
    let url = format!("{http}/resolve/nobody-here-1234");
    for _ in 0..30 {
        let r = client.get(&url).send().await.unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::NOT_FOUND);
    }
    let r = client.get(&url).send().await.unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
}
