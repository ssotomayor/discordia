//! WebSocket gateway client.

use std::collections::BTreeMap;

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

/// Find a nonce such that SHA-256(challenge ++ nonce) has ≥ `bits` leading zero
/// bits (the `Pow` join gate). Mirrors the server's `pow_ok`.
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
) -> GatewayTx {
    let (tx, rx) = unbounded_channel::<ClientMessage>();
    let gateway_tx = GatewayTx(tx.clone());

    spawn(async move {
        let reason = match run(params, &tx, rx, state, &voice_tx).await {
            Ok(()) => "connection closed".to_string(),
            Err(e) => e,
        };
        state.write().status = ConnectionStatus::Disconnected;
        let _ = voice_tx.send(VoiceCmd::Disconnect);
        on_disconnect(reason);
    });

    gateway_tx
}

/// Whether an arriving media key should replace the one we hold.
///
/// Newer epochs win. An *equal* epoch is the case that matters and used to be
/// rejected outright, which is how two members ended up on two different keys
/// hearing nothing from each other: two clients can each generate an epoch 1
/// for the same channel, and nothing about the number tells them apart. Equal
/// epochs are therefore broken by pubkey, lowest wins — the same rule both ends
/// compute, so both converge instead of each keeping its own.
fn supersedes(have: Option<u32>, epoch: u32, from: &str, me: Option<&str>) -> bool {
    match (have, me) {
        (None, _) => true,
        (Some(have), Some(mine)) if epoch == have => from < mine,
        (Some(have), _) => epoch > have,
    }
}

/// Who, if anyone, may introduce us to this host.
///
/// Whatever relay the rendezvous runs, and nothing otherwise. There is no
/// setting and no default: the third party is the one already chosen by
/// choosing that rendezvous, and a rendezvous that runs none leaves its users
/// on the relayed path rather than reaching for somebody else's servers.
fn coordination(relay_url: Option<String>) -> crate::quic::Coordination {
    match relay_url {
        Some(url) => crate::quic::Coordination::Relay(url),
        None => crate::quic::Coordination::None,
    }
}

/// Where to dial, and what to call the connection once it is up.
enum Dial {
    /// One address, no alternative: our own loopback gateway, or a URL someone
    /// typed.
    Single { url: String, transport: Transport },
    /// The host published an address of its own. Try it, but not on its own —
    /// see `connect_preferring_direct` for why both are dialled at once.
    DirectOrRelay {
        /// The host's QUIC key and where to try it, when it published one.
        /// Preferred over everything else: it is the only candidate that is
        /// both direct *and* private.
        quic: Option<(String, Vec<String>, crate::quic::Coordination)>,
        direct: String,
        relay: String,
    },
}

