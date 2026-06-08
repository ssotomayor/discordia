use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};
use uuid::Uuid;

use crate::AppContext;
use crate::livekit;
use crate::protocol::{ClientMessage, ServerMessage, User};

pub async fn handle_connection(
    socket: WebSocket,
    ctx: Arc<AppContext>,
    client_host: Option<String>,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut hub_rx = ctx.state.hub.subscribe();

    let mut user: Option<User> = None;

    loop {
        tokio::select! {
            incoming = ws_rx.next() => {
                let Some(Ok(msg)) = incoming else { break };
                let text = match msg {
                    WsMessage::Text(t) => t,
                    WsMessage::Close(_) => break,
                    WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Binary(_) => continue,
                };
                let parsed: Result<ClientMessage, _> = serde_json::from_str(&text);
                let Ok(client_msg) = parsed else {
                    let _ = send(&mut ws_tx, &ServerMessage::Error {
                        message: "invalid frame".into(),
                    }).await;
                    continue;
                };

                match client_msg {
                    ClientMessage::Identify { username } => {
                        if user.is_some() {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "already identified".into(),
                            }).await;
                            continue;
                        }
                        let new_user = User {
                            id: Uuid::new_v4(),
                            username: sanitize_username(&username),
                        };
                        let ready = ctx.state.snapshot_for(&new_user);
                        if send(&mut ws_tx, &ready).await.is_err() {
                            break;
                        }
                        for entry in ctx.state.members.iter() {
                            if let Some(member) = entry.value().get(&new_user.id) {
                                let _ = ctx.state.hub.send(ServerMessage::MemberJoin(member.clone()));
                            }
                        }
                        user = Some(new_user);
                    }
                    ClientMessage::FetchMessages { channel_id, limit } => {
                        if user.is_none() {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        }
                        let history = ctx.state.history(channel_id, limit.max(1).min(200));
                        if send(&mut ws_tx, &ServerMessage::MessageHistory {
                            channel_id,
                            messages: history,
                        }).await.is_err() {
                            break;
                        }
                    }
                    ClientMessage::SendMessage { channel_id, content } => {
                        let Some(author) = user.clone() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        let content = content.trim().to_string();
                        if content.is_empty() || content.len() > 2000 {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "message must be 1..=2000 chars".into(),
                            }).await;
                            continue;
                        }
                        match ctx.state.push_message(channel_id, author, content) {
                            Some(msg) => {
                                let _ = ctx.state.hub.send(ServerMessage::MessageCreate(msg));
                            }
                            None => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error {
                                    message: "unknown text channel".into(),
                                }).await;
                            }
                        }
                    }
                    ClientMessage::JoinVoice { channel_id } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        let Some(guild_id) = ctx.state.voice_channel_guild(channel_id) else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "not a voice channel".into(),
                            }).await;
                            continue;
                        };
                        let new_state =
                            ctx.state.set_voice_channel(u.id, guild_id, Some(channel_id));
                        let _ = ctx
                            .state
                            .hub
                            .send(ServerMessage::VoiceStateUpdate(new_state));
                        match livekit::mint_token(&ctx.livekit, u.id, &u.username, channel_id) {
                            Ok(token) => {
                                let livekit_url =
                                    ctx.livekit.url_for_client(client_host.as_deref());
                                let _ = send(
                                    &mut ws_tx,
                                    &ServerMessage::VoiceToken {
                                        channel_id,
                                        livekit_url,
                                        token,
                                    },
                                )
                                .await;
                            }
                            Err(err) => {
                                let _ = send(
                                    &mut ws_tx,
                                    &ServerMessage::Error {
                                        message: format!("token mint failed: {err}"),
                                    },
                                )
                                .await;
                            }
                        }
                    }
                    ClientMessage::LeaveVoice => {
                        let Some(u) = user.as_ref() else { continue };
                        if let Some(cleared) = ctx.state.clear_voice(u.id) {
                            let _ = ctx.state.hub.send(ServerMessage::VoiceStateUpdate(cleared));
                        }
                    }
                    ClientMessage::SetVoiceMute { muted, deafened } => {
                        let Some(u) = user.as_ref() else { continue };
                        if let Some(state) = ctx.state.update_voice_flags(u.id, muted, deafened) {
                            let _ = ctx.state.hub.send(ServerMessage::VoiceStateUpdate(state));
                        }
                    }
                    ClientMessage::SetSpeaking { speaking } => {
                        let Some(u) = user.as_ref() else { continue };
                        if let Some(state) = ctx.state.update_speaking(u.id, speaking) {
                            let _ = ctx.state.hub.send(ServerMessage::VoiceStateUpdate(state));
                        }
                    }
                }
            }

            broadcast = hub_rx.recv() => {
                let Ok(msg) = broadcast else { continue };
                if send(&mut ws_tx, &msg).await.is_err() {
                    break;
                }
            }
        }
    }

    if let Some(u) = user {
        if let Some(cleared) = ctx.state.clear_voice(u.id) {
            let _ = ctx.state.hub.send(ServerMessage::VoiceStateUpdate(cleared));
        }
        for (guild_id, user_id) in ctx.state.mark_offline(u.id) {
            let _ = ctx.state.hub.send(ServerMessage::MemberLeave { guild_id, user_id });
        }
        tracing::info!(user = %u.username, "client disconnected");
    }
}

async fn send<S>(tx: &mut S, msg: &ServerMessage) -> Result<(), axum::Error>
where
    S: SinkExt<WsMessage, Error = axum::Error> + Unpin,
{
    let json = serde_json::to_string(msg).expect("serializable");
    tx.send(WsMessage::Text(json.into())).await
}

fn sanitize_username(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "anonymous".into();
    }
    trimmed.chars().take(32).collect()
}
