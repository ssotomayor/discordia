//! WebSocket gateway client.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use url::Url;

use crate::features::voice::VoiceCmd;
use crate::host::{HostHandle, start_self_host};
use crate::protocol::{ClientMessage, ServerMessage};
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

    let hello = serde_json::to_string(&ClientMessage::Identify {
        username: params.username.clone(),
    })
    .map_err(|e| e.to_string())?;
    ws_tx
        .send(WsMessage::Text(hello.into()))
        .await
        .map_err(|e| format!("send identify: {e}"))?;

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
        } => {
            s.self_user = Some(user);
            s.guilds = guilds;
            s.channels = channels;
            s.members = members;
            s.voice_states = voice_states;
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
            s.messages.entry(m.channel_id).or_default().push(m);
        }
        ServerMessage::MemberJoin(member) => {
            let exists = s
                .members
                .iter_mut()
                .find(|m| m.guild_id == member.guild_id && m.user.id == member.user.id);
            match exists {
                Some(existing) => *existing = member,
                None => s.members.push(member),
            }
        }
        ServerMessage::MemberLeave { guild_id, user_id } => {
            if let Some(m) = s
                .members
                .iter_mut()
                .find(|m| m.guild_id == guild_id && m.user.id == user_id)
            {
                m.online = false;
            }
        }
        ServerMessage::VoiceStateUpdate(vs) => {
            let self_id = s.self_user.as_ref().map(|u| u.id);
            let is_self = Some(vs.user_id) == self_id;
            if is_self {
                eprintln!(
                    "[net] VoiceStateUpdate(self) channel={:?} muted={} phase={:?}",
                    vs.channel_id, vs.muted, s.voice.phase
                );
            }

            // Mirror into roster.
            if let Some(existing) = s
                .voice_states
                .iter_mut()
                .find(|v| v.user_id == vs.user_id)
            {
                if vs.channel_id.is_some() {
                    *existing = vs.clone();
                } else {
                    let user_id = vs.user_id;
                    s.voice_states.retain(|v| v.user_id != user_id);
                }
            } else if vs.channel_id.is_some() {
                s.voice_states.push(vs.clone());
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
        ServerMessage::Error { message } => {
            tracing::warn!(server_error = %message);
        }
    }
}