async fn resolve_session(
    mode: SessionMode,
    identity: crate::identity::Identity,
    state: &mut Signal<AppState>,
) -> Result<(Dial, Option<HostHandle>), String> {
    match mode {
        SessionMode::Remote { server_url } => {
            let url = normalize_url(&server_url)?;
            Ok((
                Dial::Single {
                    url,
                    transport: Transport::Direct,
                },
                None,
            ))
        }
        SessionMode::SelfHost {
            allow_lan,
            rendezvous_url,
            publish_name,
            description,
            publish_public,
        } => {
            let publish = crate::rendezvous::PublishOptions {
                publish_name,
                description,
                publish_public,
            };
            let handle = start_self_host(allow_lan, rendezvous_url, publish, identity).await?;
            let url = normalize_url(&handle.info.local_url)?;
            state.write().host_info = Some(handle.info.clone());
            Ok((
                Dial::Single {
                    url,
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
            let code = code.trim();
            let relay = format!("{with_scheme}/join/{code}");
            // Fallback to relay if the host has no published address; must not
            // fail the join.
            let relayed_only = |relay: String| {
                Ok((
                    Dial::Single {
                        url: relay,
                        transport: Transport::Relayed,
                    },
                    None,
                ))
            };
            let Some(entry) = resolve_host(&with_scheme, code).await else {
                return relayed_only(relay);
            };
            let coordinate = coordination(entry.relay_url.clone());
            let quic = entry
                .transport_key
                .filter(|_| !entry.transport_addrs.is_empty() || coordinate.is_coordinated())
                .map(|key| (key, entry.transport_addrs, coordinate));
            match entry.endpoint.as_deref().map(normalize_url) {
                Some(Ok(direct)) => Ok((
                    Dial::DirectOrRelay {
                        quic,
                        direct,
                        relay,
                    },
                    None,
                )),
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "host advertised an unusable endpoint");
                    relayed_only(relay)
                }
                // A key with no plaintext twin still races — against the relay
                // alone, which is the pairing that matters most anyway.
                None if quic.is_some() => Ok((
                    Dial::DirectOrRelay {
                        quic,
                        direct: relay.clone(),
                        relay,
                    },
                    None,
                )),
                None => relayed_only(relay),
            }
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

/// What a host published for this code — its direct address, its transport key,
/// or neither.
///
/// `/resolve/{code}` rather than `/discover`: the browse listing carries only
/// hosts that opted into being public, and a code handed to friends is exactly
/// the case that did not.
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

/// How long the direct address gets before we settle for the relay.
///
/// It is a ceiling, not a delay: a direct address that answers is taken the
/// moment it answers. The ceiling exists because the interesting failure is not
/// a refusal — that comes back in milliseconds — but a *black hole*: a port
/// mapping that has since expired, or an address that was never right, drops
/// the SYN and leaves the connect to run out the OS timeout, which is over a
/// minute on macOS.
const DIRECT_ATTEMPT: std::time::Duration = std::time::Duration::from_secs(4);

/// The same, for a coordinated attempt. Longer because a hole punch is a
/// negotiation with a third party in the middle of it, and because the
/// enforcement that follows waits to see whether the result is really direct.
const PUNCH_ATTEMPT: std::time::Duration = std::time::Duration::from_secs(12);

/// Try every way in at once and keep the best one that answers.
///
/// Ranked, not first-past-the-post: the QUIC path is preferred whenever it
/// works, because it is the only candidate that is direct *and* private — the
/// plaintext one is a direct socket every hop on the path can read. The relay
/// is last, and it is the only one that hands a third party the conversation.
///
/// All of them are dialled together rather than in sequence because the failure
/// that matters is silent (see `DIRECT_ATTEMPT`) — a friend must not stare at a
/// spinner for a minute to discover a stale mapping. The cost is that a
/// successful direct connection leaves the relay holding a pairing nobody
/// claims; the rendezvous expires those in ten seconds, which is what that
/// timeout is for.
async fn connect_best(
    quic: Option<(String, Vec<String>, crate::quic::Coordination)>,
    direct: &str,
    relay: &str,
) -> Result<(Socket, Transport), String> {
    let relay_url = relay.to_string();
    let relay_attempt =
        tokio::spawn(async move { tokio_tungstenite::connect_async(&relay_url).await });

    if let Some((key, addrs, coordination)) = quic {
        // Its own, longer budget: a hole punch is a negotiation, not a connect,
        // and `DIRECT_ATTEMPT` was sized for the latter.
        let budget = if coordination.is_coordinated() {
            PUNCH_ATTEMPT
        } else {
            DIRECT_ATTEMPT
        };
        match tokio::time::timeout(budget, dial_quic(&key, &addrs, &coordination)).await {
            Ok(Ok(socket)) => {
                relay_attempt.abort();
                eprintln!("[dioxusfun] connected over QUIC to {key}");
                return Ok((socket, Transport::Private));
            }
            Ok(Err(e)) => tracing::info!(error = %e, "quic path unavailable — trying the rest"),
            Err(_) => tracing::info!("quic path timed out — trying the rest"),
        }
    }

    if direct != relay {
        eprintln!("[dioxusfun] trying {direct} directly, {relay} as fallback");
        match tokio::time::timeout(DIRECT_ATTEMPT, tokio_tungstenite::connect_async(direct)).await {
            Ok(Ok((ws, _))) => {
                // Nothing is sent on the relay socket, so aborting mid-handshake
                // costs the rendezvous a pairing slot it already knows how to
                // expire.
                relay_attempt.abort();
                eprintln!("[dioxusfun] connected directly to {direct}");
                return Ok((Socket::Tcp(Box::new(ws)), Transport::Direct));
            }
            Ok(Err(e)) => {
                tracing::info!(%direct, error = %e, "direct connection refused — using the relay")
            }
            Err(_) => tracing::info!(%direct, "direct connection timed out — using the relay"),
        }
    }

    match relay_attempt.await {
        Ok(Ok((ws, _))) => {
            eprintln!("[dioxusfun] connected through the relay {relay}");
            Ok((Socket::Tcp(Box::new(ws)), Transport::Relayed))
        }
        Ok(Err(e)) => Err(format!("connect failed: {e}")),
        Err(e) => Err(format!("connect failed: {e}")),
    }
}

/// The URL used for the WebSocket handshake once a QUIC stream is open.
///
/// There is no hostname behind a QUIC key, so this is invented — but it is not
/// arbitrary, because the gateway reads the `Host` header to decide which
/// LiveKit URL to hand back. A made-up name like `gateway.quic` sails through
/// `url_for_client` untouched and produces `ws://gateway.quic:7880`, an address
/// that resolves nowhere; the symptom would be voice failing to connect, on the
/// one configuration where voice is supposed to be direct.
///
/// Loopback is the honest answer. It is what the gateway already means by "this
/// connection did not arrive on an address you can hand back" — the same
/// substitution a rendezvous-proxied friend gets — so a QUIC peer is offered
/// the host's public address, or its LAN address, rather than a fiction.
const QUIC_HANDSHAKE_URL: &str = "ws://127.0.0.1/gateway";

/// Open the gateway over QUIC: dial the key, then run the ordinary WebSocket
/// client handshake on the stream it gives back.
async fn dial_quic(
    key: &str,
    addrs: &[String],
    coordination: &crate::quic::Coordination,
) -> Result<Socket, String> {
    let endpoint_id = crate::quic::parse_endpoint_id(key)?;
    let addrs: Vec<_> = addrs
        .iter()
        .filter_map(|a| crate::quic::parse_transport_addr(a))
        .collect();
    let (io, guard) = crate::quic::dial(endpoint_id, &addrs, coordination).await?;
    let (ws, _) = tokio_tungstenite::client_async(QUIC_HANDSHAKE_URL, io)
        .await
        .map_err(|e| format!("websocket over quic: {e}"))?;
    Ok(Socket::Quic(Box::new(ws), guard))
}

async fn run(
    params: SessionParams,
    tx: &UnboundedSender<ClientMessage>,
    rx: UnboundedReceiver<ClientMessage>,
    mut state: Signal<AppState>,
    voice_tx: &UnboundedSender<VoiceCmd>,
) -> Result<(), String> {
    // For self-host, brings up the embedded server first and binds its
    // shutdown to this task by holding the HostHandle in scope.
    let (dial, _host_handle) =
        resolve_session(params.mode.clone(), params.identity.clone(), &mut state).await?;

    let (ws_stream, transport) = match dial {
        Dial::Single { url, transport } => {
            eprintln!("[dioxusfun] connecting to {url}");
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .map_err(|e| format!("connect failed: {e}"))?;
            (Socket::Tcp(Box::new(ws)), transport)
        }
        Dial::DirectOrRelay {
            quic,
            direct,
            relay,
        } => connect_best(quic, &direct, &relay).await?,
    };
    state.write().transport = transport;

    // The QUIC guard must stay in scope; dropping it closes the connection.
    match ws_stream {
        Socket::Tcp(ws) => run_session(*ws, params, tx, rx, state, voice_tx).await,
        Socket::Quic(ws, guard) => {
            let ended = run_session(*ws, params, tx, rx, state, voice_tx).await;
            quic_disconnect_reason(guard.relay_refused(), ended)
        }
    }
}

/// What to tell the user when a QUIC session ends.
///
/// The socket reports what it *noticed* — a stream that stopped — while the
/// reason may be something we decided in a task it knows nothing about: the
/// path fell back to the relay and a coordinator is not allowed to carry the
/// conversation. Reporting the symptom there is the silent-fallback shape the
/// rest of this codebase avoids, and it lands on a screen whose only other
/// content is a Reconnect button.
///
/// Says that reconnecting may help, because it genuinely may: hole punching is
/// attempted again from scratch and often succeeds where it just failed.
fn quic_disconnect_reason(relay_refused: bool, ended: Result<(), String>) -> Result<(), String> {
    if relay_refused {
        return Err(
            "the direct connection dropped, and the only path left ran through the \
                    coordinator — which is allowed to introduce you, not to carry your \
                    traffic. Reconnecting may find a direct path again."
                .into(),
        );
    }
    ended
}

/// A gateway socket, whichever transport carried it.
///
/// Both variants are boxed only because of their size. Enabling rustls on
/// `tokio-tungstenite` — which Nostr relays require, since every one of them is
/// `wss://` — grew `MaybeTlsStream` by a whole TLS session, so the plain-TCP
/// variant came to dwarf the QUIC one and every `Socket` on the stack would
/// carry that much whether or not TLS was ever negotiated. Boxing one alone
/// merely reverses which is the outlier, so both are boxed and the enum is two
/// pointers wide.
enum Socket {
    Tcp(Box<Ws>),
    /// The WebSocket plus the QUIC connection underneath it, which has to
    /// outlive the session.
    Quic(
        Box<tokio_tungstenite::WebSocketStream<crate::quic::GatewayIo>>,
        crate::quic::ConnectionGuard,
    ),
}

/// The session proper: Identify, then frames until the socket closes.
///
/// Generic over the stream so the QUIC and TCP paths share every line of it —
/// the gateway protocol is the same protocol either way, and a second copy of
/// this loop is exactly the kind of thing that drifts.
async fn run_session<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    params: SessionParams,
    tx: &UnboundedSender<ClientMessage>,
    mut rx: UnboundedReceiver<ClientMessage>,
    mut state: Signal<AppState>,
    voice_tx: &UnboundedSender<VoiceCmd>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // The identity has to be reachable from `apply`, which is where sealed
    // media keys arrive and where only our own secret can open them.
    state.write().identity = Some(params.identity.clone());
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    let nonce = loop {
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
            ServerMessage::Hello { nonce } => break nonce,
            other => return Err(format!("expected Hello, got {other:?}")),
        }
    };

    // Sign the canonical username, not the raw input. The server canonicalises
    // before verifying, so signing the raw string fails for names >32 chars.
    let username = crate::protocol::canonical_username(&params.username);
    let pubkey = params.identity.pubkey.clone();
    let mut to_sign = Vec::with_capacity(nonce.len() + pubkey.len() + username.len());
    to_sign.extend_from_slice(nonce.as_bytes());
    to_sign.extend_from_slice(pubkey.as_bytes());
    to_sign.extend_from_slice(username.as_bytes());
    let signature = params.identity.sign_hex(&to_sign);

    let identify = ClientMessage::Identify {
        username,
        pubkey,
        signature,
        // Human client — never bot-gated (bots self-declare via the SDK).
        bot: false,
        client_version: crate::version::VERSION.to_string(),
    };
    let json = serde_json::to_string(&identify).map_err(|e| e.to_string())?;
    ws_tx
        .send(WsMessage::Text(json))
        .await
        .map_err(|e| format!("send identify: {e}"))?;

    // Sent after Identify; server processes frames in order, so we are
    // identified by the time this is handled.
    if let Some(local) = crate::profile::load()
        && (local.avatar.is_some()
            || local.banner.is_some()
            || local.bio.is_some()
            || local.status.is_some()
            || local.custom_status.is_some())
    {
        let set_profile = ClientMessage::SetProfile {
            avatar: local.avatar,
            banner: local.banner,
            bio: local.bio,
            status: local.status,
            custom_status: local.custom_status,
        };
        if let Ok(json) = serde_json::to_string(&set_profile) {
            let _ = ws_tx.send(WsMessage::Text(json)).await;
        }
    }

    loop {
        tokio::select! {
            outbound = rx.recv() => {
                let Some(msg) = outbound else { break };
                let json = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
                if let Err(e) = ws_tx.send(WsMessage::Text(json)).await {
                    return Err(format!("send: {e}"));
                }
            }
            inbound = ws_rx.next() => {
                let Some(frame) = inbound else { break };
                let frame = frame.map_err(|e| format!("recv: {e}"))?;
                let text = match frame {
                    WsMessage::Text(t) => t.to_string(),
                    WsMessage::Close(_) => break,
                    _ => continue,
                };
                let parsed: ServerMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(err = %e, "bad server frame");
                        continue;
                    }
                };
                apply(&mut state, parsed, tx, voice_tx);
            }
        }
    }

    Ok(())
}

