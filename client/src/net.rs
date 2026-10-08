use std::sync::Arc;

use dioxusfun_server::watchdog::{ArmWatch, op_of, watchdog};

use dioxus::prelude::*;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use url::Url;

use crate::features::voice::VoiceCmd;
use crate::host::{HostHandle, start_self_host};
use crate::protocol::{ClientMessage, Id, ServerMessage};
use crate::state::{
    AppState, ConnectionStatus, GatewayTx, SessionMode, SessionParams, Transport, VoicePhase,
};

fn solve_pow(challenge: &str, bits: u32) -> String {
    use sha2::{Digest, Sha256};
    let mut n: u64 = 0;
    loop {
        let nonce = n.to_string();
        let mut h = Sha256::new();
        h.update(challenge.as_bytes());
        h.update(nonce.as_bytes());
        let digest = h.finalize();
        let mut seen = 0u32;
        for byte in digest {
            if byte == 0 {
                seen += 8;
                continue;
            }
            seen += byte.leading_zeros();
            break;
        }
        if seen >= bits {
            return nonce;
        }
        n += 1;
    }
}

fn normalize_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("server URL is required".into());
    }
    let with_scheme = if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        trimmed.to_string()
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        format!("ws://{trimmed}")
    };
    let mut url = Url::parse(&with_scheme).map_err(|e| format!("invalid URL: {e}"))?;
    url.set_path("/gateway");
    Ok(url.to_string())
}

pub fn spawn_gateway(
    params: SessionParams,
    mut state: Signal<AppState>,
    voice_tx: UnboundedSender<VoiceCmd>,
    on_disconnect: impl FnOnce(String) + 'static,
) -> (GatewayTx, GatewayShutdown) {
    let (tx, rx) = unbounded_channel::<ClientMessage>();
    let gateway_tx = GatewayTx(tx.clone());
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (done_tx, done_rx) = tokio::sync::watch::channel(false);

    spawn(async move {
        let reason = match run(params, &tx, rx, state, &voice_tx, shutdown_rx).await {
            Ok(()) => "connection closed".to_string(),
            Err(e) => e,
        };
        tracing::warn!(%reason, transport = ?state.peek().transport, "gateway session ended");
        state.write().status = ConnectionStatus::Disconnected;
        let _ = voice_tx.send(VoiceCmd::Disconnect { done: None });
        on_disconnect(reason);
        done_tx.send_replace(true);
    });

    (
        gateway_tx,
        GatewayShutdown {
            request: shutdown_tx,
            done: done_rx,
        },
    )
}

#[derive(Clone)]
pub struct GatewayShutdown {
    request: tokio::sync::watch::Sender<bool>,
    done: tokio::sync::watch::Receiver<bool>,
}

impl GatewayShutdown {
    pub async fn close(&self) {
        self.request.send_replace(true);
        let mut done = self.done.clone();
        if done.wait_for(|closed| *closed).await.is_err() {
            tracing::debug!("gateway task already gone");
        }
    }
}

/// Off this machine every connection is QUIC — encrypted end to end and
/// authenticated by the key in the share string or the directory entry. A
/// plain socket is allowed only to loopback, or over TLS through a proxy.
#[derive(Debug)]
enum Dial {
    Socket {
        url: String,
        origin: String,
        transport: Transport,
    },
    Quic {
        key: String,
        addrs: Vec<String>,
    },
}

fn origin_of(url: &str) -> Result<String, String> {
    crate::protocol::dial_origin(url).ok_or_else(|| format!("{url} has no host"))
}

/// `ws://` off loopback is refused rather than dialed: every hop on the path
/// could read it, and the host has a share string to give instead.
fn parse_target(raw: &str) -> Result<Dial, String> {
    if let Some(share) = crate::protocol::parse_quic_share(raw) {
        return Ok(Dial::Quic {
            key: share.key,
            addrs: share.addrs,
        });
    }
    let url = normalize_url(raw)?;
    let parsed = Url::parse(&url).map_err(|e| format!("invalid URL: {e}"))?;
    let loopback = match parsed.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    let transport = match (parsed.scheme(), loopback) {
        ("ws", true) => Transport::Loopback,
        ("wss", _) => Transport::Proxied,
        ("ws", false) => {
            return Err(format!(
                "{} would travel in the clear, readable by everyone on the way. Ask the host \
                 for their quic:// address, or use wss:// through a TLS proxy.",
                raw.trim()
            ));
        }
        (other, _) => return Err(format!("unsupported scheme {other}://")),
    };
    let origin = origin_of(&url)?;
    Ok(Dial::Socket {
        url,
        origin,
        transport,
    })
}

async fn resolve_session(
    mode: SessionMode,
    identity: crate::identity::Identity,
    state: &mut Signal<AppState>,
) -> Result<(Dial, Option<HostHandle>), String> {
    match mode {
        SessionMode::Remote { server_url } => Ok((parse_target(&server_url)?, None)),
        SessionMode::SelfHost {
            allow_lan,
            manual_ip,
            rendezvous_url,
            publish_name,
            description,
            publish_public,
            location,
        } => {
            let publish = crate::rendezvous::PublishOptions {
                publish_name,
                description,
                publish_public,
                location,
            };
            state.write().rendezvous_url = rendezvous_url.clone();
            let handle =
                start_self_host(allow_lan, manual_ip, rendezvous_url, publish, identity).await?;
            let url = normalize_url(&handle.info.local_url)?;
            let origin = origin_of(&url)?;
            state.write().host_info = Some(handle.info.clone());
            Ok((
                Dial::Socket {
                    url,
                    origin,
                    transport: Transport::Loopback,
                },
                Some(handle),
            ))
        }
        SessionMode::ByCode {
            rendezvous_url,
            code,
        } => {
            let base = rendezvous_url.trim().trim_end_matches('/');
            if base.is_empty() {
                return Err("rendezvous URL required".into());
            }
            let with_scheme = ws_scheme(base);
            state.write().rendezvous_url = Some(base.to_string());
            let code = code.trim();
            let Some(entry) = resolve_host(&with_scheme, code).await else {
                return Err(format!("no host answers to '{code}' at {base}"));
            };
            let Some(key) = entry.transport_key else {
                return Err(
                    "that host offers no encrypted path; it needs a newer Discordia".into(),
                );
            };
            let mut addrs = entry.transport_addrs;
            if let Some(relay) = entry.relay_url
                && !addrs.contains(&relay)
            {
                addrs.push(relay);
            }
            Ok((Dial::Quic { key, addrs }, None))
        }
    }
}

