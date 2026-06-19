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
    AppState, ConnectionStatus, GatewayTx, SessionMode, SessionParams, VoicePhase,
};

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
            let handle = start_self_host(allow_lan, rendezvous_url, publish).await?;
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
    let (url, _host_handle) = resolve_session(params.mode.clone(), &mut state).await?;
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

    // Sign nonce || pubkey || username with the identity's signing key and
    // send the Identify response.
    let username = params.username.clone();
    let pubkey = params.identity.pubkey.clone();
    let mut to_sign = Vec::with_capacity(nonce.len() + pubkey.len() + username.len());
    to_sign.extend_from_slice(nonce.as_bytes());
    to_sign.extend_from_slice(pubkey.as_bytes());
    to_sign.extend_from_slice(username.as_bytes());
    let signature = params.identity.sign_base58(&to_sign);

    let identify = ClientMessage::Identify {
        username,
        pubkey,
        signature,
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
        } => {
            s.self_user = Some(user);
            s.guilds = guilds;
            s.channels = channels;
            s.members = members;
            s.voice_states = voice_states;
            s.dms = dms;
            s.dm_mode = false;
            s.catalog = catalog;
            s.profiles = profiles.into_iter().map(|p| (p.pubkey.clone(), p)).collect();
            s.messages = BTreeMap::new();
            s.status = ConnectionStatus::Ready;

            if let Some(first) = s.guilds.first().map(|g| g.id) {
                s.selected_guild = Some(first);
                let chan = s
                    .channels
                    .iter()
                    .find(|c| {
                        c.guild_id == first
                            && matches!(c.kind, crate::protocol::ChannelKind::Text)
                    })
                    .map(|c| c.id);
                s.selected_channel = chan;
                if let Some(channel_id) = chan {
                    let _ = tx.send(ClientMessage::FetchMessages {
                        channel_id,
                        limit: 50,
                    });
                }
            }
        }
        ServerMessage::MessageHistory { channel_id, messages } => {
            s.messages.insert(channel_id, messages);
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
            let viewing = s.selected_channel == Some(cid)
                && (is_dm == s.dm_mode);
            // Mention = the message names us with "@username".
            let mentioned = s
                .self_user
                .as_ref()
                .map(|u| m.content.contains(&format!("@{}", u.username)))
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
        ServerMessage::GuildJoined { guild, channels, members } => {
            // We created or joined this guild — add it (dedup) and jump to it.
            let gid = guild.id;
            if !s.guilds.iter().any(|g| g.id == gid) {
                s.guilds.push(guild);
            }
            for ch in channels {
                if !s.channels.iter().any(|c| c.id == ch.id) {
                    s.channels.push(ch);
                }
            }
            for m in members {
                let existing = s.members.iter_mut().find(|x| {
                    x.guild_id == m.guild_id && x.user.pubkey == m.user.pubkey
                });
                match existing {
                    Some(slot) => *slot = m,
                    None => s.members.push(m),
                }
            }
            // Select the guild and its first text channel.
            s.dm_mode = false;
            s.selected_guild = Some(gid);
            let first_text = s
                .channels
                .iter()
                .find(|c| {
                    c.guild_id == gid && matches!(c.kind, crate::protocol::ChannelKind::Text)
                })
                .map(|c| c.id);
            s.selected_channel = first_text;
            if let Some(channel_id) = first_text {
                if !s.messages.contains_key(&channel_id) {
                    let _ = tx.send(ClientMessage::FetchMessages { channel_id, limit: 50 });
                }
            }
        }
        ServerMessage::GuildCatalog { guilds } => {
            s.catalog = guilds;
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
            for cid in &removed {
                s.messages.remove(cid);
            }
            // If we were viewing the deleted guild, fall back to another one.
            if s.selected_guild == Some(guild_id) {
                let next = s.guilds.first().map(|g| g.id);
                s.selected_guild = next;
                s.selected_channel = next.and_then(|gid| {
                    s.channels
                        .iter()
                        .find(|c| {
                            c.guild_id == gid
                                && matches!(c.kind, crate::protocol::ChannelKind::Text)
                        })
                        .map(|c| c.id)
                });
                if let Some(channel_id) = s.selected_channel {
                    if !s.messages.contains_key(&channel_id) {
                        let _ = tx.send(ClientMessage::FetchMessages { channel_id, limit: 50 });
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
                s.dms.push(crate::protocol::DmInfo {
                    channel_id,
                    other,
                });
            }
            s.messages.insert(channel_id, messages);
            s.dm_mode = true;
            s.selected_channel = Some(channel_id);
            s.dm_unread.remove(&channel_id);
        }
        ServerMessage::DmCreate(info) => {
            // Someone opened a DM with us — add it to the sidebar if new.
            if !s.dms.iter().any(|d| d.channel_id == info.channel_id) {
                s.dms.push(info);
            }
        }
        ServerMessage::ProfileUpdate(profile) => {
            s.profiles.insert(profile.pubkey.clone(), profile);
        }
        ServerMessage::ReactionUpdate { channel_id, message_id, reactions } => {
            if let Some(msgs) = s.messages.get_mut(&channel_id) {
                if let Some(msg) = msgs.iter_mut().find(|m| m.id == message_id) {
                    msg.reactions = reactions;
                }
            }
        }
        ServerMessage::TypingUpdate { channel_id, user_pubkey, username } => {
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
        ServerMessage::MemberJoin(member) => {
            let exists = s.members.iter_mut().find(|m| {
                m.guild_id == member.guild_id && m.user.pubkey == member.user.pubkey
            });
            match exists {
                Some(existing) => *existing = member,
                None => s.members.push(member),
            }
        }
        ServerMessage::MemberLeave { guild_id, user_pubkey } => {
            if let Some(m) = s.members.iter_mut().find(|m| {
                m.guild_id == guild_id && m.user.pubkey == user_pubkey
            }) {
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
                s.voice.muted = vs.muted;
                s.voice.deafened = vs.deafened;
                if vs.channel_id.is_none() && s.voice.phase != VoicePhase::Idle {
                    eprintln!("[net] server says we're out of voice — forcing Idle");
                    s.voice.phase = VoicePhase::Idle;
                    s.voice.channel_id = None;
                    let _ = voice_tx.send(VoiceCmd::Disconnect);
                    // Leaving voice also tears down the screen-share room.
                    s.screen_token = None;
                    s.screen_sharing = false;
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
            ..
        } => {
            // Hand the JS screen bridge what it needs to join the screen room.
            s.screen_token = Some((livekit_url, token));
        }
        ServerMessage::Error { message } => {
            tracing::warn!(server_error = %message);
        }
        ServerMessage::Hello { .. } => {
            // Hello is only valid as the FIRST frame and is consumed by the
            // handshake loop in `run()`. Anywhere else, ignore.
            tracing::warn!("ignoring late Hello frame from server");
        }
    }
}
