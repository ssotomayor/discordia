use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};

use crate::AppContext;
use crate::auth;
use crate::livekit;
use crate::protocol::{ClientMessage, Intent, Member, Permission, ServerMessage, User};

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
    // Set once the identified pubkey turns out to be an installed bot. Bots get
    // an intent-filtered event stream and a narrow action surface.
    let mut is_bot = false;
    // Throttles message-producing actions (protects against a runaway or
    // malicious client; the main reason bots get rate-limited like Discord's).
    let mut limiter = RateLimiter::new();

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

                // Bots have a deliberately narrow action surface: fetch history,
                // post, and react (each still subject to per-guild permissions
                // below). Creating guilds, voice, DMs, profiles and managing
                // integrations are human-only. (Identify is handled before this
                // flag is ever set, so it's never gated here.)
                if is_bot
                    && !matches!(
                        client_msg,
                        ClientMessage::FetchMessages { .. }
                            | ClientMessage::SendMessage { .. }
                            | ClientMessage::React { .. }
                    )
                {
                    let _ = send(&mut ws_tx, &ServerMessage::Error {
                        message: "bots may only fetch history, send messages, and react".into(),
                    }).await;
                    continue;
                }

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
                        // An identity that's been installed as a bot somewhere
                        // gets the scoped, intent-filtered treatment.
                        is_bot = ctx.state.is_bot(&new_user.pubkey);
                        let ready = if is_bot {
                            ctx.state.snapshot_for_bot(&new_user)
                        } else {
                            ctx.state.snapshot_for(&new_user)
                        };
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
                        if is_bot && !bot_can(&ctx.state, channel_id, &u.pubkey, Permission::ReadMessageHistory) {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "bot lacks the read_message_history permission here".into(),
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
                        if !limiter.allow() {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "rate limited: too many messages, slow down".into(),
                            }).await;
                            continue;
                        }
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
                            if is_bot
                                && !ctx
                                    .state
                                    .bot_install(gid, &author.pubkey)
                                    .map(|i| i.has_permission(Permission::SendMessages))
                                    .unwrap_or(false)
                            {
                                let _ = send(&mut ws_tx, &ServerMessage::Error {
                                    message: "bot lacks the send_messages permission here".into(),
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
                                        bot: false,
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
                        if is_bot && !bot_can(&ctx.state, channel_id, &u.pubkey, Permission::AddReactions) {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "bot lacks the add_reactions permission here".into(),
                            }).await;
                            continue;
                        }
                        if !limiter.allow() {
                            continue;
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
                    ClientMessage::InstallBot { guild_id, bot_pubkey, name, permissions, intents } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        match ctx.state.install_bot(
                            guild_id, &bot_pubkey, &name, permissions, intents, &u.pubkey,
                        ) {
                            Ok((install, member)) => {
                                tracing::info!(%guild_id, bot = %bot_pubkey, by = %u.username, "bot installed");
                                // Surface the bot in every member's roster.
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::MemberJoin(member));
                                // Refresh the owner's Integrations panel.
                                let _ = send(&mut ws_tx, &ServerMessage::GuildIntegrations {
                                    guild_id,
                                    bots: ctx.state.guild_installs(guild_id),
                                }).await;
                                tracing::debug!(perms = ?install.permissions, intents = ?install.intents, "install grants");
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::UninstallBot { guild_id, bot_pubkey } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        // Capture recipients before removal drops the bot from
                        // the roster.
                        let targets = ctx.state.guild_member_pubkeys(guild_id);
                        match ctx.state.uninstall_bot(guild_id, &bot_pubkey, &u.pubkey) {
                            Ok(()) => {
                                tracing::info!(%guild_id, bot = %bot_pubkey, by = %u.username, "bot uninstalled");
                                ctx.state.deliver(
                                    targets,
                                    ServerMessage::MemberLeave { guild_id, user_pubkey: bot_pubkey },
                                );
                                let _ = send(&mut ws_tx, &ServerMessage::GuildIntegrations {
                                    guild_id,
                                    bots: ctx.state.guild_installs(guild_id),
                                }).await;
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::FetchIntegrations { guild_id } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        let is_owner = ctx
                            .state
                            .guilds
                            .get(&guild_id)
                            .map(|g| !g.owner_pubkey.is_empty() && g.owner_pubkey == u.pubkey)
                            .unwrap_or(false);
                        if !is_owner {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "only the owner can view this guild's integrations".into(),
                            }).await;
                            continue;
                        }
                        let _ = send(&mut ws_tx, &ServerMessage::GuildIntegrations {
                            guild_id,
                            bots: ctx.state.guild_installs(guild_id),
                        }).await;
                    }
                    ClientMessage::SetScreenShare { channel_id, sharing } => {
                        let Some(u) = user.as_ref() else { continue };
                        let sharers = ctx.state.set_screen_share(channel_id, &u.pubkey, sharing);
                        // Tell the channel's guild who's live.
                        if let Some(gid) = ctx.state.channel_guild(channel_id) {
                            let targets = ctx.state.guild_member_pubkeys(gid);
                            ctx.state.deliver(
                                targets,
                                ServerMessage::ScreenShareState { channel_id, sharers },
                            );
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
                                        livekit_url: livekit_url.clone(),
                                        token,
                                    },
                                )
                                .await;
                                // Also hand over a screen-share token (separate
                                // room) for the webview JS client.
                                let screen_name = format!("{} (screen)", u.username);
                                if let Ok(screen_token) = livekit::mint_screen_token(
                                    &ctx.livekit,
                                    &u.pubkey,
                                    &screen_name,
                                    channel_id,
                                ) {
                                    let _ = send(
                                        &mut ws_tx,
                                        &ServerMessage::ScreenToken {
                                            channel_id,
                                            livekit_url,
                                            token: screen_token,
                                        },
                                    )
                                    .await;
                                }
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
                        broadcast_screen_clear(&ctx.state, &u.pubkey);
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
                // Bots see a filtered stream: only events for guilds they're
                // installed in, only the intents they were granted, and message
                // content withheld unless the privileged MessageContent intent
                // is present.
                let out = if is_bot {
                    let Some(u) = user.as_ref() else { continue };
                    match filter_for_bot(&ctx.state, &u.pubkey, &env.msg) {
                        Some(m) => m,
                        None => continue,
                    }
                } else {
                    env.msg
                };
                if send(&mut ws_tx, &out).await.is_err() {
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
        broadcast_screen_clear(&ctx.state, &u.pubkey);
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

/// Drop a user from every screen-share set and tell each affected channel's
/// guild members the updated sharer list.
fn broadcast_screen_clear(state: &crate::state::AppState, pubkey: &str) {
    for (channel_id, sharers) in state.clear_user_screen_shares(pubkey) {
        if let Some(gid) = state.channel_guild(channel_id) {
            let targets = state.guild_member_pubkeys(gid);
            state.deliver(targets, ServerMessage::ScreenShareState { channel_id, sharers });
        }
    }
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

/// True if `bot_pubkey` is installed in `channel_id`'s guild with `perm`. DM
/// channels (no guild) never grant a bot anything.
fn bot_can(
    state: &crate::state::AppState,
    channel_id: crate::protocol::Id,
    bot_pubkey: &str,
    perm: Permission,
) -> bool {
    state
        .channel_guild(channel_id)
        .and_then(|gid| state.bot_install(gid, bot_pubkey))
        .map(|i| i.has_permission(perm))
        .unwrap_or(false)
}

/// Adapt an outbound frame for a bot connection. Returns `None` to drop the
/// frame (outside the bot's installed/intent-granted scope), or the (possibly
/// content-stripped) frame to deliver. This is the data-minimization boundary:
/// by default a bot learns that a message happened, not what it said.
fn filter_for_bot(
    state: &crate::state::AppState,
    bot_pubkey: &str,
    msg: &ServerMessage,
) -> Option<ServerMessage> {
    match msg {
        ServerMessage::MessageCreate(m) => {
            // DMs have no guild — bots never receive them.
            let gid = state.channel_guild(m.channel_id)?;
            let install = state.bot_install(gid, bot_pubkey)?;
            if !install.has_intent(Intent::GuildMessages) {
                return None;
            }
            let mut m = m.clone();
            if !install.has_intent(Intent::MessageContent) {
                m.content = String::new();
                m.image = None;
            }
            Some(ServerMessage::MessageCreate(m))
        }
        ServerMessage::ReactionUpdate { channel_id, .. } => {
            let gid = state.channel_guild(*channel_id)?;
            let install = state.bot_install(gid, bot_pubkey)?;
            install.has_intent(Intent::Reactions).then(|| msg.clone())
        }
        ServerMessage::MemberJoin(member) => {
            let install = state.bot_install(member.guild_id, bot_pubkey)?;
            install.has_intent(Intent::Members).then(|| msg.clone())
        }
        ServerMessage::MemberLeave { guild_id, .. } => {
            let install = state.bot_install(*guild_id, bot_pubkey)?;
            install.has_intent(Intent::Members).then(|| msg.clone())
        }
        // Errors are useful feedback for a bot (e.g. permission denied).
        ServerMessage::Error { .. } => Some(msg.clone()),
        // Everything else (typing, voice, screen share, profiles, the public
        // catalog, guild metadata, DMs, integrations) is outside a bot's event
        // surface — never delivered.
        _ => None,
    }
}

/// Sliding-window limiter for message-producing actions. Bounds how fast any
/// one connection can append/broadcast, which is the practical defense against
/// a spammy or compromised bot.
struct RateLimiter {
    hits: VecDeque<Instant>,
}

impl RateLimiter {
    const WINDOW: Duration = Duration::from_secs(10);
    const LIMIT: usize = 30;

    fn new() -> Self {
        Self {
            hits: VecDeque::new(),
        }
    }

    /// Record an action; returns false if the window is already full.
    fn allow(&mut self) -> bool {
        let now = Instant::now();
        while let Some(front) = self.hits.front() {
            if now.duration_since(*front) > Self::WINDOW {
                self.hits.pop_front();
            } else {
                break;
            }
        }
        if self.hits.len() >= Self::LIMIT {
            return false;
        }
        self.hits.push_back(now);
        true
    }
}