fn ws_scheme(base: &str) -> String {
    if base.starts_with("ws://") || base.starts_with("wss://") {
        base.to_string()
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else {
        format!("ws://{base}")
    }
}

async fn resolve_host(
    ws_base: &str,
    code: &str,
) -> Option<crate::protocol::rendezvous::DiscoverEntry> {
    let http_base = if let Some(rest) = ws_base.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = ws_base.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        ws_base.to_string()
    };
    reqwest::Client::new()
        .get(format!("{http_base}/resolve/{code}"))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json::<crate::protocol::rendezvous::DiscoverEntry>()
        .await
        .ok()
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Long enough for a hole punch through a relay; a LAN answer takes a moment.
const QUIC_ATTEMPT: std::time::Duration = std::time::Duration::from_secs(15);

const QUIC_HANDSHAKE_URL: &str = "ws://127.0.0.1/gateway";

async fn dial_quic(key: &str, addrs: &[String]) -> Result<(Socket, bool), String> {
    let endpoint_id = crate::quic::parse_endpoint_id(key)?;
    let addrs: Vec<_> = addrs
        .iter()
        .filter_map(|a| crate::quic::parse_transport_addr(a))
        .collect();
    let coordination = crate::quic::coordination_from(&addrs);
    let (io, guard) = crate::quic::dial(endpoint_id, &addrs, &coordination).await?;
    let (ws, _) = tokio_tungstenite::client_async(QUIC_HANDSHAKE_URL, io)
        .await
        .map_err(|e| format!("websocket over quic: {e}"))?;
    let relayed = guard.relayed();
    Ok((Socket::Quic(Box::new(ws), guard), relayed))
}

async fn run(
    params: SessionParams,
    tx: &UnboundedSender<ClientMessage>,
    mut rx: UnboundedReceiver<ClientMessage>,
    mut state: Signal<AppState>,
    voice_tx: &UnboundedSender<VoiceCmd>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    let (dial, mut host_handle) =
        resolve_session(params.mode.clone(), params.identity.clone(), &mut state).await?;
    let mut host_updates = host_handle.as_mut().and_then(|h| h.updates.take());
    let mut failures = 0u32;
    loop {
        let connected = tokio::select! {
            _ = shutdown.wait_for(|requested| *requested) => return Ok(()),
            connected = connect_gateway(&dial) => connected,
        };
        let outcome = match connected {
            Ok((socket, transport, origin)) => {
                {
                    let mut s = state.write();
                    s.transport = transport;
                    s.server_origin = Some(origin.clone());
                }
                let links = Links {
                    tx,
                    rx: &mut rx,
                    voice_tx,
                    host_updates: &mut host_updates,
                    shutdown: shutdown.clone(),
                };
                match socket {
                    Socket::Tcp(ws) => run_session(*ws, params.clone(), origin, state, links).await,
                    Socket::Quic(ws, guard) => {
                        let outcome = run_session(*ws, params.clone(), origin, state, links).await;
                        if outcome.is_err() {
                            tracing::warn!(close_reason = ?guard.close_reason(), relayed = guard.relayed(), "gateway QUIC session failed");
                        }
                        if tokio::time::timeout(std::time::Duration::from_secs(1), guard.shutdown())
                            .await
                            .is_err()
                        {
                            tracing::warn!("gateway QUIC endpoint shutdown timed out");
                        }
                        outcome
                    }
                }
            }
            Err(error) => Err(error),
        };
        if *shutdown.borrow() {
            return Ok(());
        }
        if state.peek().status == ConnectionStatus::Connecting {
            return outcome;
        }
        if state.peek().status == ConnectionStatus::Ready {
            failures = 0;
        }
        let reason = outcome.err().unwrap_or_else(|| "connection closed".into());
        state.write().status = ConnectionStatus::Reconnecting;
        tracing::warn!(%reason, "gateway lost; reconnecting in place");
        let delay = reconnect_delay(failures);
        failures = failures.saturating_add(1);
        let until = tokio::time::Instant::now() + delay;
        loop {
            tokio::select! {
                _ = shutdown.wait_for(|requested| *requested) => return Ok(()),
                _ = tokio::time::sleep_until(until) => break,
                command = rx.recv() => {
                    let Some(command) = command else { return Ok(()); };
                    if matches!(command, ClientMessage::SendMessage { .. }) {
                        state.write().error_toast = Some("Not sent: the server is reconnecting. Please try again.".into());
                    }
                }
                update = async {
                    match host_updates.as_mut() {
                        Some(updates) => updates.recv().await,
                        None => std::future::pending().await,
                    }
                } => match update {
                    Some(update) => apply_host_update(&mut state.write(), update),
                    None => host_updates = None,
                }
            }
        }
        while rx.try_recv().is_ok() {}
    }
}

fn reconnect_delay(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs((1u64 << failures.min(4)).min(15))
}

async fn connect_gateway(dial: &Dial) -> Result<(Socket, Transport, String), String> {
    match dial {
        Dial::Socket {
            url,
            origin,
            transport,
        } => {
            let (ws, _) = tokio::time::timeout(QUIC_ATTEMPT, tokio_tungstenite::connect_async(url))
                .await
                .map_err(|_| "gateway connection timed out".to_string())?
                .map_err(|e| format!("connect failed: {e}"))?;
            Ok((Socket::Tcp(Box::new(ws)), *transport, origin.clone()))
        }
        Dial::Quic { key, addrs } => {
            let (socket, relayed) = tokio::time::timeout(QUIC_ATTEMPT, dial_quic(key, addrs))
                .await
                .map_err(|_| format!("no answer from the host within {QUIC_ATTEMPT:?}"))??;
            Ok((
                socket,
                if relayed {
                    Transport::QuicRelayed
                } else {
                    Transport::Quic
                },
                crate::protocol::quic_origin(key),
            ))
        }
    }
}

fn apply_host_update(s: &mut AppState, update: crate::rendezvous::HostUpdate) {
    use crate::rendezvous::HostUpdate;
    let Some(info) = s.host_info.as_mut() else {
        return;
    };
    match update {
        HostUpdate::RendezvousLost { error } => {
            info.publish_error = Some(format!("rendezvous link lost, retrying: {error}"));
        }
        HostUpdate::RendezvousRestored { shortcode } => {
            info.publish_error = None;
            info.shortcode = Some(shortcode);
        }
    }
}

enum Socket {
    Tcp(Box<Ws>),
    Quic(
        Box<tokio_tungstenite::WebSocketStream<crate::quic::GatewayIo>>,
        crate::quic::ConnectionGuard,
    ),
}

/// The channels a session talks over, apart from the socket itself.
struct Links<'a> {
    tx: &'a UnboundedSender<ClientMessage>,
    rx: &'a mut UnboundedReceiver<ClientMessage>,
    voice_tx: &'a UnboundedSender<VoiceCmd>,
    host_updates: &'a mut Option<UnboundedReceiver<crate::rendezvous::HostUpdate>>,
    shutdown: tokio::sync::watch::Receiver<bool>,
}

struct SessionWatchdog(tokio::task::JoinHandle<()>);

