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
use crate::state::{AppState, ConnectionStatus, GatewayTx, SessionMode, SessionParams, VoicePhase};

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

async fn resolve_session(
    mode: SessionMode,
    identity: crate::identity::Identity,
    state: &mut Signal<AppState>,
) -> Result<(String, Option<HostHandle>), String> {
    match mode {
        SessionMode::Remote { server_url } => {
            let url = normalize_url(&server_url)?;
            Ok((url, None))
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
            // We're hosting, so we own our Lobby.
            let handle = start_self_host(allow_lan, rendezvous_url, publish, identity).await?;
            let url = normalize_url(&handle.info.local_url)?;
            state.write().host_info = Some(handle.info.clone());
            Ok((url, Some(handle)))
        }
        SessionMode::ByCode {
            rendezvous_url,
            code,
        } => {
            let base = rendezvous_url.trim().trim_end_matches('/');
            if base.is_empty() {
                return Err("rendezvous URL required".into());
            }
            let with_scheme = if base.starts_with("ws://") || base.starts_with("wss://") {
                base.to_string()
            } else if let Some(rest) = base.strip_prefix("http://") {
                format!("ws://{rest}")
            } else if let Some(rest) = base.strip_prefix("https://") {
                format!("wss://{rest}")
            } else {
                format!("ws://{base}")
            };
            let url = format!("{with_scheme}/join/{}", code.trim());
            Ok((url, None))
        }
    }
}

