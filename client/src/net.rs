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
        let _ = voice_tx.send(VoiceCmd::Disconnect { done: None });
        on_disconnect(reason);
    });

    gateway_tx
}

fn supersedes(have: Option<u32>, epoch: u32, from: &str, me: Option<&str>) -> bool {
    match (have, me) {
        (None, _) => true,
        (Some(have), Some(mine)) if epoch == have => from < mine,
        (Some(have), _) => epoch > have,
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
    rx: UnboundedReceiver<ClientMessage>,
    mut state: Signal<AppState>,
    voice_tx: &UnboundedSender<VoiceCmd>,
) -> Result<(), String> {
    let (dial, _host_handle) =
        resolve_session(params.mode.clone(), params.identity.clone(), &mut state).await?;

    let (ws_stream, transport, origin) = match dial {
        Dial::Socket {
            url,
            origin,
            transport,
        } => {
            eprintln!("[dioxusfun] connecting to {url}");
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .map_err(|e| format!("connect failed: {e}"))?;
            (Socket::Tcp(Box::new(ws)), transport, origin)
        }
        Dial::Quic { key, addrs } => {
            eprintln!("[dioxusfun] dialling {key} at {addrs:?}");
            let (socket, relayed) = tokio::time::timeout(QUIC_ATTEMPT, dial_quic(&key, &addrs))
                .await
                .map_err(|_| format!("no answer from the host within {QUIC_ATTEMPT:?}"))??;
            let transport = if relayed {
                Transport::QuicRelayed
            } else {
                Transport::Quic
            };
            (socket, transport, crate::protocol::quic_origin(&key))
        }
    };
    state.write().transport = transport;

    match ws_stream {
        Socket::Tcp(ws) => run_session(*ws, params, origin, tx, rx, state, voice_tx).await,
        Socket::Quic(ws, _guard) => run_session(*ws, params, origin, tx, rx, state, voice_tx).await,
    }
}

enum Socket {
    Tcp(Box<Ws>),
    Quic(
        Box<tokio_tungstenite::WebSocketStream<crate::quic::GatewayIo>>,
        crate::quic::ConnectionGuard,
    ),
}

async fn run_session<S>(
    ws_stream: tokio_tungstenite::WebSocketStream<S>,
    params: SessionParams,
    origin: String,
    tx: &UnboundedSender<ClientMessage>,
    mut rx: UnboundedReceiver<ClientMessage>,
    mut state: Signal<AppState>,
    voice_tx: &UnboundedSender<VoiceCmd>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
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

    let username = crate::protocol::canonical_username(&params.username);
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
    ws_tx
        .send(WsMessage::Text(json))
        .await
        .map_err(|e| format!("send identify: {e}"))?;

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
            let _ = ws_tx.send(WsMessage::Text(json)).await;
        }
    }

    let mut media_tick = tokio::time::interval(MEDIA_TICK);
    loop {
        tokio::select! {
            outbound = rx.recv() => {
                let Some(msg) = outbound else { break };
                let json = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
                if let Err(e) = ws_tx.send(WsMessage::Text(json)).await {
                    return Err(format!("send: {e}"));
                }
            }
            _ = media_tick.tick() => {
                let mut s = state.write();
                resolve_media(&mut s, tx);
            }
            inbound = ws_rx.next() => {
                let Some(frame) = inbound else { break };
                let frame = frame.map_err(|e| format!("recv: {e}"))?;
                let text = match frame {
                    WsMessage::Text(t) => t.to_string(),
                    // A host that stops on purpose says so in the close frame;
                    // "connection closed" would be true and useless.
                    WsMessage::Close(Some(f)) if !f.reason.is_empty() => {
                        return Err(f.reason.to_string());
                    }
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

fn emoji_addresses(s: &AppState) -> Vec<String> {
    s.guild_emojis
        .values()
        .flatten()
        .map(|e| e.image.clone())
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

/// Every picture the server named and has not yet handed over. Emoji are
/// small and disk-cached, so they go in big batches; everything else can be
/// a full 3 MB, and the server's per-answer budget trims what does not fit.
fn resolve_media(s: &mut AppState, tx: &UnboundedSender<ClientMessage>) {
    const EMOJI_PER_REQUEST: usize = 16;
    const IMAGES_PER_REQUEST: usize = 8;

    let now = std::time::Instant::now();
    let in_flight = |s: &AppState, address: &str| {
        s.emoji_requested
            .get(address)
            .is_some_and(|asked| now.duration_since(*asked) < MEDIA_RETRY_AFTER)
    };

    let mut emoji: Vec<String> = Vec::new();
    for image in emoji_addresses(s) {
        if s.emoji_images.contains_key(&image) || in_flight(s, &image) {
            continue;
        }
        if let Some(data_url) = crate::emoji::load_cached(&image) {
            s.emoji_images.insert(image, data_url);
            continue;
        }
        s.emoji_requested.insert(image.clone(), now);
        emoji.push(image);
    }

    let mut images: Vec<String> = Vec::new();
    let named = s
        .profiles
        .values()
        .flat_map(|p| [p.avatar.as_deref(), p.banner.as_deref()])
        .chain(
            s.guilds
                .iter()
                .flat_map(|g| [g.icon_image.as_deref(), g.banner.as_deref()]),
        )
        .chain(s.messages.values().flatten().map(|m| m.image.as_deref()))
        .flatten()
        .filter_map(media_address)
        .collect::<Vec<_>>();
    for address in named {
        if s.emoji_images.contains_key(&address) || in_flight(s, &address) {
            continue;
        }
        s.emoji_requested.insert(address.clone(), now);
        images.push(address);
    }

    for chunk in emoji.chunks(EMOJI_PER_REQUEST) {
        let _ = tx.send(ClientMessage::FetchEmoji {
            images: chunk.to_vec(),
        });
    }
    for chunk in images.chunks(IMAGES_PER_REQUEST) {
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
            s.messages = BTreeMap::new();
            // A request the last session never got an answer to would
            // otherwise stay "in flight" forever.
            s.emoji_requested.clear();
            resolve_media(&mut s, tx);
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
                s.notify_tick = s.notify_tick.wrapping_add(1);
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
            voice_states,
        } => {
            let gid = guild.id;
            if !s.guilds.iter().any(|g| g.id == gid) {
                s.guilds.push(guild);
            }
            s.roles.insert(gid, roles);
            s.guild_emojis.insert(gid, emojis);
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
        ServerMessage::GuildRoles { guild_id, roles } => {
            s.roles.insert(guild_id, roles);
        }
        ServerMessage::GuildEmojis { guild_id, emojis } => {
            s.guild_emojis.insert(guild_id, emojis);
            resolve_media(&mut s, tx);
        }
        ServerMessage::EmojiBlobs { blobs } => {
            // Only emoji reach the disk cache: they are small, shared by a
            // whole guild and asked for on every connect. A message picture
            // is none of those.
            let emoji = emoji_addresses(&s);
            for blob in blobs {
                if !blob.data_url.is_empty() && emoji.contains(&blob.image) {
                    crate::emoji::store_cached(&blob.image, &blob.data_url);
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
            let identity = s.identity.clone();
            let Some(identity) = identity else { return };
            let me = s.self_user.as_ref().map(|u| u.pubkey.clone());
            let have = s.media_keys.get(&channel_id).copied();
            if !supersedes(have.map(|(e, _)| e), epoch, &from, me.as_deref()) {
                tracing::debug!(%from, epoch, "ignoring a media key that does not supersede ours");
                return;
            }
            match crate::mediakey::open(&blob, &from, epoch, &identity) {
                Ok(key) => {
                    if have == Some((epoch, key)) {
                        tracing::trace!(%from, epoch, "already running this key");
                        return;
                    }
                    tracing::info!(%from, epoch, "media key accepted");
                    s.media_keys.insert(channel_id, (epoch, key));
                    s.media_undecryptable = false;
                    crate::e2ee::apply_key(&key, epoch);
                }
                Err(e) => tracing::warn!(%from, epoch, error = %e, "could not open a media key"),
            }
        }
        ServerMessage::VoiceStateUpdate(vs) => {
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

            if s.screen_viewing.as_deref() == Some(vs.user_pubkey.as_str())
                && (!vs.screen_sharing || vs.channel_id.is_none())
            {
                s.screen_viewing = None;
            }

            if is_self {
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
                    let _ = voice_tx.send(VoiceCmd::Disconnect { done: None });
                    s.screen_token = None;
                    s.screen_audio_token = None;
                    s.screen_video_token = None;
                    s.screen_share_target = None;
                    s.screen_sharing = false;
                    s.screen_viewing = None;
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
            s.screen_audio_token =
                (!audio_token.is_empty()).then_some((livekit_url.clone(), audio_token));
            s.screen_video_token = (!video_token.is_empty()).then_some((livekit_url, video_token));
        }
        ServerMessage::Error { message } => {
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
    fn an_equal_epoch_is_broken_by_pubkey_so_both_sides_converge() {
        let mine = Some("bbbb");

        assert!(supersedes(None, 1, "cccc", mine));

        assert!(supersedes(Some(1), 1, "aaaa", mine));
        assert!(!supersedes(Some(1), 1, "cccc", mine));

        assert!(supersedes(Some(1), 2, "cccc", mine));
        assert!(!supersedes(Some(2), 1, "aaaa", mine));

        assert!(!supersedes(Some(3), 3, "bbbb", mine));
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

    #[test]
    fn a_picture_the_server_never_answered_is_asked_for_again() {
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
        match rx.try_recv() {
            Ok(ClientMessage::FetchEmoji { images }) => {
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
            matches!(rx.try_recv(), Ok(ClientMessage::FetchEmoji { .. })),
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
