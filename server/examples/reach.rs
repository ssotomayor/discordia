//! Is this gateway reachable from *here*, and over which transport?
//!
//! A headless joiner. It does what the client's connect path does — dial, run
//! the WebSocket handshake, wait for the server's `Hello` — and then stops,
//! without a window, an identity, or a session. That makes it runnable from
//! anywhere a Rust toolchain is: a VPS, a container, another network.
//!
//! It exists because the interesting question about this branch cannot be asked
//! from the machine doing the hosting. "Is my port mapping real" and "does the
//! QUIC path work across a NAT" are questions about the *outside*, and the
//! answers a host can compute for itself are all subject to the same doubt —
//! `portmap`'s own hairpin probe reaches the router and comes back, which is
//! evidence but not proof.
//!
//! ```text
//! # plaintext, over the mapped TCP port
//! cargo run -p dioxusfun-server --example reach -- ws://203.0.113.5:9000
//!
//! # QUIC, by key. Addresses are hints; the key is the destination.
//! cargo run -p dioxusfun-server --example reach -- \
//!     --key <endpoint-id> 203.0.113.5:41234
//!
//! # and it can be the far end too, which is how the QUIC half is testable at
//! # all without a second GUI client — run this somewhere, dial it from
//! # somewhere else.
//! cargo run -p dioxusfun-server --example reach -- --listen
//! ```
//!
//! Build it with `LIVEKIT_BUNDLE_SKIP=1` — it never hosts voice, and without
//! that the build downloads an SFU it will not use.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use dioxusfun_server::quic::{Coordination, GATEWAY_ALPN, require_direct};
use futures_util::StreamExt;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, EndpointId, TransportAddr};
use tokio_tungstenite::tungstenite::Message;