impl Drop for SessionWatchdog {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn run_session<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    params: SessionParams,
    origin: String,
    mut state: Signal<AppState>,
    links: Links<'_>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Links {
        tx,
        rx,
        voice_tx,
        host_updates,
        mut shutdown,
    } = links;
    state.write().identity = Some(params.identity.clone());
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    let nonce = tokio::select! {
        _ = shutdown.wait_for(|requested| *requested) => {
            close_gateway_socket(&mut ws_tx, &mut ws_rx).await;
            return Ok(());
        }
        nonce = tokio::time::timeout(crate::protocol::GATEWAY_HEARTBEAT_TIMEOUT, async {
        loop {
            let Some(frame) = ws_rx.next().await else {
                return Err("server closed before Hello".into());
            };
            let frame = frame.map_err(|e| format!("recv: {e}"))?;
            let text = match frame {
                WsMessage::Text(t) => t.to_string(),
                WsMessage::Close(_) => return Err("server closed before Hello".into()),
                _ => continue,
            };
            let parsed: ServerMessage = serde_json::from_str(&text)
                .map_err(|e| format!("bad server frame before Hello: {e}"))?;
            match parsed {
                ServerMessage::Hello { nonce } => break Ok::<_, String>(nonce),
                other => return Err(format!("expected Hello, got {other:?}")),
            }
        }
        }) => nonce.map_err(|_| "server did not send Hello before the deadline".to_string())??,
    };

    let username = crate::protocol::canonical_username(&params.identity.display_name);
    let pubkey = params.identity.pubkey.clone();
    let to_sign = crate::protocol::identify_payload(&nonce, &origin, &pubkey, &username);
    let signature = params.identity.sign_hex(&to_sign);

    let identify = ClientMessage::Identify {
        username,
        pubkey,
        signature,
        origin,
        bot: false,
        client_version: crate::version::VERSION.to_string(),
    };
    let json = serde_json::to_string(&identify).map_err(|e| e.to_string())?;
    gateway_send(&mut ws_tx, WsMessage::Text(json)).await?;

    // A link saved by an older build would be refused now and take the whole
    // profile with it, so it is dropped here rather than sent.
    let picture =
        |v: Option<String>| v.filter(|p| !p.starts_with("http://") && !p.starts_with("https://"));
    if let Some(local) = crate::profile::load()
        && (local.avatar.is_some()
            || local.banner.is_some()
            || local.bio.is_some()
            || local.status.is_some()
            || local.custom_status.is_some())
    {
        let set_profile = ClientMessage::SetProfile {
            avatar: picture(local.avatar),
            banner: picture(local.banner),
            bio: local.bio,
            status: local.status,
            custom_status: local.custom_status,
        };
        if let Ok(json) = serde_json::to_string(&set_profile) {
            gateway_send(&mut ws_tx, WsMessage::Text(json)).await?;
        }
    }

    let watch = Arc::new(ArmWatch::default());
    let _dog = SessionWatchdog(watchdog(watch.clone(), "session".into()));
    let mut media_tick = tokio::time::interval(MEDIA_TICK);
    let mut heartbeat = tokio::time::interval(crate::protocol::GATEWAY_HEARTBEAT_INTERVAL);
    let mut last_received = tokio::time::Instant::now();
    let ready_deadline = last_received + crate::protocol::GATEWAY_HEARTBEAT_TIMEOUT;
    let mut ready = false;
    let media_updates = state.peek().emoji_images.updates();
    loop {
        watch.finish("session");
        tokio::select! {
            _ = shutdown.wait_for(|requested| *requested) => {
                close_gateway_socket(&mut ws_tx, &mut ws_rx).await;
                break;
            }
            _ = heartbeat.tick() => {
                if !ready && tokio::time::Instant::now() >= ready_deadline {
                    return Err("the host did not accept the session before the deadline".into());
                }
                if last_received.elapsed() >= crate::protocol::GATEWAY_HEARTBEAT_TIMEOUT {
                    return Err("the host stopped answering heartbeat probes".into());
                }
                tokio::time::timeout(crate::protocol::GATEWAY_HEARTBEAT_INTERVAL,
                    ws_tx.send(WsMessage::Ping(Vec::new()))).await
                    .map_err(|_| "gateway heartbeat send timed out".to_string())?
                    .map_err(|e| format!("gateway heartbeat: {e}"))?;
            }
            update = async {
                match host_updates.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                watch.begin("host update");
                match update {
                    Some(u) => apply_host_update(&mut state.write(), u),
                    None => *host_updates = None,
                }
            }
            outbound = rx.recv() => {
                let Some(msg) = outbound else { break };
                if !ready {
                    state.write().error_toast = Some("The server is reconnecting. Please try this action again once connected.".into());
                    continue;
                }
                let json = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
                match &msg {
                    ClientMessage::JoinVoice { channel_id, .. } => {
                        state.write().voice.joining_channel_id = Some(*channel_id);
                    }
                    ClientMessage::LeaveVoice => {
                        state.write().voice.joining_channel_id = None;
                    }
                    _ => {}
                }
                tracing::debug!(op = op_of(&json), "→ gateway");
                watch.begin(format!("send {}", op_of(&json)));
                gateway_send(&mut ws_tx, WsMessage::Text(json)).await?;
            }
            _ = media_tick.tick() => {
                watch.begin("media tick");
                if state.peek().emoji_images.has_work()
                    || state.peek().emoji_requested.values().any(|asked| asked.elapsed() >= MEDIA_RETRY_AFTER)
                {
                    resolve_media(&mut state.write(), tx);
                }
            }
            _ = media_updates.ready.notified() => {
                watch.begin("media requested");
                if state.peek().emoji_images.has_work() {
                    resolve_media(&mut state.write(), tx);
                }
            }
            inbound = ws_rx.next() => {
                let Some(frame) = inbound else { break };
                let frame = frame.map_err(|e| format!("recv: {e}"))?;
                last_received = tokio::time::Instant::now();
                let text = match frame {
                    WsMessage::Text(t) => t.to_string(),
                    WsMessage::Ping(payload) => {
                        gateway_send(&mut ws_tx, WsMessage::Pong(payload)).await?;
                        continue;
                    }
                    // A host that stops on purpose says so in the close frame;
                    // "connection closed" would be true and useless.
                    WsMessage::Close(Some(f)) if !f.reason.is_empty() => {
                        return Err(f.reason.to_string());
                    }
                    WsMessage::Close(_) => break,
                    _ => continue,
                };
                tracing::debug!(op = op_of(&text), "← gateway");
                watch.begin(format!("recv {}", op_of(&text)));
                let parsed: ServerMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(err = %e, "bad server frame");
                        continue;
                    }
                };
                if matches!(parsed, ServerMessage::Ready { .. }) {
                    while rx.try_recv().is_ok() {}
                    ready = true;
                }
                apply(&mut state, parsed, tx, voice_tx);
            }
        }
    }
    watch.finish("session");
    tracing::info!("session loop ended");

    Ok(())
}

async fn close_gateway_socket<T, R>(tx: &mut T, rx: &mut R)
where
    T: futures_util::Sink<WsMessage> + Unpin,
    T::Error: std::fmt::Display,
    R: futures_util::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    let close = async {
        if let Err(error) = tx.send(WsMessage::Close(None)).await {
            tracing::debug!(%error, "gateway already closed during teardown");
            return;
        }
        while let Some(Ok(frame)) = rx.next().await {
            if matches!(frame, WsMessage::Close(_)) {
                break;
            }
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(2), close)
        .await
        .is_err()
    {
        tracing::warn!("gateway close handshake timed out");
    }
}

#[cfg(test)]
mod graceful_shutdown_tests {
    use super::*;
    use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

    #[tokio::test]
    async fn quitting_cancels_both_a_pending_connection_and_reconnect_backoff() {
        for keep_listener in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("listen");
            let address = listener.local_addr().expect("address");
            let _listener = keep_listener.then_some(listener);
            let mut dom = VirtualDom::new(|| rsx! {});
            dom.rebuild_in_place();
            let state = dom.in_scope(ScopeId::ROOT, || {
                let mut initial = AppState::empty();
                initial.status = ConnectionStatus::Reconnecting;
                Signal::new(initial)
            });
            let params = SessionParams {
                mode: SessionMode::Remote {
                    server_url: format!("ws://{address}"),
                },
                identity: crate::identity::Identity::restore_from_private_key(
                    "11".repeat(32),
                    "Alice",
                )
                .expect("identity"),
            };
            let (tx, rx) = unbounded_channel();
            let (voice_tx, _voice_rx) = unbounded_channel();
            let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
            let mut running = Box::pin(run(params, &tx, rx, state, &voice_tx, shutdown_rx));
            let session = std::future::poll_fn(|cx| {
                dom.in_scope(ScopeId::ROOT, || running.as_mut().poll(cx))
            });
            let quit = async {
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                shutdown_tx.send_replace(true);
            };
            let (outcome, ()) = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                tokio::join!(session, quit)
            })
            .await
            .expect("quit interrupts connection/retry");
            assert!(outcome.is_ok());
        }
    }

    #[tokio::test]
    async fn closing_gateway_sends_a_close_frame_and_waits_for_the_peer() {
        let (client, server) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
        let mut server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        let (mut tx, mut rx) = client.split();
        let peer = async {
            assert!(matches!(
                server.next().await,
                Some(Ok(WsMessage::Close(None)))
            ));
            server.flush().await.expect("close acknowledgement");
        };
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(close_gateway_socket(&mut tx, &mut rx), peer);
        })
        .await
        .expect("close handshake completes");
    }

    #[tokio::test]
    async fn an_unresponsive_gateway_cannot_block_exit_forever() {
        let (client, _server) = tokio::io::duplex(4096);
        let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
        let (mut tx, mut rx) = client.split();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            close_gateway_socket(&mut tx, &mut rx),
        )
        .await
        .expect("exit is bounded");
    }
}

