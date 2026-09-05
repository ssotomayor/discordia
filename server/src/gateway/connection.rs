use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures_util::{SinkExt, StreamExt};

use crate::AppContext;
use crate::auth;
use crate::livekit;
use crate::protocol::{
    ClientMessage, EmojiBlob, Id, Intent, Member, Permission, ServerMessage, User,
};

pub async fn handle_connection(
    socket: WebSocket,
    ctx: Arc<AppContext>,
    client_host: Option<String>,
    peer: Option<std::net::IpAddr>,
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (conn_id, mut outbound_rx) = match ctx.state.register_conn(peer) {
        Ok(registered) => registered,
        Err(reason) => {
            let _ = send(
                &mut ws_tx,
                &ServerMessage::Error {
                    message: reason.into(),
                },
            )
            .await;
            return;
        }
    };

    let nonce = auth::fresh_nonce();
    if send(
        &mut ws_tx,
        &ServerMessage::Hello {
            nonce: nonce.clone(),
        },
    )
    .await
    .is_err()
    {
        return;
    }

    let mut user: Option<User> = None;
    let mut is_bot = false;
    let mut limiter = RateLimiter::new(WRITE_LIMIT, RATE_WINDOW);
    let mut reads = RateLimiter::new(READ_LIMIT, RATE_WINDOW);
    let mut signals = RateLimiter::new(SIGNAL_LIMIT, RATE_WINDOW);
    let mut flood = RateLimiter::new(FLOOD_LIMIT, RATE_WINDOW);
    let mut activities = RateLimiter::new(ACTIVITY_LIMIT, RATE_WINDOW);
    let mut failed_identifies = 0u32;
    let identify_by = tokio::time::Instant::now() + IDENTIFY_TIMEOUT;
    let mut shutdown = ctx.shutdown.subscribe();

    loop {
        tokio::select! {
            incoming = ws_rx.next() => {
                let Some(Ok(msg)) = incoming else { break };

                // Ahead of the parse and of the frame kinds that skip it: a
                // peer that never sends valid JSON is still spending our time,
                // and counting only what parses leaves the flood uncounted.
                if !flood.allow() {
                    let _ = send(&mut ws_tx, &ServerMessage::Error {
                        message: RATE_LIMITED.into(),
                    }).await;
                    break;
                }

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
                    ClientMessage::Identify {
                        username,
                        pubkey,
                        signature,
                        origin,
                        bot,
                        client_version,
                    } => {
                        if user.is_some() {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "already identified".into(),
                            }).await;
                            continue;
                        }
                        let username = sanitize_username(&username);
                        let refused = if origin.is_empty() {
                            Some(
                                "identify rejected: this client is too old — a login must \
                                 name the server address it dialed"
                                    .to_string(),
                            )
                        } else if !ctx.identities.contains(&origin) {
                            Some(format!(
                                "identify rejected: this server does not answer to '{origin}'. \
                                 A host reached through another name has to list it in \
                                 DIOXUSFUN_PUBLIC_HOSTS"
                            ))
                        } else {
                            auth::verify_identify(&pubkey, &signature, &nonce, &origin, &username)
                                .err()
                                .map(|e| format!("identify rejected: {e}"))
                        };
                        if let Some(message) = refused {
                            let _ = send(&mut ws_tx, &ServerMessage::Error { message }).await;
                            failed_identifies += 1;
                            if failed_identifies >= MAX_IDENTIFY_ATTEMPTS {
                                break;
                            }
                            continue;
                        }
                        if let Err(message) = ctx.state.identify_conn(conn_id, &pubkey) {
                            let _ = send(&mut ws_tx, &ServerMessage::Error { message }).await;
                            failed_identifies += 1;
                            if failed_identifies >= MAX_IDENTIFY_ATTEMPTS {
                                break;
                            }
                            continue;
                        }
                        let new_user = User { pubkey: pubkey.clone(), username };
                        ctx.state.remember_user(&new_user).await;
                        is_bot = bot;
                        let ready = if is_bot {
                            ctx.state.snapshot_for_bot(&new_user)
                        } else {
                            ctx.state.snapshot_for(&new_user).await
                        };
                        if send(&mut ws_tx, &ready).await.is_err() {
                            break;
                        }
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
                        let client_version = sanitize_client_version(&client_version);
                        let client_version = if client_version.is_empty() {
                            "unknown".to_string()
                        } else {
                            client_version
                        };
                        tracing::info!(
                            user = ?new_user.username,
                            pubkey = %new_user.pubkey,
                            version = ?client_version,
                            "identified"
                        );
                        user = Some(new_user);
                    }
                    ClientMessage::FetchMessages { channel_id, limit, before_ms } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        let forbidden = if let Some(gid) = ctx.state.channel_guild(channel_id) {
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
                        if !reads.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        let history = ctx.state.history(channel_id, limit.clamp(1, 200), before_ms).await;
                        if send(&mut ws_tx, &ServerMessage::MessageHistory {
                            channel_id,
                            messages: history,
                        }).await.is_err() {
                            break;
                        }
                    }
                    ClientMessage::SendMessage { channel_id, content, image, reply_to } => {
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
                        if let Some(img) = &image
                            && (!img.starts_with("data:image/")
                                || img.len() > crate::state::MAX_IMAGE_LEN)
                            {
                                let _ = send(&mut ws_tx, &ServerMessage::Error {
                                    message: "image must be a data:image/* URL under the size limit".into(),
                                }).await;
                                continue;
                            }
                        if content.len() > 2000 || (content.is_empty() && image.is_none()) {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "message must be 1..=2000 chars (or include an image)".into(),
                            }).await;
                            continue;
                        }

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
                            if ctx.state.channel_read_only(channel_id) {
                                let allowed = if is_bot {
                                    ctx.state
                                        .bot_install(gid, &author.pubkey)
                                        .map(|i| {
                                            i.has_permission(Permission::ManageMessages)
                                                || i.has_permission(Permission::ManageChannels)
                                        })
                                        .unwrap_or(false)
                                } else {
                                    let perms =
                                        ctx.state.effective_permissions(gid, &author.pubkey);
                                    perms.contains(&Permission::ManageMessages)
                                        || perms.contains(&Permission::ManageChannels)
                                };
                                if !allowed {
                                    let _ = send(&mut ws_tx, &ServerMessage::Error {
                                        message: "this channel is read-only".into(),
                                    }).await;
                                    continue;
                                }
                            }
                            let mod_exempt = {
                                let perms = ctx.state.effective_permissions(gid, &author.pubkey);
                                is_bot
                                    || perms.contains(&Permission::ManageMessages)
                                    || perms.contains(&Permission::ManageChannels)
                            };
                            if !mod_exempt
                                && let Err(wait) = ctx.state.slowmode_check(channel_id, &author.pubkey) {
                                    let _ = send(&mut ws_tx, &ServerMessage::Error {
                                        message: format!("slowmode: wait {wait}s before posting again"),
                                    }).await;
                                    continue;
                                }
                            match ctx.state.push_message(channel_id, author, content, image, reply_to).await {
                                Ok(msg) => {
                                    let author_pk = msg.author.pubkey.clone();
                                    let targets = ctx.state.guild_member_pubkeys(gid);
                                    ctx.state.deliver(targets, ServerMessage::MessageCreate(msg));
                                    // A bot earns nothing: it is installed, not
                                    // present, and a level badge on one says a
                                    // person was here.
                                    if !is_bot
                                        && let Some(member) = ctx.state.award_xp(
                                            gid,
                                            Some(channel_id),
                                            &author_pk,
                                            crate::protocol::XpAction::Message,
                                        ).await
                                    {
                                        let targets = ctx.state.guild_member_pubkeys(gid);
                                        ctx.state.deliver(targets, ServerMessage::MemberUpdate(member));
                                    }
                                }
                                Err(message) => {
                                    let _ = send(&mut ws_tx, &ServerMessage::Error { message }).await;
                                }
                            }
                        } else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "unknown channel".into(),
                            }).await;
                        }
                    }
                    ClientMessage::CreateGuild { name, template } => {
                        let Some(creator) = user.clone() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        let name = match crate::protocol::sanitize_name("guild", &name, 64) {
                            Ok(name) => name,
                            Err(message) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message }).await;
                                continue;
                            }
                        };
                        let (guild, channels, member, roles) =
                            match ctx.state.create_guild(&name, template.as_deref(), &creator).await {
                                Ok(created) => created,
                                Err(message) => {
                                    let _ = send(&mut ws_tx, &ServerMessage::Error { message }).await;
                                    continue;
                                }
                            };
                        tracing::info!(guild = ?guild.name, by = ?creator.username, "guild created");
                        if send(&mut ws_tx, &ServerMessage::GuildJoined {
                            guild,
                            channels,
                            members: vec![member],
                            roles,
                            emojis: Vec::new(),
                            voice_states: Vec::new(),
                        }).await.is_err() {
                            break;
                        }
                    }
                    ClientMessage::JoinGuild { guild_id, accept, pow_nonce } => {
                        let Some(joiner) = user.clone() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "rate limited: slow down".into(),
                            }).await;
                            continue;
                        }
                        match check_join_gate(&ctx.state, guild_id, &joiner.pubkey, accept, pow_nonce.as_deref(), None, &nonce) {
                            Gate::Reject(msg) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: msg }).await;
                                continue;
                            }
                            Gate::Challenge(ch) => {
                                let _ = send(&mut ws_tx, &ch).await;
                                continue;
                            }
                            Gate::Proceed => {}
                        }
                        match ctx.state.join_guild(guild_id, &joiner).await {
                            Ok(bundle) => {
                                if deliver_join(&ctx.state, &mut ws_tx, &joiner, bundle).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::JoinByInvite { code, accept, pow_nonce } => {
                        let Some(joiner) = user.clone() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "rate limited: slow down".into(),
                            }).await;
                            continue;
                        }
                        let Some(guild_id) = ctx.state.invite_guild(&code) else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "unknown or expired invite code".into(),
                            }).await;
                            continue;
                        };
                        match check_join_gate(&ctx.state, guild_id, &joiner.pubkey, accept, pow_nonce.as_deref(), Some(code.clone()), &nonce) {
                            Gate::Reject(msg) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: msg }).await;
                                continue;
                            }
                            Gate::Challenge(ch) => {
                                let _ = send(&mut ws_tx, &ch).await;
                                continue;
                            }
                            Gate::Proceed => {}
                        }
                        match ctx.state.join_by_invite(&code, &joiner).await {
                            Ok(bundle) => {
                                if deliver_join(&ctx.state, &mut ws_tx, &joiner, bundle).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
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
                        let targets = ctx.state.guild_member_pubkeys(guild_id);
                        match ctx.state.delete_guild(guild_id, &u.pubkey).await {
                            Ok(()) => {
                                tracing::info!(%guild_id, by = ?u.username, "guild deleted");
                                ctx.state.deliver(targets, ServerMessage::GuildDelete { guild_id });
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::SetProfile { avatar, banner, bio, status, custom_status } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        let avatar = match avatar.map(|a| ctx.state.image_reference(&u.pubkey, &a)).transpose() {
                            Ok(a) => a,
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error {
                                    message: format!("avatar: {e}"),
                                }).await;
                                continue;
                            }
                        };
                        let banner = match banner.map(|b| ctx.state.image_reference(&u.pubkey, &b)).transpose() {
                            Ok(b) => b,
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error {
                                    message: format!("banner: {e}"),
                                }).await;
                                continue;
                            }
                        };
                        let bio = bio.map(|b| crate::protocol::sanitize_paragraph(&b, 280));
                        let custom_status =
                            custom_status.map(|c| crate::protocol::sanitize_line(&c, 80));
                        let status = status.map(|s| crate::protocol::sanitize_line(&s, 32));
                        if let Some(s) = status.as_deref()
                            && !crate::protocol::PRESENCES.contains(&s)
                        {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: format!(
                                    "status must be one of {}",
                                    crate::protocol::PRESENCES.join(", ")
                                ),
                            }).await;
                            continue;
                        }
                        let profile = ctx.state.set_profile(
                            &u.pubkey, avatar, banner, bio, status, custom_status,
                        ).await;
                        ctx.state.broadcast(ServerMessage::ProfileUpdate(profile));
                    }
                    ClientMessage::SetActivity { activity } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !activities.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        let update = ctx.state.set_activity(&u.pubkey, activity);
                        ctx.state.deliver(
                            ctx.state.peers_of(&u.pubkey),
                            ServerMessage::ActivityUpdate(update),
                        );
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
                            continue;
                        }
                        if is_bot && !bot_can(&ctx.state, channel_id, &u.pubkey, Permission::AddReactions) {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "bot lacks the add_reactions permission here".into(),
                            }).await;
                            continue;
                        }
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        let emoji: String = emoji.chars().take(8).collect();
                        if let Some(reactions) =
                            ctx.state.toggle_reaction(channel_id, message_id, &emoji, &u.pubkey).await
                        {
                            ctx.state.deliver(
                                audience,
                                ServerMessage::ReactionUpdate { channel_id, message_id, reactions },
                            );
                            if !is_bot
                                && let Some(gid) = ctx.state.channel_guild(channel_id)
                                && let Some(member) = ctx.state.award_xp(
                                    gid,
                                    Some(channel_id),
                                    &u.pubkey,
                                    crate::protocol::XpAction::Reaction,
                                ).await
                            {
                                let targets = ctx.state.guild_member_pubkeys(gid);
                                ctx.state.deliver(targets, ServerMessage::MemberUpdate(member));
                            }
                        }
                    }
                    ClientMessage::Typing { channel_id } => {
                        let Some(u) = user.as_ref() else { continue };
                        if !signals.allow() {
                            continue;
                        }
                        let Some(audience) = channel_audience(&ctx.state, channel_id) else {
                            continue;
                        };
                        if !audience.iter().any(|p| p == &u.pubkey) {
                            continue;
                        }
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
                    ClientMessage::CreateGuildEmoji { guild_id, shortcode, image } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !image.starts_with("data:image/") || image.len() > MAX_EMOJI_DATA_LEN {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "emoji must be an image under 256 KB".into(),
                            }).await;
                            continue;
                        }
                        let stored = match ctx.state.store_upload(&u.pubkey, &image) {
                            Ok(stored) => stored,
                            Err(message) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message }).await;
                                continue;
                            }
                        };
                        let address = stored.strip_prefix("media:").unwrap_or(&stored).to_string();
                        match ctx.state.create_emoji(guild_id, &shortcode, address, &u.pubkey).await {
                            Ok(_) => broadcast_emojis(&ctx.state, guild_id),
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::RenameGuildEmoji { guild_id, emoji_id, shortcode } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        match ctx.state.rename_emoji(guild_id, emoji_id, &shortcode, &u.pubkey).await {
                            Ok(_) => broadcast_emojis(&ctx.state, guild_id),
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::DeleteGuildEmoji { guild_id, emoji_id } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        match ctx.state.delete_emoji(guild_id, emoji_id, &u.pubkey).await {
                            Ok(()) => broadcast_emojis(&ctx.state, guild_id),
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::FetchEmoji { images } => {
                        if user.is_none() {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        }
                        if !reads.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        // A blob past the budget is left out, not blanked: an
                        // empty answer means "no such blob" and the client stops
                        // asking, while an absent one is asked for again later.
                        let mut budget = MAX_BLOB_RESPONSE_BYTES;
                        let blobs: Vec<EmojiBlob> = images
                            .into_iter()
                            .take(MAX_EMOJI_FETCH)
                            .filter_map(|image| {
                                let data_url = ctx
                                    .state
                                    .media
                                    .inline(&format!("media:{image}"))
                                    .unwrap_or_default();
                                if data_url.len() > budget {
                                    return None;
                                }
                                budget -= data_url.len();
                                Some(EmojiBlob { image, data_url })
                            })
                            .collect();
                        if send(&mut ws_tx, &ServerMessage::EmojiBlobs { blobs }).await.is_err() {
                            break;
                        }
                    }
                    ClientMessage::SetGuildAccent { guild_id, accent } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        let accent = accent.filter(|a| is_hex_color(a));
                        match ctx.state.set_guild_accent(guild_id, accent, &u.pubkey).await {
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
                        ).await {
                            Ok((install, member)) => {
                                tracing::info!(%guild_id, bot = ?bot_pubkey, by = ?u.username, "bot installed");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::MemberJoin(member));
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
                        let targets = ctx.state.guild_member_pubkeys(guild_id);
                        match ctx.state.uninstall_bot(guild_id, &bot_pubkey, &u.pubkey).await {
                            Ok(()) => {
                                tracing::info!(%guild_id, bot = ?bot_pubkey, by = ?u.username, "bot uninstalled");
                                ctx.state.deliver(
                                    targets,
                                    ServerMessage::MemberRemove { guild_id, user_pubkey: bot_pubkey },
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
                        if let Err(e) = ctx.state.require_permission(
                            guild_id,
                            &u.pubkey,
                            Permission::ManageGuild,
                        ) {
                            let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            continue;
                        }
                        let _ = send(&mut ws_tx, &ServerMessage::GuildIntegrations {
                            guild_id,
                            bots: ctx.state.guild_installs(guild_id),
                        }).await;
                    }
                    ClientMessage::SetScreenShare { channel_id: _, sharing } => {
                        let Some(u) = user.as_ref() else { continue };
                        if !signals.allow() {
                            continue;
                        }
                        let Some(vs) = ctx.state.update_screen_share(&u.pubkey, sharing) else {
                            continue;
                        };
                        let targets = ctx.state.guild_member_pubkeys(vs.guild_id);
                        let channel = vs.channel_id;
                        ctx.state
                            .deliver(targets.clone(), ServerMessage::VoiceStateUpdate(vs));
                        if let Some(cid) = channel {
                            ctx.state.deliver(
                                targets,
                                ServerMessage::ScreenShareState {
                                    channel_id: cid,
                                    sharers: ctx.state.screen_sharers_in(cid),
                                },
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
                        match livekit::voice_token(&ctx.livekit, &u.pubkey, &u.username, channel_id).await {
                            Ok(token) => {
                                let livekit_url =
                                    ctx.livekit.url_for_client(client_host.as_deref(), peer);
                                let _ = send(
                                    &mut ws_tx,
                                    &ServerMessage::VoiceToken {
                                        channel_id,
                                        livekit_url: livekit_url.clone(),
                                        token,
                                    },
                                )
                                .await;
                                let screen_name = format!("{} (screen)", u.username);
                                let screen_token = livekit::screen_token_as(
                                    &ctx.livekit,
                                    &u.pubkey,
                                    &screen_name,
                                    channel_id,
                                    true,
                                )
                                .await;
                                match screen_token {
                                    Ok(screen_token) => {
                                        let audio_token = optional_screen_token(
                                            &ctx.livekit,
                                            OptionalScreen::Audio,
                                            &u.pubkey,
                                            &screen_name,
                                            channel_id,
                                            &mut ws_tx,
                                        )
                                        .await;
                                        let video_token = optional_screen_token(
                                            &ctx.livekit,
                                            OptionalScreen::Video,
                                            &u.pubkey,
                                            &screen_name,
                                            channel_id,
                                            &mut ws_tx,
                                        )
                                        .await;
                                        let _ = send(
                                            &mut ws_tx,
                                            &ServerMessage::ScreenToken {
                                                channel_id,
                                                livekit_url,
                                                token: screen_token,
                                                audio_token,
                                                video_token,
                                            },
                                        )
                                        .await;
                                    }
                                    Err(err) => {
                                        tracing::error!(
                                            %err, %channel_id,
                                            "screen token mint failed"
                                        );
                                        let _ = send(
                                            &mut ws_tx,
                                            &ServerMessage::Error {
                                                message: format!(
                                                    "screen-share token mint failed: {err}"
                                                ),
                                            },
                                        )
                                        .await;
                                    }
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
                        let was_sharing = sharing_in(&ctx.state, &u.pubkey);
                        if let Some(cleared) = ctx.state.clear_voice(&u.pubkey) {
                            let targets = ctx.state.guild_member_pubkeys(cleared.guild_id);
                            ctx.state.deliver(targets, ServerMessage::VoiceStateUpdate(cleared));
                        }
                        if let Some((gid, cid)) = was_sharing {
                            broadcast_screen_state(&ctx.state, gid, cid);
                        }
                    }
                    ClientMessage::SetVoiceMute { muted, deafened } => {
                        let Some(u) = user.as_ref() else { continue };
                        if !signals.allow() {
                            continue;
                        }
                        if let Some(state) = ctx.state.update_voice_flags(&u.pubkey, muted, deafened) {
                            let targets = ctx.state.guild_member_pubkeys(state.guild_id);
                            ctx.state.deliver(targets, ServerMessage::VoiceStateUpdate(state));
                        }
                    }
                    ClientMessage::SetSpeaking { speaking } => {
                        let Some(u) = user.as_ref() else { continue };
                        if !signals.allow() {
                            continue;
                        }
                        if let Some(state) = ctx.state.update_speaking(&u.pubkey, speaking) {
                            let targets = ctx.state.guild_member_pubkeys(state.guild_id);
                            ctx.state.deliver(targets, ServerMessage::VoiceStateUpdate(state));
                        }
                    }
                    ClientMessage::ShareMediaKey { channel_id, to, epoch, blob } => {
                        let Some(u) = user.as_ref() else { continue };
                        if blob.len() > MAX_MEDIA_KEY_BLOB || !signals.allow() {
                            continue;
                        }
                        let Some(guild_id) = ctx.state.channel_guild(channel_id) else {
                            tracing::warn!(%channel_id, "media key for a channel with no guild");
                            continue;
                        };
                        let members = ctx.state.guild_member_pubkeys(guild_id);
                        if !members.contains(&u.pubkey) || !members.contains(&to) {
                            tracing::warn!(
                                from = %u.pubkey, %to, %guild_id,
                                "refusing to route a media key: sender or recipient is not a guild member"
                            );
                            continue;
                        }
                        tracing::info!(from = %u.pubkey, %to, epoch, "routing a media key");
                        ctx.state.deliver(
                            vec![to],
                            ServerMessage::MediaKey {
                                channel_id,
                                from: u.pubkey.clone(),
                                epoch,
                                blob,
                            },
                        );
                    }
                    ClientMessage::SetCamera { on } => {
                        let Some(u) = user.as_ref() else { continue };
                        if !signals.allow() {
                            continue;
                        }
                        if let Some(state) = ctx.state.update_camera(&u.pubkey, on) {
                            let targets = ctx.state.guild_member_pubkeys(state.guild_id);
                            ctx.state.deliver(targets, ServerMessage::VoiceStateUpdate(state));
                        }
                    }
                    ClientMessage::CreateRole { guild_id, name, color, permissions } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.create_role(guild_id, &name, color, permissions, &u.pubkey).await {
                            Ok(role) => {
                                tracing::info!(%guild_id, role = ?role.name, by = ?u.username, "role created");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildRoles {
                                    guild_id,
                                    roles: ctx.state.guild_roles(guild_id),
                                });
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::UpdateRole { guild_id, role_id, name, color, permissions } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.update_role(guild_id, role_id, &name, color, permissions, &u.pubkey).await {
                            Ok(_) => {
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildRoles {
                                    guild_id,
                                    roles: ctx.state.guild_roles(guild_id),
                                });
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::DeleteRole { guild_id, role_id } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.delete_role(guild_id, role_id, &u.pubkey).await {
                            Ok(changed_members) => {
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets.clone(), ServerMessage::GuildRoles {
                                    guild_id,
                                    roles: ctx.state.guild_roles(guild_id),
                                });
                                for member in changed_members {
                                    ctx.state.deliver(
                                        targets.clone(),
                                        ServerMessage::MemberUpdate(member),
                                    );
                                }
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::AssignRole { guild_id, role_id, user_pubkey } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.set_member_role(guild_id, role_id, &user_pubkey, true, &u.pubkey).await {
                            Ok(member) => {
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::MemberUpdate(member));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::UnassignRole { guild_id, role_id, user_pubkey } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.set_member_role(guild_id, role_id, &user_pubkey, false, &u.pubkey).await {
                            Ok(member) => {
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::MemberUpdate(member));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::SetGuildVisibility { guild_id, visibility } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        match ctx.state.set_guild_visibility(guild_id, visibility, &u.pubkey).await {
                            Ok(guild) => {
                                tracing::info!(%guild_id, ?visibility, by = ?u.username, "guild visibility set");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildUpdate(guild));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::CreateInvite { guild_id, rotate, expires_in_secs, max_uses } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.get_or_create_invite(guild_id, rotate, expires_in_secs, max_uses, &u.pubkey).await {
                            Ok(invite) => {
                                let _ = send(&mut ws_tx, &ServerMessage::GuildInvite {
                                    guild_id,
                                    code: invite.code,
                                    expires_at_ms: invite.expires_at_ms,
                                    max_uses: invite.max_uses,
                                    uses: invite.uses,
                                }).await;
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::KickMember { guild_id, user_pubkey } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        let was_sharing = sharing_in(&ctx.state, &user_pubkey);
                        match ctx.state.kick_member(guild_id, &user_pubkey, &u.pubkey).await {
                            Ok(cleared_voice) => {
                                tracing::info!(%guild_id, target = ?user_pubkey, by = ?u.username, "member kicked");
                                ctx.state.audit(guild_id, &u.pubkey, "kick", &user_pubkey, "").await;
                                removal_broadcasts(&ctx.state, guild_id, &user_pubkey, true, cleared_voice, was_sharing);
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::BanMember { guild_id, user_pubkey } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        let was_member = ctx.state.is_guild_member(guild_id, &user_pubkey);
                        let was_sharing = sharing_in(&ctx.state, &user_pubkey);
                        match ctx.state.ban_member(guild_id, &user_pubkey, &u.pubkey).await {
                            Ok(cleared_voice) => {
                                tracing::info!(%guild_id, target = ?user_pubkey, by = ?u.username, "member banned");
                                ctx.state.audit(guild_id, &u.pubkey, "ban", &user_pubkey, "").await;
                                removal_broadcasts(&ctx.state, guild_id, &user_pubkey, was_member, cleared_voice, was_sharing);
                                if let Ok(users) = ctx.state.ban_list(guild_id, &u.pubkey) {
                                    let _ = send(&mut ws_tx, &ServerMessage::GuildBans {
                                        guild_id,
                                        users,
                                    }).await;
                                }
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::UnbanMember { guild_id, user_pubkey } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        match ctx.state.unban_member(guild_id, &user_pubkey, &u.pubkey).await {
                            Ok(()) => {
                                ctx.state.audit(guild_id, &u.pubkey, "unban", &user_pubkey, "").await;
                                if let Ok(users) = ctx.state.ban_list(guild_id, &u.pubkey) {
                                    let _ = send(&mut ws_tx, &ServerMessage::GuildBans {
                                        guild_id,
                                        users,
                                    }).await;
                                }
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::FetchBans { guild_id } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !reads.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.ban_list(guild_id, &u.pubkey) {
                            Ok(users) => {
                                let _ = send(&mut ws_tx, &ServerMessage::GuildBans {
                                    guild_id,
                                    users,
                                }).await;
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::LeaveGuild { guild_id } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        let was_sharing = sharing_in(&ctx.state, &u.pubkey);
                        match ctx.state.leave_guild(guild_id, &u.pubkey).await {
                            Ok(cleared_voice) => {
                                tracing::info!(%guild_id, by = ?u.username, "left guild");
                                removal_broadcasts(&ctx.state, guild_id, &u.pubkey, true, cleared_voice, was_sharing);
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::CreateChannel { guild_id, name, kind, topic } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.create_channel(guild_id, &name, kind, topic, &u.pubkey).await {
                            Ok(channel) => {
                                tracing::info!(%guild_id, channel = ?channel.name, by = ?u.username, "channel created");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::ChannelCreate(channel));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::UpdateUsername { username } => {
                        let Some(u) = user.as_mut() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        let username = sanitize_username(&username);
                        if username == u.username {
                            continue;
                        }
                        u.username = username.clone();
                        for member in ctx.state.rename_user(&u.pubkey, &username).await {
                            let targets = ctx.state.guild_member_pubkeys(member.guild_id);
                            ctx.state.deliver(targets, ServerMessage::MemberUpdate(member));
                        }
                    }
                    ClientMessage::ReorderChannels { guild_id, positions } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.reorder_channels(guild_id, &positions, &u.pubkey).await {
                            Ok(channels) => {
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                for channel in channels {
                                    ctx.state.deliver(targets.clone(), ServerMessage::ChannelUpdate(channel));
                                }
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::UpdateChannel { channel_id, name, topic, read_only, position, slowmode_secs } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.update_channel(channel_id, &name, topic, read_only, position, slowmode_secs, &u.pubkey).await {
                            Ok(channel) => {
                                let targets = ctx.state.guild_member_pubkeys(channel.guild_id);
                                ctx.state.deliver(targets, ServerMessage::ChannelUpdate(channel));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::DeleteChannel { channel_id } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.delete_channel(channel_id, &u.pubkey).await {
                            Ok((guild_id, evicted)) => {
                                tracing::info!(%guild_id, %channel_id, by = ?u.username, "channel deleted");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(
                                    targets.clone(),
                                    ServerMessage::ChannelDelete { guild_id, channel_id },
                                );
                                for vs in evicted {
                                    ctx.state.deliver(
                                        targets.clone(),
                                        ServerMessage::VoiceStateUpdate(vs),
                                    );
                                }
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::DeleteMessage { channel_id, message_id } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.delete_message(channel_id, message_id, &u.pubkey).await {
                            Ok(()) => {
                                if let Some(audience) = channel_audience(&ctx.state, channel_id) {
                                    ctx.state.deliver(
                                        audience,
                                        ServerMessage::MessageDelete { channel_id, message_id },
                                    );
                                }
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::TransferOwnership { guild_id, new_owner_pubkey } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        match ctx.state.transfer_ownership(guild_id, &new_owner_pubkey, &u.pubkey).await {
                            Ok(guild) => {
                                tracing::info!(%guild_id, to = ?new_owner_pubkey, by = ?u.username, "ownership transferred");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildUpdate(guild));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::SetGuildProfile { guild_id, name, description, icon_image, banner } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        let name = match name
                            .map(|n| crate::protocol::sanitize_name("guild", &n, 64))
                            .transpose()
                        {
                            Ok(name) => name,
                            Err(message) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message }).await;
                                continue;
                            }
                        };
                        match ctx.state.set_guild_profile(guild_id, name, description, icon_image, banner, &u.pubkey).await {
                            Ok(guild) => {
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildUpdate(guild));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::SetGuildRetention { guild_id, days } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        match ctx.state.set_guild_retention(guild_id, days, &u.pubkey).await {
                            Ok(guild) => {
                                tracing::info!(%guild_id, ?days, by = ?u.username, "guild retention set");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildUpdate(guild));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::SetJoinGate { guild_id, gate, rules } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        match ctx.state.set_join_gate(guild_id, gate, rules, &u.pubkey).await {
                            Ok(guild) => {
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildUpdate(guild));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::SetGuildLeveling { guild_id, leveling } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.set_guild_leveling(guild_id, leveling, &u.pubkey).await {
                            Ok(guild) => {
                                tracing::info!(%guild_id, by = ?u.username, "guild leveling set");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildUpdate(guild));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::SetPanicMode { guild_id, on } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        match ctx.state.set_panic_mode(guild_id, on, &u.pubkey).await {
                            Ok(guild) => {
                                tracing::info!(%guild_id, on, by = ?u.username, "panic mode set");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildUpdate(guild));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::FetchAuditLog { guild_id } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !reads.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        match ctx.state.audit_log(guild_id, &u.pubkey).await {
                            Ok(entries) => {
                                let _ = send(&mut ws_tx, &ServerMessage::AuditLog { guild_id, entries }).await;
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::FetchCatalog { offset, limit } => {
                        const DEFAULT_PAGE: u32 = 100;
                        if !reads.allow() {
                            reject_rate_limited(&mut ws_tx).await;
                            continue;
                        }
                        const MAX_PAGE: u32 = 500;
                        let limit = if limit == 0 { DEFAULT_PAGE } else { limit.min(MAX_PAGE) };
                        let (guilds, total) = ctx.state.guild_catalog_page(offset, limit);
                        let _ = send(&mut ws_tx, &ServerMessage::GuildCatalog { guilds, offset, total }).await;
                    }
                }
            }

            _ = tokio::time::sleep_until(identify_by), if user.is_none() => {
                let _ = send(&mut ws_tx, &ServerMessage::Error {
                    message: "identify timed out".into(),
                }).await;
                break;
            }

            // The reason travels in the close frame rather than as a message:
            // the client is about to lose the socket either way, and a frame
            // it might not read before the close says nothing.
            _ = host_stopped(&mut shutdown) => {
                let _ = ws_tx.send(WsMessage::Close(Some(axum::extract::ws::CloseFrame {
                    code: axum::extract::ws::close_code::AWAY,
                    reason: HOST_STOPPED.into(),
                }))).await;
                break;
            }

            outbound = outbound_rx.recv() => {
                let Some(msg) = outbound else { break };
                let out = if is_bot {
                    let Some(u) = user.as_ref() else { continue };
                    match filter_for_bot(&ctx.state, &u.pubkey, &msg) {
                        Some(m) => m,
                        None => continue,
                    }
                } else {
                    msg
                };
                if send(&mut ws_tx, &out).await.is_err() {
                    break;
                }
            }
        }
    }

    ctx.state
        .unregister_conn(conn_id, user.as_ref().map(|u| u.pubkey.as_str()));

    if let Some(u) = user {
        let was_sharing = sharing_in(&ctx.state, &u.pubkey);
        if let Some(cleared) = ctx.state.clear_voice(&u.pubkey) {
            let targets = ctx.state.guild_member_pubkeys(cleared.guild_id);
            ctx.state
                .deliver(targets, ServerMessage::VoiceStateUpdate(cleared));
        }
        if let Some((gid, cid)) = was_sharing {
            broadcast_screen_state(&ctx.state, gid, cid);
        }
        if ctx.state.clear_activity(&u.pubkey) {
            ctx.state.deliver(
                ctx.state.peers_of(&u.pubkey),
                ServerMessage::ActivityUpdate(crate::protocol::UserActivity {
                    pubkey: u.pubkey.clone(),
                    activity: None,
                }),
            );
        }
        for (guild_id, user_pubkey) in ctx.state.mark_offline(&u.pubkey) {
            let targets = ctx.state.guild_member_pubkeys(guild_id);
            ctx.state.deliver(
                targets,
                ServerMessage::MemberLeave {
                    guild_id,
                    user_pubkey,
                },
            );
        }
        tracing::info!(user = ?u.username, "client disconnected");
    }
}

async fn send<S>(tx: &mut S, msg: &ServerMessage) -> Result<(), axum::Error>
where
    S: SinkExt<WsMessage, Error = axum::Error> + Unpin,
{
    let json = serde_json::to_string(msg).expect("serializable");
    tx.send(WsMessage::Text(json)).await
}

async fn reject_rate_limited<S>(tx: &mut S)
where
    S: SinkExt<WsMessage, Error = axum::Error> + Unpin,
{
    let _ = send(
        tx,
        &ServerMessage::Error {
            message: RATE_LIMITED.into(),
        },
    )
    .await;
}

pub const RATE_LIMITED: &str = "rate limited: slow down";

/// What a client is told when the host stops the server under it. Read back on
/// the client side to name the reason instead of "connection closed".
pub const HOST_STOPPED: &str = "the host stopped this server";

/// Resolves when the host has stopped the server — including when it already
/// had before this socket subscribed, which `changed()` alone would sleep
/// through.
async fn host_stopped(rx: &mut tokio::sync::watch::Receiver<bool>) {
    loop {
        let stopping = *rx.borrow_and_update();
        if stopping {
            return;
        }
        // The sender lives in the `AppContext` this connection holds, so the
        // only way out of here is the flag being set.
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

const RATE_WINDOW: Duration = Duration::from_secs(10);
/// Writes that cost the room something: a message, a join.
const WRITE_LIMIT: usize = 30;
/// Reads that cost the disk something: a history page, a blob. A client
/// scrolling and resolving images in batches stays far under it.
const READ_LIMIT: usize = 60;
/// Signals that fan out to a whole guild and are not worth an error: typing,
/// speaking, mute, camera, share, and a media key per member on a rekey.
const SIGNAL_LIMIT: usize = 120;
/// A sealed 32-byte key is 144 hex characters; anything near the frame cap is
/// a payload aimed at the recipient's queue.
const MAX_MEDIA_KEY_BLOB: usize = 512;
/// Every frame, writes included. A client at rest sends far less — typing is
/// throttled to one every two seconds — so this only ever catches a flood.
const FLOOD_LIMIT: usize = 300;

/// Its own budget, not the write one: a game pushing rich presence must not be
/// able to spend the allowance a person's messages need.
const ACTIVITY_LIMIT: usize = 12;
/// A real client identifies as soon as it reads the Hello. Anything still
/// silent after this is holding a socket for its own reasons.
const IDENTIFY_TIMEOUT: Duration = Duration::from_secs(10);
/// Each attempt is a Schnorr verification, so an unbounded retry is CPU we hand
/// to anyone who can open a socket.
const MAX_IDENTIFY_ATTEMPTS: u32 = 5;
/// Over `MAX_IMAGE_LEN` so a legal upload always fits, and far under the 64 MiB
/// tungstenite would otherwise buffer per connection.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// The socket enforces the cap and the handler enforces the image size, so a
/// cap under it would kill the connection before anything could explain why.
const _: () = assert!(MAX_FRAME_BYTES > crate::state::MAX_IMAGE_LEN);
/// Under the write bar the write limiter would be dead code, and every burst
/// would close the socket instead of slowing it.
const _: () = assert!(FLOOD_LIMIT > WRITE_LIMIT);

#[derive(Clone, Copy)]
enum OptionalScreen {
    Audio,
    Video,
}

async fn optional_screen_token<S>(
    cfg: &livekit::LiveKitConfig,
    which: OptionalScreen,
    user_pubkey: &str,
    screen_name: &str,
    channel_id: Id,
    ws_tx: &mut S,
) -> String
where
    S: SinkExt<WsMessage, Error = axum::Error> + Unpin,
{
    let (identity, can_publish, label, notify) = match which {
        OptionalScreen::Audio => (
            livekit::screen_audio_identity(user_pubkey),
            false,
            "audio",
            false,
        ),
        OptionalScreen::Video => (
            livekit::screen_video_identity(user_pubkey),
            true,
            "video",
            true,
        ),
    };
    match livekit::screen_token_as(cfg, &identity, screen_name, channel_id, can_publish).await {
        Ok(token) => token,
        Err(err) => {
            tracing::error!(%err, %channel_id, kind = label, "screen token mint failed");
            if notify {
                let _ = send(
                    ws_tx,
                    &ServerMessage::Error {
                        message: format!("screen-share {label} token mint failed: {err}"),
                    },
                )
                .await;
            }
            String::new()
        }
    }
}

fn removal_broadcasts(
    state: &crate::state::AppState,
    guild_id: crate::protocol::Id,
    target: &str,
    was_member: bool,
    cleared_voice: Option<crate::protocol::VoiceState>,
    was_sharing: Option<(Id, Id)>,
) {
    // `was_sharing` and `cleared_voice` are read by the caller *before* the
    // removal, because removing the member is what ends the share.
    let rest = state.guild_member_pubkeys(guild_id);
    if was_member {
        state.deliver(
            vec![target.to_string()],
            ServerMessage::GuildDelete { guild_id },
        );
        state.deliver(
            rest.clone(),
            ServerMessage::MemberRemove {
                guild_id,
                user_pubkey: target.to_string(),
            },
        );
    }
    if let Some(vs) = cleared_voice {
        let mut vs_targets = rest.clone();
        vs_targets.push(target.to_string());
        state.deliver(vs_targets, ServerMessage::VoiceStateUpdate(vs));
    }
    if let Some((gid, cid)) = was_sharing {
        broadcast_screen_state(state, gid, cid);
    }
}

const MAX_EMOJI_DATA_LEN: usize = 350_000; // ~256 KB of bytes once base64 is undone
const MAX_EMOJI_FETCH: usize = 64;
/// Four full-size images, so a client asking for message pictures four at a time
/// is never cut off, and 64 of them cannot be asked for in one frame.
const MAX_BLOB_RESPONSE_BYTES: usize = 4 * crate::state::MAX_IMAGE_LEN;

fn broadcast_emojis(state: &crate::state::AppState, guild_id: Id) {
    let targets = state.guild_member_pubkeys(guild_id);
    let emojis = state.emojis_of(guild_id);
    state.deliver(targets, ServerMessage::GuildEmojis { guild_id, emojis });
}

fn sharing_in(state: &crate::state::AppState, pubkey: &str) -> Option<(Id, Id)> {
    let vs = state.voice_states.get(pubkey)?;
    if !vs.screen_sharing {
        return None;
    }
    vs.channel_id.map(|cid| (vs.guild_id, cid))
}

fn broadcast_screen_state(state: &crate::state::AppState, guild_id: Id, channel_id: Id) {
    let targets = state.guild_member_pubkeys(guild_id);
    state.deliver(
        targets,
        ServerMessage::ScreenShareState {
            channel_id,
            sharers: state.screen_sharers_in(channel_id),
        },
    );
}

fn channel_audience(
    state: &crate::state::AppState,
    channel_id: crate::protocol::Id,
) -> Option<Vec<String>> {
    state
        .channel_guild(channel_id)
        .map(|gid| state.guild_member_pubkeys(gid))
}

use crate::state::is_hex_color;

fn sanitize_username(raw: &str) -> String {
    crate::protocol::canonical_username(raw)
}

const MAX_CLIENT_VERSION_BYTES: usize = 64;

fn sanitize_client_version(raw: &str) -> String {
    let mut out = String::with_capacity(MAX_CLIENT_VERSION_BYTES);
    for c in raw
        .trim()
        .chars()
        .filter(|c| !crate::protocol::unsafe_to_display(*c))
    {
        if out.len() + c.len_utf8() > MAX_CLIENT_VERSION_BYTES {
            break;
        }
        out.push(c);
    }
    out.truncate(out.trim_end().len());
    out.drain(..out.len() - out.trim_start().len());
    out
}

/// A DM or any channel with no guild grants a bot nothing: installs are
/// per-guild, and there is no guild here to have been installed in.
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

fn filter_for_bot(
    state: &crate::state::AppState,
    bot_pubkey: &str,
    msg: &ServerMessage,
) -> Option<ServerMessage> {
    match msg {
        ServerMessage::MessageCreate(m) => {
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
        ServerMessage::MemberUpdate(member) => {
            let install = state.bot_install(member.guild_id, bot_pubkey)?;
            install.has_intent(Intent::Members).then(|| msg.clone())
        }
        ServerMessage::MemberRemove { guild_id, .. } => {
            let install = state.bot_install(*guild_id, bot_pubkey)?;
            install.has_intent(Intent::Members).then(|| msg.clone())
        }
        ServerMessage::MessageDelete { channel_id, .. } => {
            let gid = state.channel_guild(*channel_id)?;
            let install = state.bot_install(gid, bot_pubkey)?;
            install
                .has_intent(Intent::GuildMessages)
                .then(|| msg.clone())
        }
        ServerMessage::ChannelCreate(ch) | ServerMessage::ChannelUpdate(ch) => state
            .bot_install(ch.guild_id, bot_pubkey)
            .map(|_| msg.clone()),
        ServerMessage::ChannelDelete { guild_id, .. } => state
            .bot_install(*guild_id, bot_pubkey)
            .map(|_| msg.clone()),
        ServerMessage::Error { .. } => Some(msg.clone()),
        _ => None,
    }
}

async fn deliver_join<S>(
    state: &crate::state::AppState,
    ws_tx: &mut S,
    joiner: &User,
    bundle: (
        crate::protocol::Guild,
        Vec<crate::protocol::Channel>,
        Vec<Member>,
        Vec<crate::protocol::Role>,
    ),
) -> Result<(), ()>
where
    S: SinkExt<WsMessage, Error = axum::Error> + Unpin,
{
    let (guild, channels, members, roles) = bundle;
    let guild_id = guild.id;
    tracing::info!(%guild_id, by = ?joiner.username, "guild joined");

    let mut targets = state.guild_member_pubkeys(guild_id);
    targets.retain(|p| p != &joiner.pubkey);
    state.deliver(
        targets,
        ServerMessage::MemberJoin(Member {
            user: joiner.clone(),
            guild_id,
            online: true,
            bot: false,
            roles: Vec::new(),
            xp: state.xp_of(guild_id, &joiner.pubkey),
        }),
    );
    let emojis = state.emojis_of(guild_id);
    let voice_states = state.voice_states_in(guild_id);
    if send(
        ws_tx,
        &ServerMessage::GuildJoined {
            guild,
            channels,
            members,
            roles,
            emojis,
            voice_states,
        },
    )
    .await
    .is_err()
    {
        return Err(());
    }
    if let Some(updated) = state.note_join_and_maybe_panic(guild_id).await {
        let members = state.guild_member_pubkeys(guild_id);
        state.deliver(members, ServerMessage::GuildUpdate(updated));
    }
    Ok(())
}

/// About a million hashes: well under a second for a joiner, a thousand times
/// that for a raid of a thousand identities.
const POW_BITS: u32 = 20;

/// The connection's own nonce is in the challenge, so the work cannot be done
/// before dialing and a solved answer dies with the socket it was solved on —
/// a ban and a rejoin on the same key start from zero.
fn pow_challenge(guild_id: crate::protocol::Id, pubkey: &str, conn_nonce: &str) -> String {
    format!("{guild_id}:{pubkey}:{conn_nonce}")
}

fn pow_ok(challenge: &str, nonce: &str, bits: u32) -> bool {
    use sha2::{Digest, Sha256};
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
    seen >= bits
}

#[allow(clippy::large_enum_variant)]
enum Gate {
    Proceed,
    Challenge(ServerMessage),
    Reject(String),
}

fn check_join_gate(
    state: &crate::state::AppState,
    guild_id: crate::protocol::Id,
    pubkey: &str,
    accept: bool,
    pow_nonce: Option<&str>,
    invite_code: Option<String>,
    conn_nonce: &str,
) -> Gate {
    use crate::protocol::JoinGate;
    if state.is_guild_member(guild_id, pubkey) {
        return Gate::Proceed;
    }
    let Some((gate, rules, panic)) = state.join_requirements(guild_id) else {
        return Gate::Proceed; // unknown guild — the join call will 404
    };
    if panic {
        return Gate::Reject("this server is in anti-raid lockdown — try again later".into());
    }
    match gate {
        JoinGate::Open => Gate::Proceed,
        JoinGate::Rules => {
            if accept {
                Gate::Proceed
            } else {
                Gate::Challenge(ServerMessage::JoinChallenge {
                    guild_id,
                    gate,
                    rules,
                    pow_challenge: None,
                    pow_difficulty: None,
                    invite_code,
                })
            }
        }
        JoinGate::Pow => {
            let challenge = pow_challenge(guild_id, pubkey, conn_nonce);
            let solved = pow_nonce
                .map(|n| pow_ok(&challenge, n, POW_BITS))
                .unwrap_or(false);
            if solved {
                Gate::Proceed
            } else {
                Gate::Challenge(ServerMessage::JoinChallenge {
                    guild_id,
                    gate,
                    rules: None,
                    pow_challenge: Some(challenge),
                    pow_difficulty: Some(POW_BITS),
                    invite_code,
                })
            }
        }
    }
}

struct RateLimiter {
    hits: VecDeque<Instant>,
    limit: usize,
    window: Duration,
}

impl RateLimiter {
    fn new(limit: usize, window: Duration) -> Self {
        Self {
            hits: VecDeque::new(),
            limit,
            window,
        }
    }

    fn allow(&mut self) -> bool {
        let now = Instant::now();
        while let Some(front) = self.hits.front() {
            if now.duration_since(*front) > self.window {
                self.hits.pop_front();
            } else {
                break;
            }
        }
        if self.hits.len() >= self.limit {
            return false;
        }
        self.hits.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_CLIENT_VERSION_BYTES, RATE_WINDOW, RateLimiter, WRITE_LIMIT, sanitize_client_version,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn a_full_window_is_admitted_and_the_next_hit_is_not() {
        let mut limiter = RateLimiter::new(WRITE_LIMIT, RATE_WINDOW);
        for i in 0..WRITE_LIMIT {
            assert!(limiter.allow(), "hit {i} is inside the window");
        }
        assert!(
            !limiter.allow(),
            "the hit past the limit must be refused, not admitted"
        );
    }

    #[test]
    fn a_hit_older_than_the_window_stops_counting() {
        let mut limiter = RateLimiter::new(WRITE_LIMIT, RATE_WINDOW);
        let expired = Instant::now() - RATE_WINDOW - Duration::from_secs(1);
        for _ in 0..WRITE_LIMIT {
            limiter.hits.push_back(expired);
        }
        assert!(
            limiter.allow(),
            "a window full of expired hits must not block a fresh one"
        );
        assert_eq!(
            limiter.hits.len(),
            1,
            "the expired hits should have been evicted, not merely ignored"
        );
    }

    #[test]
    fn a_version_cannot_forge_a_log_line() {
        let forged = "v1.0\nINFO identified user=admin pubkey=deadbeef version=v1.0";
        let clean = sanitize_client_version(forged);
        assert!(!clean.contains('\n'), "newline survived: {clean:?}");
        assert!(!clean.contains('\r'));
        assert!(
            !clean.chars().any(|c| c.is_control()),
            "a control character survived: {clean:?}"
        );

        for sep in ['\u{2028}', '\u{2029}'] {
            let forged = format!("v1.0{sep}INFO identified user=admin");
            let clean = sanitize_client_version(&forged);
            assert!(
                !clean.contains(sep),
                "U+{:04X} survived: {clean:?}",
                sep as u32
            );
        }
        assert!(clean.starts_with("v1.0"));
    }

    #[test]
    fn a_version_is_capped() {
        let long = "a".repeat(10_000);
        assert_eq!(
            sanitize_client_version(&long).len(),
            MAX_CLIENT_VERSION_BYTES
        );
    }

    #[test]
    fn control_characters_do_not_consume_the_cap() {
        let padded = format!(
            "{}v0.1.0-pre.223",
            "\u{0}".repeat(MAX_CLIENT_VERSION_BYTES * 2)
        );
        assert_eq!(sanitize_client_version(&padded), "v0.1.0-pre.223");

        let interior = format!("v0.1.0{}pre.223", "\n".repeat(MAX_CLIENT_VERSION_BYTES * 2));
        assert_eq!(sanitize_client_version(&interior), "v0.1.0pre.223");
    }

    #[test]
    fn filtering_does_not_leave_exposed_padding() {
        assert_eq!(sanitize_client_version("\u{0} v0.1.0 \u{0}"), "v0.1.0");
    }

    #[test]
    fn the_cap_counts_bytes_not_characters() {
        let wide = "🙂".repeat(1000);
        let clean = sanitize_client_version(&wide);
        assert!(
            clean.len() <= MAX_CLIENT_VERSION_BYTES,
            "{} bytes, over the {MAX_CLIENT_VERSION_BYTES}-byte budget",
            clean.len()
        );
        assert_eq!(clean.chars().count(), MAX_CLIENT_VERSION_BYTES / 4);
    }

    #[test]
    fn real_versions_survive_unchanged() {
        for v in ["v0.1.0-pre.223", "0.1.0-dev+a1b2c3d", "bot-sdk/0.1.0"] {
            assert_eq!(sanitize_client_version(v), v);
        }
        assert_eq!(sanitize_client_version("   "), "");
    }
}