const TIMEOUT: Duration = Duration::from_secs(15);

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--coordinated` may appear anywhere; pull it out before anything else
    // reads positionally.
    let coordination = if let Some(i) = args.iter().position(|a| a == "--coordinated") {
        args.remove(i);
        Coordination::CoordinatorOnly
    } else {
        Coordination::None
    };
    if args.is_empty() {
        eprintln!(
            "usage:\n\
             \x20 reach <ws://host:port>\n\
             \x20 reach --key <endpoint-id> [ip:port …] [--coordinated]\n\
             \x20 reach --listen [--coordinated]\n\n\
             --coordinated lets an iroh relay introduce the two ends (tier 2), and then\n\
             insists the result is a direct path — a connection the relay is still\n\
             carrying is refused rather than used."
        );
        std::process::exit(2);
    }

    if args[0] == "--listen" {
        listen(coordination).await;
        return;
    }

    let result = if args[0] == "--key" {
        // An address is required only without a coordinator. With one, being
        // findable by key alone is the whole point of the exercise.
        if args.len() < 2 {
            eprintln!("--key needs an endpoint id");
            std::process::exit(2);
        }
        if args.len() < 3 && coordination == Coordination::None {
            eprintln!(
                "--key needs at least one <ip:port> without --coordinated: with no relay to \
                 introduce you, an address is the only way to find the host"
            );
            std::process::exit(2);
        }
        reach_quic(&args[1], &args[2..], coordination).await
    } else {
        reach_ws(&args[0]).await
    };

    match result {
        Ok(report) => println!("\nREACHABLE — {report}"),
        Err(e) => {
            println!("\nNOT REACHABLE — {e}");
            std::process::exit(1);
        }
    }
}

/// Be the far end: serve a throwaway gateway over QUIC and print how to reach
/// it.
///
/// A diagnostic that can only ever be one half of a connection cannot be
/// checked without arranging the other half by hand, which is exactly the
/// problem this whole file is about. Two copies of this binary on two networks
/// are a complete test.
///
/// The data directory is temporary and the guilds in it are meaningless — the
/// question is whether packets arrive, and `Hello` is proof that they did.
async fn listen(coordination: Coordination) {
    let dir = std::env::temp_dir().join(format!("dioxusfun-reach-{}", std::process::id()));
    let router = dioxusfun_server::build_router(dioxusfun_server::ServerConfig {
        livekit: dioxusfun_server::livekit::LiveKitConfig::from_env(),
        operators: Default::default(),
        data_dir: dir,
    })
    .await
    .expect("build router");

    let endpoint = dioxusfun_server::quic::bind_quic(None, coordination)
        .await
        .expect("bind quic");

    // With a coordinator allowed, wait to reach a relay before printing
    // anything: until then there is no introduction on offer, and the only
    // addresses are ones this network can use.
    if coordination == Coordination::CoordinatorOnly {
        println!("reaching a relay …");
        let _ = tokio::time::timeout(Duration::from_secs(20), endpoint.online()).await;
        let relays: Vec<String> = endpoint
            .addr()
            .addrs
            .iter()
            .filter_map(|a| match a {
                TransportAddr::Relay(url) => Some(url.to_string()),
                _ => None,
            })
            .collect();
        if relays.is_empty() {
            println!("WARNING: no relay reached — no punch can be arranged from here");
        } else {
            println!("relay home: {}", relays.join(", "));
        }
    }

    let key = endpoint.id();
    let handle =
        dioxusfun_server::quic::serve_on_with(endpoint, router, coordination).expect("serve quic");
    println!("\nlistening as {key}\n");
    if coordination == Coordination::CoordinatorOnly {
        // No address: being findable by key alone is the thing under test.
        println!("  dial from anywhere:  reach --coordinated --key {key}");
    } else {
        for addr in dioxusfun_server::quic::dialable_addrs(&handle.sockets) {
            println!(
                "  reach --key {key} <this-machine's-address>:{}",
                addr.port()
            );
        }
    }
    println!("\n(bound to {:?})", handle.sockets);
    std::future::pending::<()>().await;
}

/// The plaintext path: an ordinary WebSocket to `/gateway`.
async fn reach_ws(url: &str) -> Result<String, String> {
    let url = if url.ends_with("/gateway") {
        url.to_string()
    } else {
        format!("{}/gateway", url.trim_end_matches('/'))
    };
    println!("dialling {url} …");
    let started = Instant::now();

    let (ws, _) = tokio::time::timeout(TIMEOUT, tokio_tungstenite::connect_async(&url))
        .await
        .map_err(|_| format!("no answer within {TIMEOUT:?} — the port is not open from here"))?
        .map_err(|e| format!("connect failed: {e}"))?;

    let nonce = first_hello(ws).await?;
    Ok(format!(
        "plaintext WebSocket, {}ms, server said hello (nonce {}…)",
        started.elapsed().as_millis(),
        &nonce[..8.min(nonce.len())]
    ))
}

/// The private path: QUIC, authenticated by the host's key.
async fn reach_quic(
    key: &str,
    addrs: &[String],
    coordination: Coordination,
) -> Result<String, String> {
    let endpoint_id: EndpointId = key
        .parse()
        .map_err(|e| format!("that is not an endpoint key: {e}"))?;
    let parsed: Vec<SocketAddr> = addrs.iter().filter_map(|a| a.parse().ok()).collect();
    // Coordinated, the key is enough: the relay knows where the host is, which
    // is the entire point. Uncoordinated, an address is all we have.
    if parsed.is_empty() && coordination == Coordination::None {
        return Err("no usable ip:port addresses given".into());
    }
    println!("dialling key {key} at {parsed:?} (coordination: {coordination:?}) …");
    let started = Instant::now();

    // Uncoordinated this has to prove the *address* works, so no relay and no
    // discovery: a connection quietly rescued by a third party would prove the
    // opposite of what is being asked. Coordinated is the other test entirely —
    // let the relay introduce us, then check what we actually got.
    let endpoint = match coordination {
        Coordination::None => Endpoint::builder(presets::Minimal),
        Coordination::CoordinatorOnly => Endpoint::builder(presets::N0),
    }
    .bind()
    .await
    .map_err(|e| format!("local quic bind: {e}"))?;
    let addr = EndpointAddr::new(endpoint_id).with_addrs(parsed.into_iter().map(TransportAddr::Ip));

    let conn = tokio::time::timeout(TIMEOUT, endpoint.connect(addr, GATEWAY_ALPN))
        .await
        .map_err(|_| {
            format!(
                "no answer within {TIMEOUT:?} — either the UDP port is not forwarded, or nothing \
                 is listening on it"
            )
        })?
        .map_err(|e| format!("quic connect: {e} (a key mismatch also lands here)"))?;

    // The tier-2 question, asked out loud: a relay that arranged this will carry
    // it just as happily, and it would work — so the refusal is the test.
    if let Err(refusal) = require_direct(&conn, coordination).await {
        return Err(format!(
            "REFUSED (correctly, if the punch failed): {refusal}"
        ));
    }

    let (send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| format!("quic stream: {e}"))?;
    let (ws, _) =
        tokio_tungstenite::client_async("ws://gateway.quic/gateway", tokio::io::join(recv, send))
            .await
            .map_err(|e| format!("websocket over quic: {e}"))?;

    let direct = conn.paths().iter().any(|p| p.is_ip() && p.is_selected());
    let nonce = first_hello(ws).await?;
    Ok(format!(
        "QUIC, {}ms, path is {}, server said hello (nonce {}…)",
        started.elapsed().as_millis(),
        if direct { "direct" } else { "NOT direct" },
        &nonce[..8.min(nonce.len())]
    ))
}

/// Read the gateway's opening frame. Getting this back means a real gateway is
/// on the other end, not merely an open socket.
async fn first_hello<S>(mut ws: tokio_tungstenite::WebSocketStream<S>) -> Result<String, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = tokio::time::timeout(TIMEOUT, ws.next())
        .await
        .map_err(|_| "connected, but the gateway never said hello".to_string())?
        .ok_or_else(|| "the connection closed before hello".to_string())?
        .map_err(|e| format!("recv: {e}"))?;
    let text = match frame {
        Message::Text(t) => t,
        other => return Err(format!("expected a text frame, got {other:?}")),
    };
    let parsed: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("unreadable frame: {e}"))?;
    if parsed["op"] != "hello" {
        return Err(format!("expected hello, got {text}"));
    }
    Ok(parsed["d"]["nonce"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}
