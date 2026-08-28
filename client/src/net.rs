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
        let _ = voice_tx.send(VoiceCmd::Disconnect);
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

fn coordination(relay_url: Option<String>) -> crate::quic::Coordination {
    match relay_url {
        Some(url) => crate::quic::Coordination::Relay(url),
        None => crate::quic::Coordination::None,
    }
}

enum Dial {
    Single {
        url: String,
        transport: Transport,
    },
    DirectOrRelay {
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

const DIRECT_ATTEMPT: std::time::Duration = std::time::Duration::from_secs(4);

const PUNCH_ATTEMPT: std::time::Duration = std::time::Duration::from_secs(12);

async fn connect_best(
    quic: Option<(String, Vec<String>, crate::quic::Coordination)>,
    direct: &str,
    relay: &str,
) -> Result<(Socket, Transport), String> {
    let relay_url = relay.to_string();
    let relay_attempt =
        tokio::spawn(async move { tokio_tungstenite::connect_async(&relay_url).await });

    if let Some((key, addrs, coordination)) = quic {
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

const QUIC_HANDSHAKE_URL: &str = "ws://127.0.0.1/gateway";

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

    match ws_stream {
        Socket::Tcp(ws) => run_session(*ws, params, tx, rx, state, voice_tx).await,
        Socket::Quic(ws, guard) => {
            let ended = run_session(*ws, params, tx, rx, state, voice_tx).await;
            quic_disconnect_reason(guard.relay_refused(), ended)
        }
    }
}

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
    let mut to_sign = Vec::with_capacity(nonce.len() + pubkey.len() + username.len());
    to_sign.extend_from_slice(nonce.as_bytes());
    to_sign.extend_from_slice(pubkey.as_bytes());
    to_sign.extend_from_slice(username.as_bytes());
    let signature = params.identity.sign_hex(&to_sign);

    let identify = ClientMessage::Identify {
        username,
        pubkey,
        signature,
        bot: false,
        client_version: crate::version::VERSION.to_string(),
    };
    let json = serde_json::to_string(&identify).map_err(|e| e.to_string())?;
    ws_tx
        .send(WsMessage::Text(json))
        .await
        .map_err(|e| format!("send identify: {e}"))?;

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

fn resolve_emoji_images(s: &mut AppState, tx: &UnboundedSender<ClientMessage>) {
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
            s.messages.entry(cid).or_default().push(m);
            if is_dm && !author_is_self && !viewing {
                *s.dm_unread.entry(cid).or_insert(0) += 1;
            }
            if s.should_ring(cid, author_is_self, viewing) {
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
            pow_challenge,
            pow_difficulty,
            invite_code,
            ..
        } => {
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
                    let _ = voice_tx.send(VoiceCmd::Disconnect);
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

    async fn dead_address() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("ws://{addr}/gateway")
    }

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

    #[tokio::test]
    async fn direct_wins_when_it_answers() {
        let direct = ws_server().await;
        let relay = ws_server().await;
        let (_ws, transport) = connect_best(None, &direct, &relay).await.unwrap();
        assert_eq!(transport, Transport::Direct);
    }

    #[tokio::test]
    async fn relay_carries_the_session_when_direct_is_refused() {
        let direct = dead_address().await;
        let relay = ws_server().await;
        let (_ws, transport) = connect_best(None, &direct, &relay).await.unwrap();
        assert_eq!(transport, Transport::Relayed);
    }

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