async fn run(
    params: SessionParams,
    tx: &UnboundedSender<ClientMessage>,
    mut rx: UnboundedReceiver<ClientMessage>,
    mut state: Signal<AppState>,
    voice_tx: &UnboundedSender<VoiceCmd>,
) -> Result<(), String> {
    // For self-host, brings up the embedded server first and binds its
    // shutdown to this task by holding the HostHandle in scope.
    let (url, _host_handle) =
        resolve_session(params.mode.clone(), params.identity.clone(), &mut state).await?;
    eprintln!("[dioxusfun] connecting to {url}");

    let (ws_stream, _) = tokio_tungstenite::connect_async(&url)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Wait for the server's Hello frame so we know what nonce to sign.
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

    // Schnorr-sign nonce || pubkey || username with the Nostr identity key and
    // send the Identify response.
    let username = params.username.clone();
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
    };
    let json = serde_json::to_string(&identify).map_err(|e| e.to_string())?;
    ws_tx
        .send(WsMessage::Text(json.into()))
        .await
        .map_err(|e| format!("send identify: {e}"))?;

    // Publish our locally-owned profile (avatar/bio) so it travels with us to
    // this host. Sent right after Identify; the server processes frames in
    // order, so we're identified by the time it handles this.
    if let Some(local) = crate::profile::load() {
        if local.avatar.is_some()
            || local.banner.is_some()
            || local.bio.is_some()
            || local.status.is_some()
            || local.custom_status.is_some()
        {
            let set_profile = ClientMessage::SetProfile {
                avatar: local.avatar,
                banner: local.banner,
                bio: local.bio,
                status: local.status,
                custom_status: local.custom_status,
            };
            if let Ok(json) = serde_json::to_string(&set_profile) {
                let _ = ws_tx.send(WsMessage::Text(json.into())).await;
            }
        }
    }

    loop {
        tokio::select! {
            outbound = rx.recv() => {
                let Some(msg) = outbound else { break };
                let json = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
                if let Err(e) = ws_tx.send(WsMessage::Text(json.into())).await {
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
            dms,
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
            s.dms = dms;
            s.dm_mode = false;
            s.catalog = catalog;
            s.profiles = profiles
                .into_iter()
                .map(|p| (p.pubkey.clone(), p))
                .collect();
            // Group the flattened role list by guild.
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
            // Merge rather than replace: an initial load starts from empty, but
            // an older page (infinite scroll) must fold into what's already
            // there without dropping live messages. Dedupe by id, keep
            // chronological order.
            let combined = s.messages.entry(channel_id).or_default();
            let existing: std::collections::HashSet<_> = combined.iter().map(|m| m.id).collect();
            for m in messages {
                if !existing.contains(&m.id) {
                    combined.push(m);
                }
            }
            combined.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        }
        ServerMessage::MessageCreate(m) => {
            let cid = m.channel_id;
            // A channel we don't have as a guild channel must be a DM addressed
            // to us (the server only delivers DM frames to participants).
            let is_dm = !s.channels.iter().any(|c| c.id == cid);
            // First message of a DM someone started with us — materialise the
            // conversation from the author.
            if is_dm && s.dm_of(cid).is_none() {
                s.dms.push(crate::protocol::DmInfo {
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
            // Mention = the message names us with "@username", or it is a reply
            // to something we wrote.
            //
            // A reply counts even in a channel you're looking at and even
            // without an "@": answering someone is addressing them, and it's the
            // case most worth hearing about. Matched on pubkey rather than
            // username because a username is not unique and can change, while
            // the key is the account.
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
            // A new message from someone clears their typing indicator.
            if let Some(set) = s.typing.get_mut(&cid) {
                set.remove(&m.author.pubkey);
            }
            s.messages.entry(cid).or_default().push(m);
            // Badge only on inbound DM messages for a conversation you're not
            // currently looking at.
            if is_dm && !author_is_self && !viewing {
                *s.dm_unread.entry(cid).or_insert(0) += 1;
            }
            // Chime on an inbound DM you're not looking at, or any mention.
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
        } => {
            // We created or joined this guild — add it (dedup) and jump to it.
            let gid = guild.id;
            if !s.guilds.iter().any(|g| g.id == gid) {
                s.guilds.push(guild);
            }
            s.roles.insert(gid, roles);
            s.guild_emojis.insert(gid, emojis);
            resolve_emoji_images(&mut s, tx);
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
            // Select the guild and its first text channel.
            s.dm_mode = false;
            s.selected_guild = Some(gid);
            let first_text = s.default_channel_of(gid);
            s.selected_channel = first_text;
            if let Some(channel_id) = first_text {
                if !s.messages.contains_key(&channel_id) {
                    let _ = tx.send(ClientMessage::FetchMessages {
                        channel_id,
                        limit: 50,
                        before_ms: None,
                    });
                }
            }
        }
        ServerMessage::GuildCatalog {
            guilds,
            offset,
            total,
        } => {
            // Page 0 replaces the directory; later pages append (infinite
            // scroll). The catalog is now pull-based (FetchCatalog on browse).
            if offset == 0 {
                s.catalog = guilds;
            } else {
                s.catalog.extend(guilds);
            }
            s.catalog_total = total;
        }
        ServerMessage::GuildDelete { guild_id } => {
            // Drop the guild, its channels and their message logs.
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
            // If we were viewing the deleted guild, fall back to another one.
            if s.selected_guild == Some(guild_id) {
                let next = s.guilds.first().map(|g| g.id);
                s.selected_guild = next;
                s.selected_channel = next.and_then(|gid| s.default_channel_of(gid));
                if let Some(channel_id) = s.selected_channel {
                    if !s.messages.contains_key(&channel_id) {
                        let _ = tx.send(ClientMessage::FetchMessages {
                            channel_id,
                            limit: 50,
                            before_ms: None,
                        });
                    }
                }
            }
        }
        ServerMessage::DmReady {
            channel_id,
            other,
            messages,
        } => {
            // Authoritative open of a DM we initiated: list it, load history,
            // switch to the DM view and select it.
            if !s.dms.iter().any(|d| d.channel_id == channel_id) {
                s.dms.push(crate::protocol::DmInfo { channel_id, other });
            }
            s.messages.insert(channel_id, messages);
            s.dm_mode = true;
            s.selected_channel = Some(channel_id);
            s.dm_unread.remove(&channel_id);
        }
        ServerMessage::ProfileUpdate(profile) => {
            eprintln!(
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
            if let Some(msgs) = s.messages.get_mut(&channel_id) {
                if let Some(msg) = msgs.iter_mut().find(|m| m.id == message_id) {
                    msg.reactions = reactions;
                }
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
                // An empty data URL means the server has no such blob. Cache
                // that too: without it a missing image is re-requested on every
                // catalog push, forever.
                if !blob.data_url.is_empty() {
                    crate::emoji::store_cached(&blob.image, &blob.data_url);
                }
                s.emoji_images.insert(blob.image, blob.data_url);
            }
        }
        ServerMessage::MemberUpdate(member) => {
            // Role set (or other member metadata) changed — upsert the row.
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
            // Gone from the guild (kicked/banned/left/uninstalled) — drop the
            // roster row. (If WE were the one removed, the server also sends
            // us a targeted GuildDelete, which tears down the whole guild.)
            s.members
                .retain(|m| !(m.guild_id == guild_id && m.user.pubkey == user_pubkey));
        }
        ServerMessage::GuildInvite { guild_id, code } => {
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
            // Auto-satisfy the join gate and retry. (Rules are auto-accepted
            // for now; a read-and-accept dialog is a follow-up. PoW is solved
            // off-thread so the UI never stalls.)
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
                if let Some(cid) = next {
                    if !s.messages.contains_key(&cid) {
                        let _ = tx.send(ClientMessage::FetchMessages {
                            channel_id: cid,
                            limit: 50,
                            before_ms: None,
                        });
                    }
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
            // Close the viewer if the person we're watching stopped sharing.
            if let Some(pk) = s.screen_viewing.clone() {
                if !s.screen_shares.values().any(|v| v.contains(&pk)) {
                    s.screen_viewing = None;
                }
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
        ServerMessage::VoiceStateUpdate(vs) => {
            let self_pubkey = s.self_user.as_ref().map(|u| u.pubkey.clone());
            let is_self = self_pubkey.as_deref() == Some(vs.user_pubkey.as_str());
            if is_self {
                eprintln!(
                    "[net] VoiceStateUpdate(self) channel={:?} muted={} phase={:?}",
                    vs.channel_id, vs.muted, s.voice.phase
                );
            }

            // Mirror into roster.
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

            // Self updates propagate to local VoiceSession.
            if is_self {
                // The server is the authority on these two and can disagree
                // with what we asked for — it forces mute on while deafened,
                // and it drops both when we weren't in a channel to begin with.
                // Its answer has to reach the audio path and not only the
                // buttons, or the mic ends up live under a red icon (or the
                // mixer stays gated while the icon says otherwise).
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
                    // Leaving voice also tears down the screen-share room, and
                    // with it any native capture — the target has to go too, or
                    // the effect that owns publishing would resume the share on
                    // the next voice session.
                    s.screen_token = None;
                    s.screen_audio_token = None;
                    s.screen_video_token = None;
                    s.screen_share_target = None;
                    s.screen_sharing = false;
                    s.screen_viewing = None;
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
            // Hand the JS screen bridge what it needs to join the screen room.
            s.screen_token = Some((livekit_url.clone(), token));
            // Empty from a server that predates the native audio path; leaving
            // this None is what keeps the webview playing stream audio there.
            s.screen_audio_token =
                (!audio_token.is_empty()).then_some((livekit_url.clone(), audio_token));
            // Likewise for native *video*. None from an older server, which on
            // macOS means no share is possible at all — the webview has no
            // capture API to fall back to. `screen_capture_available` follows
            // this, so the button explains itself rather than failing silently.
            s.screen_video_token = (!video_token.is_empty()).then_some((livekit_url, video_token));
        }
        ServerMessage::Error { message } => {
            tracing::warn!(server_error = %message);
            // Surface as a toast — permission/moderation rejections would
            // otherwise be invisible to the user.
            s.error_toast = Some(message);
        }
        ServerMessage::Hello { .. } => {
            // Hello is only valid as the FIRST frame and is consumed by the
            // handshake loop in `run()`. Anywhere else, ignore.
            tracing::warn!("ignoring late Hello frame from server");
        }
    }
}