async fn gateway_send<S>(sink: &mut S, frame: WsMessage) -> Result<(), String>
where
    S: futures_util::Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    tokio::time::timeout(
        crate::protocol::GATEWAY_HEARTBEAT_INTERVAL,
        sink.send(frame),
    )
    .await
    .map_err(|_| "gateway send timed out".to_string())?
    .map_err(|error| format!("gateway send: {error}"))
}

/// Stable, so roles written before positions were kept apart stay in the
/// order the server sent them.
fn by_position(mut roles: Vec<crate::protocol::Role>) -> Vec<crate::protocol::Role> {
    roles.sort_by_key(|r| r.position);
    roles
}

fn emoji_addresses(s: &AppState) -> Vec<String> {
    s.guild_emojis
        .values()
        .flatten()
        .map(|e| e.image.clone())
        .collect()
}

/// Only the open board's guild: a library can be dozens of files, and a
/// listener never needs them — what they hear arrives over the SFU.
fn sound_addresses(s: &AppState) -> Vec<String> {
    let Some(guild) = s.voice_guild().filter(|_| s.soundboard_open) else {
        return Vec::new();
    };
    s.sounds_of(guild)
        .iter()
        .filter_map(|sound| media_address(&sound.audio))
        .collect()
}

fn media_address(raw: &str) -> Option<String> {
    raw.strip_prefix("media:").map(str::to_string)
}

/// Asked-for and not yet answered counts as in flight for this long; after
/// it the address is asked for again. Covers a throttled request, an answer
/// the server cut for size, and a frame lost to a reconnect.
const MEDIA_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(8);
const MEDIA_TICK: std::time::Duration = std::time::Duration::from_secs(3);

pub(crate) fn resolve_media(s: &mut AppState, tx: &UnboundedSender<ClientMessage>) {
    let now = std::time::Instant::now();
    let (loaded, mut wanted) = s.emoji_images.take_work();
    for (address, data) in loaded {
        s.emoji_requested.remove(&address);
        s.emoji_images.insert(address, data);
    }
    wanted.extend(
        s.emoji_requested
            .iter()
            .filter(|(_, asked)| now.duration_since(**asked) >= MEDIA_RETRY_AFTER)
            .map(|(address, _)| address.clone()),
    );
    wanted.sort_unstable();
    wanted.dedup();
    wanted.retain(|address| {
        !s.emoji_images.contains_key(address)
            && !s
                .emoji_requested
                .get(address)
                .is_some_and(|asked| now.duration_since(*asked) < MEDIA_RETRY_AFTER)
    });
    for address in &wanted {
        s.emoji_requested.insert(address.clone(), now);
    }
    for chunk in wanted.chunks(8) {
        crate::media_cache::load(chunk.to_vec(), tx, &s.emoji_images);
    }
}

