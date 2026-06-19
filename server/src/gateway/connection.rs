use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};

use crate::AppContext;
use crate::auth;
use crate::livekit;
use crate::protocol::{ClientMessage, Member, ServerMessage, User};

pub async fn handle_connection(
    socket: WebSocket,
    ctx: Arc<AppContext>,
    client_host: Option<String>,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let mut hub_rx = ctx.state.hub.subscribe();

    // Issue a per-connection nonce immediately so the client knows what to
    // sign in its Identify response.
    let nonce = auth::fresh_nonce();
    if send(&mut ws_tx, &ServerMessage::Hello { nonce: nonce.clone() })
        .await
        .is_err()
    {
        return;
    }

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
                    ClientMessage::Identify { username, pubkey, signature } => {
                        if user.is_some() {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "already identified".into(),
                            }).await;
                            continue;
                        }
                        let username = sanitize_username(&username);
                        if let Err(e) = auth::verify_identify(&pubkey, &signature, &nonce, &username) {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: format!("identify rejected: {e}"),
                            }).await;
                            continue;
                        }
                        let new_user = User { pubkey: pubkey.clone(), username };
                        ctx.state.remember_user(&new_user);
                        let ready = ctx.state.snapshot_for(&new_user);
                        if send(&mut ws_tx, &ready).await.is_err() {
                            break;
                        }
                        // Announce our (re)appearance to the members of each
                        // guild we belong to — only they should see it. Collect
                        // first so we don't re-enter the members DashMap while
                        // iterating it.
                        let joins: Vec<(uuid::Uuid, Member)> = ctx
                            .state
                            .members
                            .iter()
                            .filter_map(|e| {
                                e.value()
                                    .get(&new_user.pubkey)
                                    .map(|m| (*e.key(), m.clone()))
                            })
                            .collect();
                        for (gid, member) in joins {
                            let targets = ctx.state.guild_member_pubkeys(gid);
                            ctx.state.deliver(targets, ServerMessage::MemberJoin(member));
                        }
                        tracing::info!(user = %new_user.username, pubkey = %new_user.pubkey, "identified");
                        user = Some(new_user);
                    }
                    ClientMessage::FetchMessages { channel_id, limit } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        // Never hand DM history to a non-participant, nor guild
                        // channel history to a non-member.
                        let forbidden = if let Some(p) = ctx.state.dm_participants(channel_id) {
                            !p.iter().any(|x| x == &u.pubkey)
                        } else if let Some(gid) = ctx.state.channel_guild(channel_id) {
                            !ctx.state.is_guild_member(gid, &u.pubkey)
                        } else {
                            false
                        };
                        if forbidden {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "you don't have access to that channel".into(),
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
                    ClientMessage::SendMessage { channel_id, content, image } => {
                        let Some(author) = user.clone() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        let content = content.trim().to_string();
                        if let Some(img) = &image {
                            if !img.starts_with("data:image/")
                                || img.len() > crate::state::MAX_IMAGE_LEN
                            {
                                let _ = send(&mut ws_tx, &ServerMessage::Error {
                                    message: "image must be a data:image/* URL under the size limit".into(),
                                }).await;
                                continue;
                            }
                        }
                        // Text is optional when an image is attached, but
                        // either way it's length-capped and a wholly empty
                        // message is rejected.
                        if content.len() > 2000 || (content.is_empty() && image.is_none()) {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "message must be 1..=2000 chars (or include an image)".into(),
                            }).await;
                            continue;
                        }

                        // Route to a guild text channel (members only), else a
                        // DM the author participates in, else reject.
                        if let Some(gid) = ctx.state.channel_guild(channel_id) {
                            if !ctx.state.is_guild_member(gid, &author.pubkey) {
                                let _ = send(&mut ws_tx, &ServerMessage::Error {
                                    message: "you're not a member of this guild".into(),
                                }).await;
                                continue;
                            }
                            match ctx.state.push_message(channel_id, author, content, image) {
                                Some(msg) => {
                                    let targets = ctx.state.guild_member_pubkeys(gid);
                                    ctx.state.deliver(targets, ServerMessage::MessageCreate(msg));
                                }
                                None => {
                                    let _ = send(&mut ws_tx, &ServerMessage::Error {
                                        message: "can't post to this channel".into(),
                                    }).await;
                                }
                            }
                        } else if let Some(participants) = ctx.state.dm_participants(channel_id) {
                            if participants.iter().any(|p| p == &author.pubkey) {
                                if let Some(msg) =
                                    ctx.state.push_dm_message(channel_id, author, content, image)
                                {
                                    ctx.state.deliver(
                                        participants.to_vec(),
                                        ServerMessage::MessageCreate(msg),
                                    );
                                }
                            } else {
                                let _ = send(&mut ws_tx, &ServerMessage::Error {
                                    message: "not a participant of this conversation".into(),
                                }).await;
                            }
                        } else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "unknown channel".into(),
                            }).await;
                        }
                    }
                    ClientMessage::CreateGuild { name } => {
                        let Some(creator) = user.clone() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        let name = name.trim().to_string();
                        if name.is_empty() || name.chars().count() > 64 {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "guild name must be 1..=64 chars".into(),
                            }).await;
                            continue;
                        }
                        let (guild, channels, member) = ctx.state.create_guild(&name, &creator);
                        tracing::info!(guild = %guild.name, by = %creator.username, "guild created");
                        // The creator is the only member — hand the guild to
                        // them directly, then refresh everyone's directory.
                        if send(&mut ws_tx, &ServerMessage::GuildJoined {
                            guild,
                            channels,
                            members: vec![member],
                        }).await.is_err() {
                            break;
                        }
                        ctx.state.broadcast(ServerMessage::GuildCatalog {
                            guilds: ctx.state.guild_catalog(),
                        });
                    }
                    ClientMessage::JoinGuild { guild_id } => {
                        let Some(joiner) = user.clone() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        match ctx.state.join_guild(guild_id, &joiner) {
                            Some((guild, channels, members)) => {
                                tracing::info!(%guild_id, by = %joiner.username, "guild joined");
                                // Notify the EXISTING members (exclude the
                                // joiner, who gets the full roster below).
                                let mut targets = ctx.state.guild_member_pubkeys(guild_id);
                                targets.retain(|p| p != &joiner.pubkey);
                                ctx.state.deliver(
                                    targets,
                                    ServerMessage::MemberJoin(Member {
                                        user: joiner.clone(),
                                        guild_id,
                                        online: true,
                                    }),
                                );
                                if send(&mut ws_tx, &ServerMessage::GuildJoined {
                                    guild,
                                    channels,
                                    members,
                                }).await.is_err() {
                                    break;
                                }
                                ctx.state.broadcast(ServerMessage::GuildCatalog {
                                    guilds: ctx.state.guild_catalog(),
                                });
                            }
                            None => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error {
                                    message: "unknown guild".into(),
                                }).await;
                            }
                        }
                    }
                    ClientMessage::DeleteGuild { guild_id } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        // Capture members BEFORE deletion removes them.
                        let targets = ctx.state.guild_member_pubkeys(guild_id);
                        match ctx.state.delete_guild(guild_id, &u.pubkey) {
                            Ok(()) => {
                                tracing::info!(%guild_id, by = %u.username, "guild deleted");
                                ctx.state.deliver(targets, ServerMessage::GuildDelete { guild_id });
                                ctx.state.broadcast(ServerMessage::GuildCatalog {
                                    guilds: ctx.state.guild_catalog(),
                                });
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::OpenDm { user_pubkey } => {
                        let Some(me) = user.clone() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if user_pubkey.is_empty() || user_pubkey == me.pubkey {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "cannot open a DM with yourself".into(),
                            }).await;
                            continue;
                        }
                        let channel_id = ctx.state.get_or_create_dm(&me.pubkey, &user_pubkey);
                        let other = ctx
                            .state
                            .users
                            .get(&user_pubkey)
                            .map(|u| u.clone())
                            .unwrap_or_else(|| User {
                                pubkey: user_pubkey.clone(),
                                username: user_pubkey.chars().take(6).collect(),
                            });
                        let messages = ctx.state.history(channel_id, 50);
                        // Reply to the requester only. The other participant
                        // learns of the conversation when the first message
                        // actually arrives — opening a DM window doesn't ping
                        // them or light up their unread badge.
                        if send(&mut ws_tx, &ServerMessage::DmReady {
                            channel_id,
                            other,
                            messages,
                        }).await.is_err() {
                            break;
                        }
                    }
                    ClientMessage::SetProfile { avatar, banner, bio, status, custom_status } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        let valid_image = |img: &String| {
                            img.starts_with("data:image/") && img.len() <= crate::state::MAX_IMAGE_LEN
                        };
                        if avatar.as_ref().is_some_and(|i| !valid_image(i))
                            || banner.as_ref().is_some_and(|i| !valid_image(i))
                        {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "profile images must be data:image/* URLs under the size limit".into(),
                            }).await;
                            continue;
                        }
                        let bio = bio.map(|b| b.chars().take(280).collect::<String>());
                        let custom_status = custom_status.map(|c| c.chars().take(80).collect::<String>());
                        let profile = ctx.state.set_profile(
                            &u.pubkey, avatar, banner, bio, status, custom_status,
                        );
                        // Profiles are public — everyone gets the update.
                        ctx.state.broadcast(ServerMessage::ProfileUpdate(profile));
                    }
                    ClientMessage::React { channel_id, message_id, emoji } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        let Some(audience) = channel_audience(&ctx.state, channel_id) else {
                            continue;
                        };
                        if !audience.iter().any(|p| p == &u.pubkey) {
                            continue; // not allowed in this channel
                        }
                        // Keep the emoji short to avoid abuse.
                        let emoji: String = emoji.chars().take(8).collect();
                        if let Some(reactions) =
                            ctx.state.toggle_reaction(channel_id, message_id, &emoji, &u.pubkey)
                        {
                            ctx.state.deliver(
                                audience,
                                ServerMessage::ReactionUpdate { channel_id, message_id, reactions },
                            );
                        }
                    }
                    ClientMessage::Typing { channel_id } => {
                        let Some(u) = user.as_ref() else { continue };
                        let Some(audience) = channel_audience(&ctx.state, channel_id) else {
                            continue;
                        };
                        if !audience.iter().any(|p| p == &u.pubkey) {
                            continue;
                        }
                        // Notify everyone in the channel except the typist.
                        let targets: Vec<String> =
                            audience.into_iter().filter(|p| p != &u.pubkey).collect();
                        if !targets.is_empty() {
                            ctx.state.deliver(targets, ServerMessage::TypingUpdate {
                                channel_id,
                                user_pubkey: u.pubkey.clone(),
                                username: u.username.clone(),
                            });
                        }
                    }
                    ClientMessage::SetGuildAccent { guild_id, accent } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        // Sanitise to a hex color if present.
                        let accent = accent.filter(|a| is_hex_color(a));
                        match ctx.state.set_guild_accent(guild_id, accent, &u.pubkey) {
                            Ok(guild) => {
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildUpdate(guild));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
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
                        if !ctx.state.is_guild_member(guild_id, &u.pubkey) {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "you're not a member of this guild".into(),
                            }).await;
                            continue;
                        }
                        let new_state =
                            ctx.state.set_voice_channel(&u.pubkey, guild_id, Some(channel_id));
                        let targets = ctx.state.guild_member_pubkeys(guild_id);
                        ctx.state.deliver(targets, ServerMessage::VoiceStateUpdate(new_state));
                        match livekit::mint_token(&ctx.livekit, &u.pubkey, &u.username, channel_id) {
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
                        if let Some(cleared) = ctx.state.clear_voice(&u.pubkey) {
                            let targets = ctx.state.guild_member_pubkeys(cleared.guild_id);
                            ctx.state.deliver(targets, ServerMessage::VoiceStateUpdate(cleared));
                        }
                    }
                    ClientMessage::SetVoiceMute { muted, deafened } => {
                        let Some(u) = user.as_ref() else { continue };
                        if let Some(state) = ctx.state.update_voice_flags(&u.pubkey, muted, deafened) {
                            let targets = ctx.state.guild_member_pubkeys(state.guild_id);
                            ctx.state.deliver(targets, ServerMessage::VoiceStateUpdate(state));
                        }
                    }
                    ClientMessage::SetSpeaking { speaking } => {
                        let Some(u) = user.as_ref() else { continue };
                        if let Some(state) = ctx.state.update_speaking(&u.pubkey, speaking) {
                            let targets = ctx.state.guild_member_pubkeys(state.guild_id);
                            ctx.state.deliver(targets, ServerMessage::VoiceStateUpdate(state));
                        }
                    }
                }
            }

            broadcast = hub_rx.recv() => {
                let Ok(env) = broadcast else { continue };
                // Targeted (DM) frames are delivered only to participants;
                // an unidentified or non-matching connection skips them.
                if let Some(targets) = &env.to {
                    let deliver = user
                        .as_ref()
                        .map(|u| targets.iter().any(|p| p == &u.pubkey))
                        .unwrap_or(false);
                    if !deliver {
                        continue;
                    }
                }
                if send(&mut ws_tx, &env.msg).await.is_err() {
                    break;
                }
            }
        }
    }

    if let Some(u) = user {
        if let Some(cleared) = ctx.state.clear_voice(&u.pubkey) {
            let targets = ctx.state.guild_member_pubkeys(cleared.guild_id);
            ctx.state.deliver(targets, ServerMessage::VoiceStateUpdate(cleared));
        }
        for (guild_id, user_pubkey) in ctx.state.mark_offline(&u.pubkey) {
            // Only the members of that guild should see the leave.
            let targets = ctx.state.guild_member_pubkeys(guild_id);
            ctx.state.deliver(targets, ServerMessage::MemberLeave { guild_id, user_pubkey });
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

/// The set of pubkeys allowed to see activity in a channel: a guild's members
/// for guild channels, or the two participants for a DM.
fn channel_audience(state: &crate::state::AppState, channel_id: crate::protocol::Id) -> Option<Vec<String>> {
    if let Some(gid) = state.channel_guild(channel_id) {
        Some(state.guild_member_pubkeys(gid))
    } else {
        state.dm_participants(channel_id).map(|p| p.to_vec())
    }
}

/// Accept only short `#rrggbb`/`#rgb` hex colors for guild accents.
fn is_hex_color(s: &str) -> bool {
    let s = s.trim();
    let Some(hex) = s.strip_prefix('#') else { return false };
    (hex.len() == 3 || hex.len() == 6) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

fn sanitize_username(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "anonymous".into();
    }
    trimmed.chars().take(32).collect()
}