/// Make sure every emoji in the catalog has its image, asking the server only
/// for what we don't already hold.
///
/// Three tiers, cheapest first: already in memory, on disk from a previous run,
/// or a `FetchEmoji` round trip. The disk tier is what stops a restart from
/// re-downloading a guild's whole emoji set, and it is safe to trust
/// indefinitely because the filename is the SHA-256 of the contents.
fn resolve_emoji_images(s: &mut AppState, tx: &UnboundedSender<ClientMessage>) {
    /// Keep requests under the server's per-frame ceiling; the rest are picked
    /// up by the next catalog push (or the next launch, from disk).
    const MAX_PER_REQUEST: usize = 64;

    let mut wanted: Vec<String> = Vec::new();
    let images: Vec<String> = s
        .guild_emojis
        .values()
        .flatten()
        .map(|e| e.image.clone())
        .collect();
    for image in images {
        if s.emoji_images.contains_key(&image) || s.emoji_requested.contains(&image) {
            continue;
        }
        if let Some(data_url) = crate::emoji::load_cached(&image) {
            s.emoji_images.insert(image, data_url);
            continue;
        }
        s.emoji_requested.insert(image.clone());
        wanted.push(image);
    }
    for chunk in wanted.chunks(MAX_PER_REQUEST) {
        let _ = tx.send(ClientMessage::FetchEmoji {
            images: chunk.to_vec(),
        });
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
            operator,
        } => {
            s.self_user = Some(user);
            s.is_operator = operator;
            s.guilds = guilds;
            s.channels = channels;
            s.members = members;
            s.voice_states = voice_states;
            // Deliberately not cleared: `s.dms` is the Nostr conversation list
            // now, owned by `nostr::service` and unrelated to this snapshot. A
            // reconnect to the gateway must not wipe conversations the gateway
            // has never heard of.
            s.dm_mode = false;
            s.catalog = catalog;
            s.profiles = profiles
                .into_iter()
                .map(|p| (p.pubkey.clone(), p))
                .collect();
            s.roles = {
                let mut map: std::collections::HashMap<Id, Vec<crate::protocol::Role>> =
                    std::collections::HashMap::new();
                for role in roles {
                    map.entry(role.guild_id).or_default().push(role);
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
            resolve_emoji_images(&mut s, tx);
            s.messages = BTreeMap::new();
            s.screen_shares = std::collections::HashMap::new();
            s.screen_viewing = None;
            s.status = ConnectionStatus::Ready;

            if let Some(first) = s.guilds.first().map(|g| g.id) {
                s.selected_guild = Some(first);
                let chan = s.default_channel_of(first);
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
            // Merge rather than replace: older pages (infinite scroll) must
            // fold into existing messages without dropping live ones.
            let combined = s.messages.entry(channel_id).or_default();
            let existing: std::collections::HashSet<_> = combined.iter().map(|m| m.id).collect();
            for m in messages {
                if !existing.contains(&m.id) {
                    combined.push(m);
                }
            }
            combined.sort_by_key(|a| a.created_at);
        }
        ServerMessage::MessageCreate(m) => {
            let cid = m.channel_id;
            // A channel we don't have as a guild channel must be a DM addressed
            // to us (the server only delivers DM frames to participants).
            let is_dm = !s.channels.iter().any(|c| c.id == cid);
            if is_dm && s.dm_of(cid).is_none() {
                s.dms.push(crate::state::DmInfo {
                    channel_id: cid,
                    other: m.author.clone(),
                });
            }
            let author_is_self = s
                .self_user
                .as_ref()
                .map(|u| u.pubkey == m.author.pubkey)
                .unwrap_or(false);
            let viewing = s.selected_channel == Some(cid) && (is_dm == s.dm_mode);
            // Match reply on pubkey, not username, because usernames are
            // mutable and non-unique.
            let mentioned = s
                .self_user
                .as_ref()
                .map(|u| {
                    m.content.contains(&format!("@{}", u.username))
                        || m.reply_to
                            .as_ref()
                            .is_some_and(|r| r.author_pubkey == u.pubkey)
                })
                .unwrap_or(false);
            if let Some(set) = s.typing.get_mut(&cid) {
                set.remove(&m.author.pubkey);
            }
            s.messages.entry(cid).or_default().push(m);
            if is_dm && !author_is_self && !viewing {
                *s.dm_unread.entry(cid).or_insert(0) += 1;
            }
            if !author_is_self && ((is_dm && !viewing) || mentioned) {
                s.notify_tick = s.notify_tick.wrapping_add(1);
            }
        }
        ServerMessage::GuildJoined {
            guild,
            channels,
            members,
            roles,
            emojis,
            voice_states,
        } => {
            let gid = guild.id;
            if !s.guilds.iter().any(|g| g.id == gid) {
                s.guilds.push(guild);
            }
            s.roles.insert(gid, roles);
            s.guild_emojis.insert(gid, emojis);
            resolve_emoji_images(&mut s, tx);
            // Replace rather than merge: server sends the full roster, so
            // local state is stale. Scope by guild_id to preserve voice
            // sessions in other guilds.
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
        }
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
        }
        ServerMessage::GuildIntegrations { guild_id, bots } => {
            s.integrations.insert(guild_id, bots);
        }
        ServerMessage::GuildRoles { guild_id, roles } => {
            s.roles.insert(guild_id, roles);
        }
        ServerMessage::GuildEmojis { guild_id, emojis } => {
            s.guild_emojis.insert(guild_id, emojis);
            resolve_emoji_images(&mut s, tx);
        }
        ServerMessage::EmojiBlobs { blobs } => {
            for blob in blobs {
                // Cache empty data URLs to prevent re-requesting missing
                // images on every catalog push.
                if !blob.data_url.is_empty() {
                    crate::emoji::store_cached(&blob.image, &blob.data_url);
                }
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
            // Drop the roster row. If we were removed, a targeted GuildDelete
            // follows to tear down the guild.
            s.members
                .retain(|m| !(m.guild_id == guild_id && m.user.pubkey == user_pubkey));
            // Rekey because the removed member held the media key and it
            // cannot be revoked; new messages use a new key.
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
            pow_challenge,
            pow_difficulty,
            invite_code,
            ..
        } => {
            // Auto-accept rules for now; PoW is solved off-thread to avoid UI
            // stalls.
            use crate::protocol::JoinGate;
            let tx = tx.clone();
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
                JoinGate::Rules => resend(true, None),
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
            // If we were looking at it, fall back to the guild's first text
            // channel (mirrors the GuildDelete reselect).
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
            if let Some(pk) = s.screen_viewing.clone()
                && !s.screen_shares.values().any(|v| v.contains(&pk))
            {
                s.screen_viewing = None;
            }
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
            // Presence only: the member went offline. Actual removals
            // (kick/ban/leave/uninstall) arrive as MemberRemove.
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
            // Security: only accept if we can open it. Freshness: only if
            // newer, to prevent late blobs from older epochs dragging the
            // channel back to a key a removed member still has.
            let identity = s.identity.clone();
            let Some(identity) = identity else { return };
            // Equal epochs are broken by pubkey (lowest wins) so both ends
            // converge on one key. Without this, two clients generating epoch
            // 1 independently would keep different keys and hear nothing.
            let me = s.self_user.as_ref().map(|u| u.pubkey.clone());
            let have = s.media_keys.get(&channel_id).copied();
            if !supersedes(have.map(|(e, _)| e), epoch, &from, me.as_deref()) {
                tracing::debug!(%from, epoch, "ignoring a media key that does not supersede ours");
                return;
            }
            match crate::mediakey::open(&blob, &from, epoch, &identity) {
                Ok(key) => {
                    // Skip if we already run this key. Treating a re-send as
                    // an adoption caused a livelock: state write re-ran the
                    // effect that sends keys, causing dozens of round
                    // trips/sec until the gateway dropped us.
                    if have == Some((epoch, key)) {
                        tracing::trace!(%from, epoch, "already running this key");
                        return;
                    }
                    tracing::info!(%from, epoch, "media key accepted");
                    s.media_keys.insert(channel_id, (epoch, key));
                    // Reset the undecryptable latch; this key is the most
                    // likely fix for previously failed decryption.
                    s.media_undecryptable = false;
                    crate::e2ee::apply_key(&key, epoch);
                }
                // Not exceptional: a blob for somebody else, or from a member
                // who left mid-rekey. We keep what we have.
                Err(e) => tracing::warn!(%from, epoch, error = %e, "could not open a media key"),
            }
        }
        ServerMessage::VoiceStateUpdate(vs) => {
            let self_pubkey = s.self_user.as_ref().map(|u| u.pubkey.clone());
            let is_self = self_pubkey.as_deref() == Some(vs.user_pubkey.as_str());
            if is_self {
                // Include `speaking` and `deafened` to distinguish state
                // changes; omitting them makes logs look like redundant
                // resends, masking actual updates.
                crate::dlog!(
                    "[net] VoiceStateUpdate(self) channel={:?} muted={} deafened={} speaking={} phase={:?}",
                    vs.channel_id,
                    vs.muted,
                    vs.deafened,
                    vs.speaking,
                    s.voice.phase
                );
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

            // Close viewer if the watched user stops sharing or leaves. This
            // handles cases where the `ScreenShareState` frame is missing or
            // legacy.
            if s.screen_viewing.as_deref() == Some(vs.user_pubkey.as_str())
                && (!vs.screen_sharing || vs.channel_id.is_none())
            {
                s.screen_viewing = None;
            }

            if is_self {
                // Server is authority on mute/deafen state (e.g., forces mute
                // when deafened). Sync audio path to match UI, preventing mic
                // live under red icon.
                if vs.muted != s.voice.muted {
                    let _ = voice_tx.send(VoiceCmd::SetMute { muted: vs.muted });
                }
                if vs.deafened != s.voice.deafened {
                    let _ = voice_tx.send(VoiceCmd::SetDeafen {
                        deafened: vs.deafened,
                    });
                }
                s.voice.muted = vs.muted;
                s.voice.deafened = vs.deafened;
                if vs.channel_id.is_none() && s.voice.phase != VoicePhase::Idle {
                    eprintln!("[net] server says we're out of voice — forcing Idle");
                    s.voice.phase = VoicePhase::Idle;
                    s.voice.channel_id = None;
                    let _ = voice_tx.send(VoiceCmd::Disconnect);
                    // Clear screen-share tokens and target to tear down native
                    // capture; otherwise, the share effect resumes on the next
                    // voice session.
                    s.screen_token = None;
                    s.screen_audio_token = None;
                    s.screen_video_token = None;
                    s.screen_share_target = None;
                    s.screen_sharing = false;
                    s.screen_viewing = None;
                    // Camera uses the same webview room; clearing the token
                    // disconnects the JS controller and releases the device.
                    s.camera_on = false;
                    s.camera_starting = false;
                    s.cameras_watching.clear();
                }
            }
        }
        ServerMessage::VoiceToken {
            channel_id,
            livekit_url,
            token,
        } => {
            eprintln!("[net] VoiceToken channel={channel_id} url={livekit_url}");
            s.voice.phase = VoicePhase::Connecting;
            s.voice.channel_id = Some(channel_id);
            s.voice.error = None;
            let _ = voice_tx.send(VoiceCmd::Connect {
                livekit_url,
                token,
                channel_id,
            });
        }
        ServerMessage::ScreenToken {
            livekit_url,
            token,
            audio_token,
            video_token,
            ..
        } => {
            s.screen_token = Some((livekit_url.clone(), token));
            // Empty from a server that predates the native audio path; leaving
            // this None is what keeps the webview playing stream audio there.
            s.screen_audio_token =
                (!audio_token.is_empty()).then_some((livekit_url.clone(), audio_token));
            // None from an older server means no share is possible on macOS
            // (no webview capture fallback); `screen_capture_available`
            // follows this to explain the disabled state.
            s.screen_video_token = (!video_token.is_empty()).then_some((livekit_url, video_token));
        }
        ServerMessage::Error { message } => {
            tracing::warn!(server_error = %message);
            // Surface as a toast; permission/moderation rejections would
            // otherwise be invisible.
            s.error_toast = Some(message);
        }
        ServerMessage::Hello { .. } => {
            // Hello is only valid as the first frame (consumed by the
            // handshake loop); ignore late arrivals.
            tracing::warn!("ignoring late Hello frame from server");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal has to replace whatever the socket said, and only then.
    ///
    /// Both directions matter. Substituting always would relabel every ordinary
    /// disconnect as a relay problem; substituting never leaves the deliberate
    /// close reported as an anonymous dropped stream, which is the state this
    /// exists to end.
    #[test]
    fn a_refused_relay_fallback_is_what_the_user_is_told() {
        let socket_said = Err("connection reset".to_string());

        let told = quic_disconnect_reason(true, socket_said.clone()).unwrap_err();
        assert!(
            told.contains("coordinator"),
            "the refusal must name what refused: {told}"
        );
        assert!(
            told.contains("Reconnecting"),
            "and say what to do about it: {told}"
        );

        assert_eq!(
            quic_disconnect_reason(false, socket_said.clone()),
            socket_said
        );
        assert_eq!(quic_disconnect_reason(false, Ok(())), Ok(()));
    }

    /// A WebSocket server that completes the handshake and then says nothing.
    /// Enough to answer "did this address connect", which is all the race asks.
    async fn ws_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let _ = tokio_tungstenite::accept_async(stream).await;
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                });
            }
        });
        format!("ws://{addr}/gateway")
    }

    /// A port nothing is listening on, which refuses immediately.
    async fn dead_address() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("ws://{addr}/gateway")
    }

    /// Two members can each generate an epoch 1 for the same channel, and the
    /// rule that decides between them has to be one both compute the same way
    /// — otherwise each keeps its own key and neither hears the other, which is
    /// exactly what happened on the first two-machine test.
    #[test]
    fn an_equal_epoch_is_broken_by_pubkey_so_both_sides_converge() {
        let mine = Some("bbbb");

        assert!(supersedes(None, 1, "cccc", mine));

        // Tie-break by sender ID ensures convergence: for any two members,
        // exactly one adopts.
        assert!(supersedes(Some(1), 1, "aaaa", mine));
        assert!(!supersedes(Some(1), 1, "cccc", mine));

        // Newer always wins, whoever sent it; older never does.
        assert!(supersedes(Some(1), 2, "cccc", mine));
        assert!(!supersedes(Some(2), 1, "aaaa", mine));

        // Our own key can never be superseded by itself echoing back.
        assert!(!supersedes(Some(3), 3, "bbbb", mine));
    }

    /// The QUIC handshake has to present a host the gateway will substitute,
    /// not one it will echo back. An invented hostname passes through
    /// `url_for_client` unchanged and becomes a LiveKit URL that resolves
    /// nowhere — which only shows up as voice failing, and only on the
    /// configuration where the host serves its own SFU.
    #[test]
    fn the_quic_handshake_presents_a_loopback_host() {
        let url = url::Url::parse(QUIC_HANDSHAKE_URL).unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.path(), "/gateway");
    }

    /// The point of the whole exercise: when the host published an address that
    /// works, the relay is not used — nobody in the middle sees the session.
    #[tokio::test]
    async fn direct_wins_when_it_answers() {
        let direct = ws_server().await;
        let relay = ws_server().await;
        let (_ws, transport) = connect_best(None, &direct, &relay).await.unwrap();
        assert_eq!(transport, Transport::Direct);
    }

    /// And the fallback is the whole reason both are dialled: a mapping that
    /// has expired, or a host that never had one, must still be joinable.
    #[tokio::test]
    async fn relay_carries_the_session_when_direct_is_refused() {
        let direct = dead_address().await;
        let relay = ws_server().await;
        let (_ws, transport) = connect_best(None, &direct, &relay).await.unwrap();
        assert_eq!(transport, Transport::Relayed);
    }

    /// A QUIC key that cannot be reached must not strand the join: the plaintext
    /// paths are still there, and the point of racing is that no single
    /// candidate can hold the session hostage.
    #[tokio::test]
    async fn an_unreachable_quic_key_falls_through_to_the_rest() {
        let direct = ws_server().await;
        let relay = ws_server().await;
        let key = iroh::SecretKey::generate().public().to_string();
        let dead = dead_address().await;
        let addr = dead
            .trim_start_matches("ws://")
            .trim_end_matches("/gateway");

        let quic = Some((key, vec![addr.to_string()], crate::quic::Coordination::None));
        let (_ws, transport) = connect_best(quic, &direct, &relay).await.unwrap();
        assert_eq!(transport, Transport::Direct);
    }

    /// With neither reachable, the error is the relay's — the direct address is
    /// the optimistic attempt, and reporting its failure would send someone
    /// looking at the wrong end of the connection.
    #[tokio::test]
    async fn both_down_is_a_connect_failure() {
        let direct = dead_address().await;
        let relay = dead_address().await;
        let err = match connect_best(None, &direct, &relay).await {
            Ok(_) => panic!("connected to two dead addresses"),
            Err(e) => e,
        };
        assert!(err.contains("connect failed"), "{err}");
    }
}
