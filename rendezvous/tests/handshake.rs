//! End-to-end rendezvous name-claim handshake over a real WebSocket:
//! Challenge → signed Register → Registered, and the rejection paths (bad
//! signature, name already owned by a different key).

use std::sync::Arc;

use dioxusfun_protocol::rendezvous::DiscoverEntry;
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

/// An endpoint a host advertises has to survive the round trip to a friend who
/// only holds a join code — that is the whole path a direct connection depends
/// on, and it crosses two hops (`Register`, then `/resolve`) either of which
/// could quietly drop the field.
#[tokio::test]
async fn an_advertised_endpoint_reaches_a_friend_holding_the_code() {
    let (base, registry) = spawn_with(Config::default()).await;
    let (secret, pubkey) = identity(11);

    let (mut ws, nonce) = connect_control(&base).await;
    let sig = sign(&secret, &nonce, &pubkey, "Casa");
    let frame = serde_json::json!({
        "op": "register",
        "d": {
            "name": "Casa", "pubkey": pubkey, "signature": sig,
            // Unlisted on purpose: a code handed to friends is exactly the case
            // that wants the direct path, and it never appears in `/discover`.
            "publish_public": false,
            "endpoint": "ws://203.0.113.5:9000",
        }
    });
    ws.send(Message::Text(frame.to_string())).await.unwrap();
    assert_eq!(next_json(&mut ws).await["op"], "registered");

    let entry = registry.lookup("casa").expect("host is live");
    assert_eq!(entry.endpoint.as_deref(), Some("ws://203.0.113.5:9000"));

    // And over HTTP, which is how the client actually asks. The code is matched
    // case-insensitively, like `/join`.
    let http = base.replace("ws://", "http://");
    let fetched: DiscoverEntry = reqwest::get(format!("{http}/resolve/CASA"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched.endpoint.as_deref(), Some("ws://203.0.113.5:9000"));
    // Unlisted, so the browse listing must still not carry it.
    assert!(registry.discover().is_empty());
}

/// A host that obtained no address is not an error and not a special case: the
/// field is simply absent, and the joiner falls back to the relay.
#[tokio::test]
async fn a_host_without_an_endpoint_resolves_to_none() {
    let (base, _registry) = spawn_with(Config::default()).await;
    let (secret, pubkey) = identity(12);

    let (mut ws, nonce) = connect_control(&base).await;
    let sig = sign(&secret, &nonce, &pubkey, "Plain");
    send_register(&mut ws, "Plain", &pubkey, &sig).await;
    assert_eq!(next_json(&mut ws).await["op"], "registered");

    let http = base.replace("ws://", "http://");
    let fetched: DiscoverEntry = reqwest::get(format!("{http}/resolve/plain"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(fetched.endpoint.is_none());

    // An unknown code is a 404 rather than an empty entry, so a joiner can tell
    // "no direct address" from "no such host".
    let missing = reqwest::get(format!("{http}/resolve/nobody-here-01"))
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

/// A transport key is published only when the registering key signs for it.
///
/// This is what stops the QUIC path being pointed somewhere else: the transport
/// authenticates whoever holds the key it is given, so an unattested key would
/// move the trust problem rather than solve it — a joiner would faithfully
/// verify a connection to the wrong host.
#[tokio::test]
async fn a_transport_key_is_published_only_when_it_is_signed_for() {
    let (base, registry) = spawn_with(Config::default()).await;
    let (secret, pubkey) = identity(13);
    let transport_key = "k51qzi5uqu5dl".to_string();

    let (mut ws, nonce) = connect_control(&base).await;
    // Same nonce, same key, different payload — see `verify_ownership`.
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

/// And an unsigned — or wrongly signed — key is dropped rather than published,
/// while the host still registers.
///
/// Dropping rather than refusing on purpose: a host whose transport key cannot
/// be attested is still perfectly good over the relay, and locking it out would
/// turn a degraded path into no path at all.
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
            // Signed over the *name* rather than the key: a real signature from
            // the right identity, vouching for the wrong thing.
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

/// Send a release frame and return whatever the rendezvous answers.
async fn send_release(ws: &mut Ws, name: &str, pubkey: &str, signature: &str) -> serde_json::Value {
    let frame = serde_json::json!({
        "op": "release_name",
        "d": { "name": name, "pubkey": pubkey, "signature": signature }
    });
    ws.send(Message::Text(frame.to_string())).await.unwrap();
    next_json(ws).await
}

/// The owner can give a name back, and it is claimable by somebody else after.
///
/// Before this a reservation was permanent: `claim_name` persisted and nothing
/// removed one, so a name claimed by mistake was stuck for the life of the
/// relay's data directory.
#[tokio::test]
async fn an_owner_can_release_a_name_and_another_key_can_then_take_it() {
    let (base, registry) = spawn_with(Config::default()).await;
    let (owner_secret, owner) = identity(21);
    let (other_secret, other) = identity(22);

    // Claim, then drop the session so the name is reserved but not live.
    {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&owner_secret, &nonce, &owner, "Mudanza");
        send_register(&mut ws, "Mudanza", &owner, &sig).await;
        assert_eq!(next_json(&mut ws).await["op"], "registered");
    }
    // The host entry is dropped when the socket closes; wait for it.
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

    // Somebody else cannot take it while it is reserved.
    {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&other_secret, &nonce, &other, "Mudanza");
        send_register(&mut ws, "Mudanza", &other, &sig).await;
        assert_eq!(next_json(&mut ws).await["op"], "error");
    }

    // The owner releases it.
    {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&owner_secret, &nonce, &owner, "Mudanza");
        let reply = send_release(&mut ws, "Mudanza", &owner, &sig).await;
        assert_eq!(reply["op"], "released", "got {reply}");
        assert_eq!(reply["d"]["name"], "mudanza", "the slug, lowercased");
    }
    assert!(registry.reservation_owner("mudanza").is_none());

    // And now the other key can have it.
    {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&other_secret, &nonce, &other, "Mudanza");
        send_register(&mut ws, "Mudanza", &other, &sig).await;
        assert_eq!(next_json(&mut ws).await["op"], "registered");
    }
}

/// A stranger cannot release somebody else's name, and is told the same thing
/// they would be told about a name nobody has claimed — otherwise this becomes
/// an oracle for which names are taken by hosts that are offline, which
/// `/discover` deliberately does not answer.
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

    // A valid signature, by the wrong key.
    let taken = {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&thief_secret, &nonce, &thief, "Fortaleza");
        send_release(&mut ws, "Fortaleza", &thief, &sig).await
    };
    // And the same attempt against a name nobody ever claimed.
    let free = {
        let (mut ws, nonce) = connect_control(&base).await;
        let sig = sign(&thief_secret, &nonce, &thief, "Vacante");
        send_release(&mut ws, "Vacante", &thief, &sig).await
    };

    assert_eq!(taken["op"], "error");
    // Compared with the name blanked out: both messages name the slug that was
    // asked about, so a literal comparison would always differ and would be
    // asserting nothing. What must match is everything else — a stranger has
    // to be unable to tell a claimed name from a free one by the answer.
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

/// A forged signature is refused even when it names the real owner's key.
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

    // Wait for the registration to drop with its socket. Without this the
    // release below can be refused for being *live* rather than for its
    // signature, and an assertion that only checks "some error came back" is
    // satisfied by the wrong one — which is exactly how this test passed a
    // mutation run with the signature check disabled.
    for _ in 0..50 {
        if registry.lookup("sellada").is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let (mut ws, _nonce) = connect_control(&base).await;
    // Signed against a nonce that is not this connection's.
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