fn apply(
    state: &mut Signal<AppState>,
    msg: ServerMessage,
    tx: &UnboundedSender<ClientMessage>,
    voice_tx: &UnboundedSender<VoiceCmd>,
) {
    let mut s = state.write();
    match msg {
        ServerMessage::Ready {
            user,
            guilds,
            channels,
            members,
            voice_states,
            catalog,
            profiles,
            roles,
            emojis,
            sounds,
            activities,
            bot_commands,
            operator,
        } => {
            let recovering = s.status == ConnectionStatus::Reconnecting;
            let selected_guild = s.selected_guild;
            let selected_channel = s.selected_channel;
            let selected_dm =
                recovering && s.dm_mode && selected_channel.is_some_and(|id| s.dm_of(id).is_some());
            let local_channel = s.voice.channel_id;
            s.self_user = Some(user);
            s.is_operator = operator;
            s.guilds = guilds;
            s.channels = channels;
            s.members = members;
            s.voice_states = voice_states;
            if !recovering {
                s.dm_mode = false;
            }
            s.catalog = catalog;
            s.profiles = profiles
                .into_iter()
                .map(|p| (p.pubkey.clone(), p))
                .collect();
            s.activities = activities
                .into_iter()
                .filter_map(|u| u.activity.map(|a| (u.pubkey, a)))
                .collect();
            s.roles = {
                let mut map: std::collections::HashMap<Id, Vec<crate::protocol::Role>> =
                    std::collections::HashMap::new();
                for role in roles {
                    map.entry(role.guild_id).or_default().push(role);
                }
                for list in map.values_mut() {
                    list.sort_by_key(|r| r.position);
                }
                map
            };
            s.guild_emojis = {
                let mut map: std::collections::HashMap<Id, Vec<crate::protocol::GuildEmoji>> =
                    std::collections::HashMap::new();
                for e in emojis {
                    map.entry(e.guild_id).or_default().push(e);
                }
                map
            };
            s.guild_sounds = {
                let mut map: std::collections::HashMap<Id, Vec<crate::protocol::GuildSound>> =
                    std::collections::HashMap::new();
                for sound in sounds {
                    map.entry(sound.guild_id).or_default().push(sound);
                }
                map
            };
            s.bot_commands = bot_commands
                .into_iter()
                .map(|set| (set.bot_pubkey, set.commands))
                .collect();
            s.command_notes.clear();
            let dm_channels: std::collections::HashSet<_> =
                s.dms.iter().map(|dm| dm.channel_id).collect();
            s.messages.retain(|id, _| dm_channels.contains(id));
            // A request the last session never got an answer to would
            // otherwise stay "in flight" forever.
            s.emoji_requested.clear();
            resolve_media(&mut s, tx);
            let mut shares = std::collections::HashMap::<Id, Vec<String>>::new();
            for vs in &s.voice_states {
                if vs.screen_sharing
                    && let Some(channel) = vs.channel_id
                {
                    shares
                        .entry(channel)
                        .or_default()
                        .push(vs.user_pubkey.clone());
                }
            }
            s.screen_shares = shares;
            if !recovering {
                s.screen_viewing.clear();
            }
            let server_channel = s.server_voice_channel();
            if recovering
                && let Some(channel) = local_channel
                && server_channel != Some(channel)
            {
                let guild = s
                    .channels
                    .iter()
                    .find(|c| c.id == channel)
                    .map(|c| c.guild_id);
                let _ = voice_tx.send(VoiceCmd::Disconnect { done: None });
                s.end_voice_locally();
                if guild.is_some() && server_channel.is_none() {
                    let _ = tx.send(s.voice.join_message(channel));
                }
            }
            s.status = ConnectionStatus::Ready;

            let chosen = selected_guild
                .filter(|id| recovering && s.guilds.iter().any(|g| g.id == *id))
                .or_else(|| s.guilds.first().map(|g| g.id));
            if selected_dm {
                s.selected_guild = chosen;
                s.selected_channel = selected_channel;
            } else if let Some(first) = chosen {
                s.selected_guild = Some(first);
                let chan = selected_channel
                    .filter(|id| {
                        recovering
                            && s.channels
                                .iter()
                                .any(|c| c.id == *id && c.guild_id == first)
                    })
                    .or_else(|| s.default_channel_of(first));
                s.selected_channel = chan;
                if let Some(channel_id) = chan {
                    let _ = tx.send(ClientMessage::FetchMessages {
                        channel_id,
                        limit: 50,
                        before_ms: None,
                    });
                }
            }
        }
        ServerMessage::MessageHistory {
            channel_id,
            messages,
        } => {
            s.merge_history(channel_id, messages);
            resolve_media(&mut s, tx);
        }
        ServerMessage::MessageCreate(m) => {
            let cid = m.channel_id;
            let is_dm = !s.channels.iter().any(|c| c.id == cid);
            if is_dm && s.dm_of(cid).is_none() {
                s.dms.push(crate::state::DmInfo {
                    channel_id: cid,
                    other_pubkey: m.author.pubkey.clone(),
                });
            }
            let author_is_self = s
                .self_user
                .as_ref()
                .map(|u| u.pubkey == m.author.pubkey)
                .unwrap_or(false);
            let viewing = s.selected_channel == Some(cid) && (is_dm == s.dm_mode);
            if let Some(set) = s.typing.get_mut(&cid) {
                set.remove(&m.author.pubkey);
            }
            let has_image = m.image.is_some();
            s.messages.entry(cid).or_default().push(m);
            if is_dm && !author_is_self && !viewing {
                *s.dm_unread.entry(cid).or_insert(0) += 1;
            }
            if s.should_ring(cid, author_is_self, viewing) {
                if is_dm {
                    s.dm_notify_tick = s.dm_notify_tick.wrapping_add(1);
                } else {
                    s.notify_tick = s.notify_tick.wrapping_add(1);
                }
            }
            if has_image {
                resolve_media(&mut s, tx);
            }
        }
        ServerMessage::GuildJoined {
            guild,
            channels,
            members,
            roles,
            emojis,
            sounds,
            voice_states,
        } => {
            let gid = guild.id;
            if !s.guilds.iter().any(|g| g.id == gid) {
                s.guilds.push(guild);
            }
            s.roles.insert(gid, by_position(roles));
            s.guild_emojis.insert(gid, emojis);
            s.guild_sounds.insert(gid, sounds);
            resolve_media(&mut s, tx);
            s.voice_states.retain(|v| v.guild_id != gid);
            s.voice_states.extend(voice_states);
            for ch in channels {
                if !s.channels.iter().any(|c| c.id == ch.id) {
                    s.channels.push(ch);
                }
            }
            for m in members {
                let existing = s
                    .members
                    .iter_mut()
                    .find(|x| x.guild_id == m.guild_id && x.user.pubkey == m.user.pubkey);
                match existing {
                    Some(slot) => *slot = m,
                    None => s.members.push(m),
                }
            }
            s.dm_mode = false;
            s.selected_guild = Some(gid);
            let first_text = s.default_channel_of(gid);
            s.selected_channel = first_text;
            if let Some(channel_id) = first_text
                && !s.messages.contains_key(&channel_id)
            {
                let _ = tx.send(ClientMessage::FetchMessages {
                    channel_id,
                    limit: 50,
                    before_ms: None,
                });
            }
        }
        ServerMessage::GuildCatalog {
            guilds,
            offset,
            total,
        } => {
            if offset == 0 {
                s.catalog = guilds;
            } else {
                s.catalog.extend(guilds);
            }
            s.catalog_total = total;
        }
        ServerMessage::GuildDelete { guild_id } => {
            s.guilds.retain(|g| g.id != guild_id);
            let removed: Vec<Id> = s
                .channels
                .iter()
                .filter(|c| c.guild_id == guild_id)
                .map(|c| c.id)
                .collect();
            s.channels.retain(|c| c.guild_id != guild_id);
            s.members.retain(|m| m.guild_id != guild_id);
            s.roles.remove(&guild_id);
            s.bans.remove(&guild_id);
            s.invites.remove(&guild_id);
            s.integrations.remove(&guild_id);
            for cid in &removed {
                s.messages.remove(cid);
            }
            if s.selected_guild == Some(guild_id) {
                let next = s.guilds.first().map(|g| g.id);
                s.selected_guild = next;
                s.selected_channel = next.and_then(|gid| s.default_channel_of(gid));
                if let Some(channel_id) = s.selected_channel
                    && !s.messages.contains_key(&channel_id)
                {
                    let _ = tx.send(ClientMessage::FetchMessages {
                        channel_id,
                        limit: 50,
                        before_ms: None,
                    });
                }
            }
        }
        ServerMessage::ProfileUpdate(profile) => {
            crate::dlog!(
                "[profile] ProfileUpdate pubkey={} avatar={} banner={}",
                &profile.pubkey[..profile.pubkey.len().min(8)],
                profile.avatar.is_some(),
                profile.banner.is_some()
            );
            s.profiles.insert(profile.pubkey.clone(), profile);
            resolve_media(&mut s, tx);
        }
        ServerMessage::ActivityUpdate(update) => match update.activity {
            Some(activity) => {
                s.activities.insert(update.pubkey, activity);
            }
            None => {
                s.activities.remove(&update.pubkey);
            }
        },
        ServerMessage::ReactionUpdate {
            channel_id,
            message_id,
            reactions,
        } => {
            if let Some(msgs) = s.messages.get_mut(&channel_id)
                && let Some(msg) = msgs.iter_mut().find(|m| m.id == message_id)
            {
                msg.reactions = reactions;
            }
        }
        ServerMessage::TypingUpdate {
            channel_id,
            user_pubkey,
            username,
        } => {
            s.typing
                .entry(channel_id)
                .or_default()
                .insert(user_pubkey, (username, std::time::Instant::now()));
        }
        ServerMessage::GuildUpdate(guild) => {
            if let Some(slot) = s.guilds.iter_mut().find(|g| g.id == guild.id) {
                *slot = guild;
            }
            resolve_media(&mut s, tx);
        }
        ServerMessage::GuildIntegrations { guild_id, bots } => {
            s.integrations.insert(guild_id, bots);
        }
        ServerMessage::BotCommands(set) => {
            s.bot_commands.insert(set.bot_pubkey, set.commands);
        }
        ServerMessage::CommandResponse {
            invocation_id,
            bot_pubkey,
            channel_id,
            content,
        } => {
            s.command_notes.push(crate::state::CommandNote {
                invocation_id,
                bot_pubkey,
                channel_id,
                content,
            });
            let over = s
                .command_notes
                .len()
                .saturating_sub(crate::state::MAX_COMMAND_NOTES);
            s.command_notes.drain(..over);
        }
        ServerMessage::CommandInvoked(_) => {
            tracing::warn!("ignoring a bot invocation sent to a person");
        }
        ServerMessage::GuildRoles { guild_id, roles } => {
            s.roles.insert(guild_id, by_position(roles));
        }
        ServerMessage::GuildEmojis { guild_id, emojis } => {
            s.guild_emojis.insert(guild_id, emojis);
            resolve_media(&mut s, tx);
        }
        ServerMessage::GuildSounds { guild_id, sounds } => {
            s.guild_sounds.insert(guild_id, sounds);
            resolve_media(&mut s, tx);
        }
        ServerMessage::SoundPlayed {
            channel_id,
            user_pubkey,
            sound_id,
        } => {
            let name = s
                .channels
                .iter()
                .find(|c| c.id == channel_id)
                .and_then(|c| s.guild_sounds.get(&c.guild_id))
                .and_then(|list| list.iter().find(|x| x.id == sound_id))
                .map(|x| x.name.clone());
            if let Some(name) = name {
                s.recent_sounds
                    .insert(user_pubkey, (name, std::time::Instant::now()));
            }
        }
        ServerMessage::EmojiBlobs { blobs } => {
            // Only emoji and sounds reach the disk cache: they are shared by a
            // whole guild and asked for again on every connect. A message
            // picture is neither.
            let mut shared = emoji_addresses(&s);
            shared.extend(sound_addresses(&s));
            for blob in blobs {
                if !blob.data_url.is_empty() && shared.contains(&blob.image) {
                    crate::media_cache::store(&blob.image, &blob.data_url);
                }
                s.emoji_requested.remove(&blob.image);
                s.emoji_images.insert(blob.image, blob.data_url);
            }
        }
        ServerMessage::MemberUpdate(member) => {
            let existing = s
                .members
                .iter_mut()
                .find(|x| x.guild_id == member.guild_id && x.user.pubkey == member.user.pubkey);
            match existing {
                Some(slot) => *slot = member,
                None => s.members.push(member),
            }
        }
        ServerMessage::MemberRemove {
            guild_id,
            user_pubkey,
        } => {
            s.members
                .retain(|m| !(m.guild_id == guild_id && m.user.pubkey == user_pubkey));
            s.pending_rekey = true;
        }
        ServerMessage::GuildInvite { guild_id, code, .. } => {
            s.invites.insert(guild_id, code);
        }
        ServerMessage::GuildBans { guild_id, users } => {
            s.bans.insert(guild_id, users);
        }
        ServerMessage::AuditLog { guild_id, entries } => {
            s.audit_logs.insert(guild_id, entries);
        }
        ServerMessage::JoinChallenge {
            guild_id,
            gate,
            rules,
            pow_challenge,
            pow_difficulty,
            invite_code,
        } => {
            use crate::protocol::JoinGate;
            let tx = tx.clone();
            let pending_code = invite_code.clone();
            let resend = move |accept: bool, pow_nonce: Option<String>| {
                let msg = match &invite_code {
                    Some(code) => ClientMessage::JoinByInvite {
                        code: code.clone(),
                        accept,
                        pow_nonce,
                    },
                    None => ClientMessage::JoinGuild {
                        guild_id,
                        accept,
                        pow_nonce,
                    },
                };
                let _ = tx.send(msg);
            };
            match gate {
                JoinGate::Open => resend(false, None),
                JoinGate::Rules => {
                    s.rules_prompt = Some(crate::state::RulesPrompt {
                        guild_id,
                        guild_name: s
                            .catalog
                            .iter()
                            .find(|g| g.id == guild_id)
                            .map(|g| g.name.clone()),
                        rules: rules.unwrap_or_default(),
                        invite_code: pending_code,
                    });
                }
                JoinGate::Pow => {
                    if let (Some(challenge), Some(bits)) = (pow_challenge, pow_difficulty) {
                        spawn(async move {
                            let nonce = solve_pow(&challenge, bits);
                            resend(false, Some(nonce));
                        });
                    }
                }
            }
        }
        ServerMessage::ChannelCreate(ch) => {
            if !s.channels.iter().any(|c| c.id == ch.id) {
                s.channels.push(ch);
            }
        }
        ServerMessage::ChannelUpdate(ch) => {
            if let Some(slot) = s.channels.iter_mut().find(|c| c.id == ch.id) {
                *slot = ch;
            }
        }
        ServerMessage::ChannelDelete {
            guild_id,
            channel_id,
        } => {
            s.channels.retain(|c| c.id != channel_id);
            s.messages.remove(&channel_id);
            s.typing.remove(&channel_id);
            s.screen_shares.remove(&channel_id);
            if s.selected_channel == Some(channel_id) {
                let next = s.default_channel_of(guild_id);
                s.selected_channel = next;
                if let Some(cid) = next
                    && !s.messages.contains_key(&cid)
                {
                    let _ = tx.send(ClientMessage::FetchMessages {
                        channel_id: cid,
                        limit: 50,
                        before_ms: None,
                    });
                }
            }
        }
        ServerMessage::MessageDelete {
            channel_id,
            message_id,
        } => {
            if let Some(msgs) = s.messages.get_mut(&channel_id) {
                msgs.retain(|m| m.id != message_id);
            }
        }
        ServerMessage::ScreenShareState {
            channel_id,
            sharers,
        } => {
            if sharers.is_empty() {
                s.screen_shares.remove(&channel_id);
            } else {
                s.screen_shares.insert(channel_id, sharers);
            }
            let active: std::collections::HashSet<String> =
                s.screen_shares.values().flatten().cloned().collect();
            s.screen_viewing.retain(|pk| active.contains(pk));
        }
        ServerMessage::MemberJoin(member) => {
            let exists = s
                .members
                .iter_mut()
                .find(|m| m.guild_id == member.guild_id && m.user.pubkey == member.user.pubkey);
            match exists {
                Some(existing) => *existing = member,
                None => s.members.push(member),
            }
        }
        ServerMessage::MemberLeave {
            guild_id,
            user_pubkey,
        } => {
            if let Some(m) = s
                .members
                .iter_mut()
                .find(|m| m.guild_id == guild_id && m.user.pubkey == user_pubkey)
            {
                m.online = false;
            }
        }
        ServerMessage::MediaKey {
            channel_id,
            from,
            epoch,
            blob,
        } => {
            let Some(identity) = s.identity.clone() else {
                return;
            };
            // The call the server has us in, not the one we are still leaving:
            // the key in use is global, so a late one for another call replaces ours.
            if s.server_voice_channel() != Some(channel_id) {
                tracing::debug!(%from, epoch, "ignoring a media key for a call we are not in");
                return;
            }
            let key = match crate::mediakey::open(&blob, &from, epoch, &identity) {
                Ok(key) => key,
                Err(e) => {
                    tracing::warn!(%from, epoch, error = %e, "could not open a media key");
                    return;
                }
            };
            let held = s.media_keys.get(&channel_id).copied();
            match crate::mediakey::judge(held, (epoch, key)) {
                crate::mediakey::Verdict::Keep => {
                    tracing::trace!(%from, epoch, "already running this key");
                }
                crate::mediakey::Verdict::Adopt => {
                    tracing::info!(%from, epoch, "media key accepted");
                    s.media_keys.insert(channel_id, (epoch, key));
                    s.media_undecryptable = false;
                    crate::e2ee::apply_key(&key, epoch);
                }
                crate::mediakey::Verdict::Answer => {
                    let Some(ours) = held else { return };
                    tracing::info!(%from, epoch, ours = ours.0, "answering a losing media key with ours");
                    match crate::mediakey::seal(&ours.1, &from, ours.0, &identity) {
                        Ok(blob) => {
                            let _ = tx.send(ClientMessage::ShareMediaKey {
                                channel_id,
                                to: from.clone(),
                                epoch: ours.0,
                                blob,
                            });
                            crate::mediakey::note_sent(channel_id, &from, ours);
                        }
                        Err(e) => tracing::warn!(%from, error = %e, "could not seal our media key"),
                    }
                }
            }
        }
        ServerMessage::VoiceStateUpdate(vs) => {
            let mut vs = vs;
            let self_pubkey = s.self_user.as_ref().map(|u| u.pubkey.clone());
            let is_self = self_pubkey.as_deref() == Some(vs.user_pubkey.as_str());
            if is_self {
                crate::dlog!(
                    "[net] VoiceStateUpdate(self) channel={:?} muted={} deafened={} speaking={} phase={:?}",
                    vs.channel_id,
                    vs.muted,
                    vs.deafened,
                    vs.speaking,
                    s.voice.phase
                );
            }

            if is_self && vs.channel_id.is_some() && s.voice.joining_channel_id == vs.channel_id {
                s.voice.joining_channel_id = None;
                let muted = s.voice.muted || s.voice.deafened;
                let deafened = s.voice.deafened;
                // Older hosts ignore join preferences; their initial echo must not open the mic.
                if vs.muted != muted || vs.deafened != deafened {
                    let _ = tx.send(ClientMessage::SetVoiceMute { muted, deafened });
                    vs.muted = muted;
                    vs.deafened = deafened;
                }
            }

            let existing_idx = s
                .voice_states
                .iter()
                .position(|v| v.user_pubkey == vs.user_pubkey);
            match existing_idx {
                Some(i) => {
                    if vs.channel_id.is_some() {
                        s.voice_states[i] = vs.clone();
                    } else {
                        s.voice_states.remove(i);
                    }
                }
                None => {
                    if vs.channel_id.is_some() {
                        s.voice_states.push(vs.clone());
                    }
                }
            }

            if s.screen_viewing.contains(&vs.user_pubkey)
                && (!vs.screen_sharing || vs.channel_id.is_none())
            {
                s.screen_viewing.remove(&vs.user_pubkey);
            }

            if is_self {
                if vs.channel_id.is_some() && vs.muted != s.voice.muted {
                    let _ = voice_tx.send(VoiceCmd::SetMute { muted: vs.muted });
                }
                if vs.channel_id.is_some() && vs.deafened != s.voice.deafened {
                    let _ = voice_tx.send(VoiceCmd::SetDeafen {
                        deafened: vs.deafened,
                    });
                }
                if vs.channel_id.is_some() {
                    s.voice.muted = vs.muted;
                    s.voice.deafened = vs.deafened;
                }
                if vs.channel_id.is_none() && s.voice.phase != VoicePhase::Idle {
                    eprintln!("[net] server says we're out of voice — forcing Idle");
                    let _ = voice_tx.send(VoiceCmd::Disconnect { done: None });
                    s.end_voice_locally();
                }
            }
        }
        ServerMessage::VoiceToken {
            channel_id,
            livekit_url,
            alternate_urls,
            route_revision,
            token,
            ice_servers,
        } => {
            eprintln!("[net] VoiceToken channel={channel_id} url={livekit_url}");
            if route_revision < s.voice_route_revision
                || s.server_voice_channel() != Some(channel_id)
            {
                return;
            }
            s.voice_route_revision = route_revision;
            if route_revision == 1
                && let Some(host) = &mut s.host_info
            {
                host.voice_bundled = false;
                host.livekit_url = livekit_url.clone();
                host.voice_reason =
                    "Local voice endpoints failed; this session now uses rendezvous voice.".into();
            }
            s.voice.phase = VoicePhase::Connecting;
            s.voice.channel_id = Some(channel_id);
            s.voice.error = None;
            s.voice_endpoint = None;
            s.ice_servers = ice_servers.clone();
            let _ = voice_tx.send(VoiceCmd::Connect {
                livekit_url,
                alternate_urls,
                route_revision,
                report_tx: tx.clone(),
                token,
                channel_id,
                ice_servers,
            });
        }
        ServerMessage::ScreenToken {
            channel_id,
            route_revision,
            livekit_url,
            token,
            audio_token,
            video_token,
            viewer_token,
            ice_servers,
            ..
        } => {
            if route_revision < s.voice_route_revision
                || s.server_voice_channel() != Some(channel_id)
            {
                return;
            }
            let livekit_url = s.voice_endpoint.clone().unwrap_or(livekit_url);
            s.ice_servers = ice_servers;
            s.screen_token = Some((livekit_url.clone(), token));
            s.screen_audio_token =
                (!audio_token.is_empty()).then_some((livekit_url.clone(), audio_token));
            s.screen_video_token =
                (!video_token.is_empty()).then_some((livekit_url.clone(), video_token));
            s.screen_viewer_token =
                (!viewer_token.is_empty()).then_some((livekit_url, viewer_token));
        }
        ServerMessage::VoiceRouteChanged {
            livekit_url,
            reason,
        } => {
            s.voice_route_revision = 1;
            if let Some(host) = &mut s.host_info {
                host.voice_bundled = false;
                host.livekit_url = livekit_url;
                host.voice_reason = reason;
            }
        }
        ServerMessage::Error { message } => {
            s.voice.joining_channel_id = None;
            tracing::warn!(server_error = %message);
            s.error_toast = Some(message);
        }
        ServerMessage::Hello { .. } => {
            tracing::warn!("ignoring late Hello frame from server");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_local_tokens_cannot_restore_an_old_route_and_screen_follows_the_selected_address() {
        fn harness() -> Element {
            let mut state = use_signal(AppState::empty);
            use_hook(move || {
                let channel = Id::new_v4();
                let pubkey = "ab".repeat(32);
                state.write().self_user = Some(crate::protocol::User {
                    pubkey: pubkey.clone(),
                    username: "Host".into(),
                });
                state
                    .write()
                    .voice_states
                    .push(crate::protocol::VoiceState {
                        user_pubkey: pubkey,
                        guild_id: Id::new_v4(),
                        channel_id: Some(channel),
                        muted: false,
                        deafened: false,
                        speaking: false,
                        camera_on: false,
                        screen_sharing: false,
                        screen_watching: Vec::new(),
                    });
                let (tx, _messages) = unbounded_channel();
                let (voice_tx, mut native) = unbounded_channel();
                apply(
                    &mut state,
                    ServerMessage::VoiceRouteChanged {
                        livekit_url: "wss://shared".into(),
                        reason: "local failed".into(),
                    },
                    &tx,
                    &voice_tx,
                );
                let token = |revision| ServerMessage::VoiceToken {
                    channel_id: channel,
                    livekit_url: "ws://primary".into(),
                    alternate_urls: Vec::new(),
                    route_revision: revision,
                    token: "voice".into(),
                    ice_servers: Vec::new(),
                };
                apply(&mut state, token(0), &tx, &voice_tx);
                assert!(native.try_recv().is_err());
                apply(&mut state, token(1), &tx, &voice_tx);
                assert!(matches!(
                    native.try_recv().unwrap(),
                    VoiceCmd::Connect {
                        route_revision: 1,
                        ..
                    }
                ));
                state
                    .write()
                    .set_voice_endpoint("ws://[2800:810::1]:7880".into());
                let screen = |revision| ServerMessage::ScreenToken {
                    channel_id: channel,
                    livekit_url: "ws://primary".into(),
                    route_revision: revision,
                    token: "screen".into(),
                    audio_token: "audio".into(),
                    video_token: "video".into(),
                    viewer_token: "viewer".into(),
                    ice_servers: Vec::new(),
                };
                apply(&mut state, screen(0), &tx, &voice_tx);
                assert!(state.peek().screen_token.is_none());
                apply(&mut state, screen(1), &tx, &voice_tx);
                for token in [
                    &state.peek().screen_token,
                    &state.peek().screen_audio_token,
                    &state.peek().screen_video_token,
                    &state.peek().screen_viewer_token,
                ] {
                    assert_eq!(token.as_ref().unwrap().0, "ws://[2800:810::1]:7880");
                }
            });
            rsx! {}
        }
        let mut dom = VirtualDom::new(harness);
        dom.rebuild_in_place();
    }

    #[tokio::test]
    async fn legacy_join_echo_cannot_clear_local_mute_or_deafen_and_leave_keeps_choices() {
        fn harness() -> Element {
            let mut state = use_signal(AppState::empty);
            use_hook(move || {
                for (muted, deafened) in [(true, false), (true, true), (false, false)] {
                    let channel = Id::new_v4();
                    let pubkey = "ab".repeat(32);
                    {
                        let mut s = state.write();
                        s.self_user = Some(crate::protocol::User {
                            pubkey: pubkey.clone(),
                            username: "Alice".into(),
                        });
                        s.voice.muted = muted;
                        s.voice.deafened = deafened;
                        s.voice.joining_channel_id = Some(channel);
                    }
                    let (tx, mut messages) = unbounded_channel();
                    let (voice_tx, mut native) = unbounded_channel();
                    let vs = crate::protocol::VoiceState {
                        user_pubkey: pubkey,
                        guild_id: Id::new_v4(),
                        channel_id: Some(channel),
                        muted: false,
                        deafened: false,
                        speaking: false,
                        camera_on: false,
                        screen_sharing: false,
                        screen_watching: Vec::new(),
                    };
                    apply(
                        &mut state,
                        ServerMessage::VoiceStateUpdate(vs.clone()),
                        &tx,
                        &voice_tx,
                    );
                    assert_eq!(state.peek().voice.muted, muted);
                    assert_eq!(state.peek().voice.deafened, deafened);
                    assert!(
                        native.try_recv().is_err(),
                        "join must not send an unmute command"
                    );
                    if muted || deafened {
                        assert!(
                            matches!(messages.try_recv().unwrap(), ClientMessage::SetVoiceMute { muted: true, deafened: d } if d == deafened)
                        );
                    } else {
                        assert!(messages.try_recv().is_err());
                    }
                    apply(
                        &mut state,
                        ServerMessage::VoiceStateUpdate(crate::protocol::VoiceState {
                            channel_id: None,
                            ..vs
                        }),
                        &tx,
                        &voice_tx,
                    );
                    assert_eq!(state.peek().voice.muted, muted);
                    assert_eq!(state.peek().voice.deafened, deafened);
                }
            });
            rsx! {}
        }
        let mut dom = VirtualDom::new(harness);
        dom.rebuild_in_place();
    }

    #[test]
    fn reconnect_wait_is_bounded_and_starts_with_a_short_retry() {
        assert_eq!(reconnect_delay(0).as_secs(), 1);
        assert_eq!(reconnect_delay(1).as_secs(), 2);
        assert_eq!(reconnect_delay(2).as_secs(), 4);
        assert_eq!(reconnect_delay(3).as_secs(), 8);
        assert_eq!(reconnect_delay(4).as_secs(), 15);
        assert_eq!(reconnect_delay(u32::MAX).as_secs(), 15);
    }

    #[tokio::test]
    async fn ready_after_reconnect_preserves_selected_dm_history_and_delivery_state() {
        fn harness() -> Element {
            let mut state = use_signal(AppState::empty);
            use_hook(move || {
                let channel = Id::new_v4();
                let message_id = Id::new_v4();
                let user = crate::protocol::User {
                    pubkey: "ab".repeat(32),
                    username: "Alice".into(),
                };
                {
                    let mut s = state.write();
                    s.status = ConnectionStatus::Reconnecting;
                    s.dm_mode = true;
                    s.selected_channel = Some(channel);
                    s.dms.push(crate::state::DmInfo {
                        channel_id: channel,
                        other_pubkey: "cd".repeat(32),
                    });
                    s.messages.insert(
                        channel,
                        vec![crate::protocol::Message {
                            id: message_id,
                            channel_id: channel,
                            author: user.clone(),
                            content: "Keep this message".into(),
                            image: None,
                            reactions: vec![],
                            reply_to: None,
                            created_at: chrono::Utc::now(),
                        }],
                    );
                    s.dm_delivery
                        .insert(message_id, crate::nostr::delivery::Delivery::Pending);
                }
                let (tx, mut commands) = unbounded_channel();
                let (voice_tx, _voice_rx) = unbounded_channel();
                apply(
                    &mut state,
                    ServerMessage::Ready {
                        user,
                        guilds: vec![],
                        channels: vec![],
                        members: vec![],
                        voice_states: vec![],
                        catalog: vec![],
                        profiles: vec![],
                        roles: vec![],
                        emojis: vec![],
                        sounds: vec![],
                        activities: vec![],
                        bot_commands: vec![],
                        operator: false,
                    },
                    &tx,
                    &voice_tx,
                );
                let s = state.peek();
                assert_eq!(s.status, ConnectionStatus::Ready);
                assert!(s.dm_mode);
                assert_eq!(s.selected_channel, Some(channel));
                assert_eq!(s.messages[&channel][0].id, message_id);
                assert_eq!(
                    s.dm_delivery[&message_id],
                    crate::nostr::delivery::Delivery::Pending
                );
                assert!(commands.try_recv().is_err());
            });
            rsx! {}
        }
        let mut dom = VirtualDom::new(harness);
        dom.rebuild_in_place();
    }

    #[test]
    fn the_quic_handshake_presents_a_loopback_host() {
        let url = url::Url::parse(QUIC_HANDSHAKE_URL).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.path(), "/gateway");
    }

    fn share(addrs: &str) -> String {
        format!("quic://{}@{addrs}", "ab".repeat(32))
    }

    #[test]
    fn a_share_string_dials_the_key_it_names() {
        match parse_target(&share("192.168.1.5:4433;https://relay.example/")) {
            Ok(Dial::Quic { key, addrs }) => {
                assert_eq!(key, "ab".repeat(32));
                assert_eq!(addrs, ["192.168.1.5:4433", "https://relay.example/"]);
            }
            _ => panic!("a share string must dial QUIC"),
        }
    }

    #[test]
    fn plaintext_is_for_this_machine_only() {
        for local in [
            "ws://127.0.0.1:9000",
            "localhost:9000",
            "http://[::1]:9000/",
        ] {
            match parse_target(local) {
                Ok(Dial::Socket {
                    transport, origin, ..
                }) => {
                    assert_eq!(transport, Transport::Loopback, "{local}");
                    assert!(origin.ends_with(":9000"), "{origin}");
                }
                _ => panic!("{local} is loopback and must be allowed"),
            }
        }
        let refused = parse_target("ws://192.168.1.5:9000").expect_err("plaintext off loopback");
        assert!(refused.contains("in the clear"), "{refused}");
        assert!(
            parse_target("box.example:9000").is_err(),
            "a bare host means ws://"
        );
    }

    #[tokio::test]
    async fn a_picture_the_server_never_answered_is_asked_for_again() {
        let (tx, mut rx) = unbounded_channel::<ClientMessage>();
        let mut s = AppState::empty();
        let channel = Id::new_v4();
        s.messages.insert(
            channel,
            vec![crate::protocol::Message {
                id: Id::new_v4(),
                channel_id: channel,
                author: crate::protocol::User {
                    pubkey: "a".repeat(64),
                    username: "a".into(),
                },
                content: String::new(),
                image: Some(format!("media:{}.png", "b".repeat(64))),
                reactions: Vec::new(),
                reply_to: None,
                created_at: chrono::Utc::now(),
            }],
        );
        let address = format!("{}.png", "b".repeat(64));

        resolve_media(&mut s, &tx);
        assert!(
            rx.try_recv().is_err(),
            "unviewed pictures should not be fetched"
        );
        assert!(s.media_src(&format!("media:{address}")).is_none());
        resolve_media(&mut s, &tx);
        match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
            Ok(Some(ClientMessage::FetchEmoji { images })) => {
                assert_eq!(images, std::slice::from_ref(&address))
            }
            other => panic!("expected one fetch, got {other:?}"),
        }
        resolve_media(&mut s, &tx);
        assert!(rx.try_recv().is_err(), "still in flight, not asked twice");

        s.emoji_requested.insert(
            address.clone(),
            std::time::Instant::now() - MEDIA_RETRY_AFTER * 2,
        );
        resolve_media(&mut s, &tx);
        assert!(
            matches!(
                tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await,
                Ok(Some(ClientMessage::FetchEmoji { .. }))
            ),
            "an old unanswered request is repeated"
        );

        s.emoji_requested.remove(&address);
        s.emoji_images
            .insert(address.clone(), "data:image/png;base64,AA==".into());
        resolve_media(&mut s, &tx);
        assert!(
            rx.try_recv().is_err(),
            "an answered picture is never asked for again"
        );
        assert_eq!(
            s.media_src(&format!("media:{address}")),
            Some("data:image/png;base64,AA==")
        );
    }

    #[test]
    fn tls_through_a_proxy_is_allowed_anywhere() {
        match parse_target("wss://chat.example.com") {
            Ok(Dial::Socket {
                transport, origin, ..
            }) => {
                assert_eq!(transport, Transport::Proxied);
                assert_eq!(origin, "chat.example.com:443");
            }
            _ => panic!("wss:// must be allowed"),
        }
    }
}
