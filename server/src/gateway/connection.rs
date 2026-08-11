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
) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    // Register on the routing table immediately so any frame broadcast between
    // now and identify still reaches us; `outbound_rx` is this connection's
    // private queue drained in the select loop below.
    let (conn_id, mut outbound_rx) = ctx.state.register_conn();

    // Issue a per-connection nonce immediately so the client knows what to
    // sign in its Identify response.
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
                    ClientMessage::Identify { username, pubkey, signature, bot } => {
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
                        ctx.state.remember_user(&new_user).await;
                        // Bot-ness is SELF-DECLARED, not inferred from installs:
                        // if a mere install flipped a pubkey to "bot", anyone
                        // could strip a victim's human abilities by installing
                        // their pubkey in a throwaway guild. A declared bot gets
                        // the scoped, intent-filtered treatment; its per-guild
                        // powers still come only from actual installs.
                        is_bot = bot;
                        // Map this connection to its pubkey BEFORE snapshotting,
                        // so a frame delivered concurrently with identify queues
                        // for us rather than falling into a gap (a benign dup
                        // with the snapshot is fine; a lost frame is not).
                        ctx.state.identify_conn(conn_id, &new_user.pubkey);
                        let ready = if is_bot {
                            ctx.state.snapshot_for_bot(&new_user)
                        } else {
                            ctx.state.snapshot_for(&new_user).await
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
                    ClientMessage::FetchMessages { channel_id, limit, before_ms } => {
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
                            // Read-only channels: one predicate, two grant
                            // paths — humans via roles, bots via their install.
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
                            // Slowmode — moderators (ManageMessages/ManageChannels)
                            // and bots are exempt; everyone else waits their turn.
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
                                Some(msg) => {
                                    let author_pk = msg.author.pubkey.clone();
                                    let targets = ctx.state.guild_member_pubkeys(gid);
                                    ctx.state.deliver(targets, ServerMessage::MessageCreate(msg));
                                    // Award per-guild message-XP; on level-up,
                                    // the guild's roster re-renders the member.
                                    if let Some(member) = ctx.state.add_xp(gid, &author_pk).await {
                                        let targets = ctx.state.guild_member_pubkeys(gid);
                                        ctx.state.deliver(targets, ServerMessage::MemberUpdate(member));
                                    }
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
                                    ctx.state.push_dm_message(channel_id, author, content, image, reply_to).await
                                {
                                    // DMs have no guild, so no XP is earned.
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
                    ClientMessage::CreateGuild { name, template } => {
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
                        let (guild, channels, member, roles) =
                            ctx.state.create_guild(&name, template.as_deref(), &creator).await;
                        tracing::info!(guild = %guild.name, by = %creator.username, "guild created");
                        // The creator is the only member — hand the guild
                        // (channels + any template roles) to them directly. The
                        // public directory is fetched on demand (FetchCatalog),
                        // so no catalog push here.
                        if send(&mut ws_tx, &ServerMessage::GuildJoined {
                            guild,
                            channels,
                            members: vec![member],
                            roles,
                            // A brand-new guild has no custom emoji yet.
                            emojis: Vec::new(),
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
                        match check_join_gate(&ctx.state, guild_id, &joiner.pubkey, accept, pow_nonce.as_deref(), None) {
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
                        // Rate-limited: codes are high-entropy, but nobody gets
                        // to free-spin guesses either.
                        if !limiter.allow() {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "rate limited: slow down".into(),
                            }).await;
                            continue;
                        }
                        // Resolve the invite to a guild so we can gate-check it
                        // (the gate lives on the guild, not the code).
                        let Some(guild_id) = ctx.state.invite_guild(&code) else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "unknown or expired invite code".into(),
                            }).await;
                            continue;
                        };
                        match check_join_gate(&ctx.state, guild_id, &joiner.pubkey, accept, pow_nonce.as_deref(), Some(code.clone())) {
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
                        // Capture members BEFORE deletion removes them.
                        let targets = ctx.state.guild_member_pubkeys(guild_id);
                        match ctx.state.delete_guild(guild_id, &u.pubkey).await {
                            Ok(()) => {
                                tracing::info!(%guild_id, by = %u.username, "guild deleted");
                                ctx.state.deliver(targets, ServerMessage::GuildDelete { guild_id });
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
                        let channel_id = ctx.state.get_or_create_dm(&me.pubkey, &user_pubkey).await;
                        let other = ctx
                            .state
                            .users
                            .get(&user_pubkey)
                            .map(|u| u.clone())
                            .unwrap_or_else(|| User {
                                pubkey: user_pubkey.clone(),
                                username: user_pubkey.chars().take(6).collect(),
                            });
                        let messages = ctx.state.history(channel_id, 50, None).await;
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
                            // A Blossom (or any http) URL, or an inline data URL
                            // under the size cap (fallback when Blossom is down).
                            let is_url = (img.starts_with("https://") || img.starts_with("http://"))
                                && img.len() <= 2048;
                            let is_data =
                                img.starts_with("data:image/") && img.len() <= crate::state::MAX_IMAGE_LEN;
                            is_url || is_data
                        };
                        let field_kind = |v: &Option<String>| match v {
                            None => "none".to_string(),
                            Some(s) if s.starts_with("data:") => format!("data-url({})", s.len()),
                            Some(s) => format!("url({}..)", &s[..s.len().min(24)]),
                        };
                        eprintln!(
                            "[profile] SetProfile from {} avatar={} banner={}",
                            &u.pubkey[..u.pubkey.len().min(8)],
                            field_kind(&avatar),
                            field_kind(&banner),
                        );
                        if avatar.as_ref().is_some_and(|i| !valid_image(i)) {
                            eprintln!("[profile] REJECTED: avatar invalid");
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "avatar must be an http(s) or data:image URL under the size limit".into(),
                            }).await;
                            continue;
                        }
                        if banner.as_ref().is_some_and(|i| !valid_image(i)) {
                            eprintln!("[profile] REJECTED: banner invalid");
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "banner must be an http(s) or data:image URL under the size limit".into(),
                            }).await;
                            continue;
                        }
                        let bio = bio.map(|b| b.chars().take(280).collect::<String>());
                        let custom_status = custom_status.map(|c| c.chars().take(80).collect::<String>());
                        let profile = ctx.state.set_profile(
                            &u.pubkey, avatar, banner, bio, status, custom_status,
                        ).await;
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
                            ctx.state.toggle_reaction(channel_id, message_id, &emoji, &u.pubkey).await
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
                    ClientMessage::CreateGuildEmoji { guild_id, shortcode, image } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        // Emoji render at ~24px next to text, so the cap is far
                        // tighter than a message attachment's: a multi-megabyte
                        // emoji would be re-sent to every member of the guild
                        // and gains nothing on screen.
                        if !image.starts_with("data:image/") || image.len() > MAX_EMOJI_DATA_LEN {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "emoji must be an image under 256 KB".into(),
                            }).await;
                            continue;
                        }
                        // Decode into the content-addressed blob store, exactly
                        // as message attachments do — an emoji that duplicates
                        // an existing image costs no extra bytes on disk.
                        let Some(stored) = ctx.state.media.store_data_url(&image) else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "unsupported image format".into(),
                            }).await;
                            continue;
                        };
                        // `store_data_url` hands back the DB sentinel
                        // (`media:<hash>.<ext>`), but `GuildEmoji.image` is the
                        // bare content address: it is a cache key on the client
                        // and a filename there, so the `media:` prefix would
                        // both break `FetchEmoji` (double-prefixing) and be
                        // rejected by the client's filename check.
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
                        // Bounded per request so one client can't ask us to read
                        // the whole blob directory in a single frame. The client
                        // batches, so a big catalog just takes a few round trips.
                        let blobs: Vec<EmojiBlob> = images
                            .into_iter()
                            .take(MAX_EMOJI_FETCH)
                            .map(|image| {
                                // `inline` sanitises the name (path traversal is
                                // impossible) and returns None for anything
                                // missing; an empty data URL tells the client
                                // "no such blob, stop asking".
                                let data_url = ctx
                                    .state
                                    .media
                                    .inline(&format!("media:{image}"))
                                    .unwrap_or_default();
                                EmojiBlob { image, data_url }
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
                        // Sanitise to a hex color if present.
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
                        match ctx.state.uninstall_bot(guild_id, &bot_pubkey, &u.pubkey).await {
                            Ok(()) => {
                                tracing::info!(%guild_id, bot = %bot_pubkey, by = %u.username, "bot uninstalled");
                                // Removal, not offline — clients drop the row.
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
                    ClientMessage::SetScreenShare { channel_id, sharing } => {
                        let Some(u) = user.as_ref() else { continue };
                        // Members only — otherwise any connected user could flip
                        // LIVE badges in guilds they don't belong to.
                        let Some(gid) = ctx.state.channel_guild(channel_id) else { continue };
                        if !ctx.state.is_guild_member(gid, &u.pubkey) {
                            continue;
                        }
                        let sharers = ctx.state.set_screen_share(channel_id, &u.pubkey, sharing);
                        // Tell the channel's guild who's live.
                        let targets = ctx.state.guild_member_pubkeys(gid);
                        ctx.state.deliver(
                            targets,
                            ServerMessage::ScreenShareState { channel_id, sharers },
                        );
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
                                // Also hand over screen-share tokens (separate
                                // room): one for the webview JS client, which
                                // renders the video and captures it where it
                                // can; one for the native client subscribing to
                                // the audio so it plays through the same device
                                // as voice; and one for the native client to
                                // *publish* video where the webview has no
                                // capture API at all (macOS/WKWebView). Three
                                // tokens because they join under different
                                // identities — LiveKit permits only one
                                // connection per identity per room.
                                let screen_name = format!("{} (screen)", u.username);
                                if let Ok(screen_token) = livekit::screen_token_as(
                                    &ctx.livekit,
                                    &u.pubkey,
                                    &screen_name,
                                    channel_id,
                                )
                                .await
                                {
                                    let audio_token = livekit::screen_token_as(
                                        &ctx.livekit,
                                        &livekit::screen_audio_identity(&u.pubkey),
                                        &screen_name,
                                        channel_id,
                                    )
                                    .await
                                    .unwrap_or_default();
                                    let video_token = livekit::screen_token_as(
                                        &ctx.livekit,
                                        &livekit::screen_video_identity(&u.pubkey),
                                        &screen_name,
                                        channel_id,
                                    )
                                    .await
                                    .unwrap_or_default();
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
                    ClientMessage::CreateRole { guild_id, name, color, permissions } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            continue;
                        }
                        match ctx.state.create_role(guild_id, &name, color, permissions, &u.pubkey).await {
                            Ok(role) => {
                                tracing::info!(%guild_id, role = %role.name, by = %u.username, "role created");
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
                            continue;
                        }
                        match ctx.state.delete_role(guild_id, role_id, &u.pubkey).await {
                            Ok(changed_members) => {
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets.clone(), ServerMessage::GuildRoles {
                                    guild_id,
                                    roles: ctx.state.guild_roles(guild_id),
                                });
                                // Everyone re-renders the members whose role
                                // set just lost the deleted role.
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
                                tracing::info!(%guild_id, ?visibility, by = %u.username, "guild visibility set");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildUpdate(guild));
                                // Visibility changes show up in the directory on
                                // the next FetchCatalog.
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::CreateInvite { guild_id, rotate } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            continue;
                        }
                        match ctx.state.get_or_create_invite(guild_id, rotate, &u.pubkey).await {
                            Ok(code) => {
                                let _ = send(&mut ws_tx, &ServerMessage::GuildInvite {
                                    guild_id,
                                    code,
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
                            continue;
                        }
                        match ctx.state.kick_member(guild_id, &user_pubkey, &u.pubkey).await {
                            Ok(cleared_voice) => {
                                tracing::info!(%guild_id, target = %user_pubkey, by = %u.username, "member kicked");
                                ctx.state.audit(guild_id, &u.pubkey, "kick", &user_pubkey, "").await;
                                removal_broadcasts(&ctx.state, guild_id, &user_pubkey, true, cleared_voice);
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
                            continue;
                        }
                        let was_member = ctx.state.is_guild_member(guild_id, &user_pubkey);
                        match ctx.state.ban_member(guild_id, &user_pubkey, &u.pubkey).await {
                            Ok(cleared_voice) => {
                                tracing::info!(%guild_id, target = %user_pubkey, by = %u.username, "member banned");
                                ctx.state.audit(guild_id, &u.pubkey, "ban", &user_pubkey, "").await;
                                removal_broadcasts(&ctx.state, guild_id, &user_pubkey, was_member, cleared_voice);
                                // Refresh the moderator's ban panel.
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
                        match ctx.state.leave_guild(guild_id, &u.pubkey).await {
                            Ok(cleared_voice) => {
                                tracing::info!(%guild_id, by = %u.username, "left guild");
                                removal_broadcasts(&ctx.state, guild_id, &u.pubkey, true, cleared_voice);
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
                            continue;
                        }
                        match ctx.state.create_channel(guild_id, &name, kind, topic, &u.pubkey).await {
                            Ok(channel) => {
                                tracing::info!(%guild_id, channel = %channel.name, by = %u.username, "channel created");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::ChannelCreate(channel));
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
                            continue;
                        }
                        match ctx.state.delete_channel(channel_id, &u.pubkey).await {
                            Ok((guild_id, evicted)) => {
                                tracing::info!(%guild_id, %channel_id, by = %u.username, "channel deleted");
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(
                                    targets.clone(),
                                    ServerMessage::ChannelDelete { guild_id, channel_id },
                                );
                                // Anyone who was in the deleted voice channel
                                // is forced idle on every roster.
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
                            continue;
                        }
                        match ctx.state.delete_message(channel_id, message_id, &u.pubkey).await {
                            Ok(()) => {
                                // Works for guild channels AND DMs — the
                                // audience helper covers both.
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
                                tracing::info!(%guild_id, to = %new_owner_pubkey, by = %u.username, "ownership transferred");
                                // Clients re-derive the crown/menus from the
                                // updated owner_pubkey — no extra frame needed.
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildUpdate(guild));
                            }
                            Err(e) => {
                                let _ = send(&mut ws_tx, &ServerMessage::Error { message: e }).await;
                            }
                        }
                    }
                    ClientMessage::SetGuildProfile { guild_id, description, icon_image, banner } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        if !limiter.allow() {
                            continue;
                        }
                        match ctx.state.set_guild_profile(guild_id, description, icon_image, banner, &u.pubkey).await {
                            Ok(guild) => {
                                let targets = ctx.state.guild_member_pubkeys(guild_id);
                                ctx.state.deliver(targets, ServerMessage::GuildUpdate(guild));
                                // Branding refreshes in the directory on the next
                                // FetchCatalog.
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
                                tracing::info!(%guild_id, ?days, by = %u.username, "guild retention set");
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
                    ClientMessage::SetPanicMode { guild_id, on } => {
                        let Some(u) = user.as_ref() else {
                            let _ = send(&mut ws_tx, &ServerMessage::Error {
                                message: "identify first".into(),
                            }).await;
                            continue;
                        };
                        match ctx.state.set_panic_mode(guild_id, on, &u.pubkey).await {
                            Ok(guild) => {
                                tracing::info!(%guild_id, on, by = %u.username, "panic mode set");
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
                        // On-demand public directory (replaces the old
                        // broadcast-to-everyone). Requester only; open to any
                        // connection so the browse dialog works pre-join.
                        const DEFAULT_PAGE: u32 = 100;
                        const MAX_PAGE: u32 = 500;
                        let limit = if limit == 0 { DEFAULT_PAGE } else { limit.min(MAX_PAGE) };
                        let (guilds, total) = ctx.state.guild_catalog_page(offset, limit);
                        let _ = send(&mut ws_tx, &ServerMessage::GuildCatalog { guilds, offset, total }).await;
                    }
                }
            }

            outbound = outbound_rx.recv() => {
                // Routing already guaranteed this frame was meant for us
                // (targeted delivery / broadcast), so there's no per-frame
                // address check. `None` means our sender was dropped — the
                // registry shed us for being too slow, or the server is going
                // away; either way we close and the client reconnects.
                let Some(msg) = outbound else { break };
                // Bots see a filtered stream: only events for guilds they're
                // installed in, only the intents they were granted, and message
                // content withheld unless the privileged MessageContent intent
                // is present.
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

    // Drop this connection from the routing table first, so nothing is routed
    // to a socket we're tearing down.
    ctx.state
        .unregister_conn(conn_id, user.as_ref().map(|u| u.pubkey.as_str()));

    if let Some(u) = user {
        if let Some(cleared) = ctx.state.clear_voice(&u.pubkey) {
            let targets = ctx.state.guild_member_pubkeys(cleared.guild_id);
            ctx.state
                .deliver(targets, ServerMessage::VoiceStateUpdate(cleared));
        }
        broadcast_screen_clear(&ctx.state, &u.pubkey);
        for (guild_id, user_pubkey) in ctx.state.mark_offline(&u.pubkey) {
            // Only the members of that guild should see the leave.
            let targets = ctx.state.guild_member_pubkeys(guild_id);
            ctx.state.deliver(
                targets,
                ServerMessage::MemberLeave {
                    guild_id,
                    user_pubkey,
                },
            );
        }
        tracing::info!(user = %u.username, "client disconnected");
    }
}

async fn send<S>(tx: &mut S, msg: &ServerMessage) -> Result<(), axum::Error>
where
    S: SinkExt<WsMessage, Error = axum::Error> + Unpin,
{
    let json = serde_json::to_string(msg).expect("serializable");
    tx.send(WsMessage::Text(json)).await
}

/// The delivery recipe after a membership removal (kick/ban/leave): the
/// removed user gets a targeted `GuildDelete` (their client already tears the
/// guild down cleanly and reselects), the remaining members drop the roster
/// row, any voice/screen presence in that guild is broadcast clear, and the
/// public catalog refreshes (member count changed).
fn removal_broadcasts(
    state: &crate::state::AppState,
    guild_id: crate::protocol::Id,
    target: &str,
    was_member: bool,
    cleared_voice: Option<crate::protocol::VoiceState>,
) {
    // Compute the remaining audience AFTER removal so the target never
    // receives the roster frames meant for members.
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
        // Include the removed user: their client sees channel_id=None for self
        // and force-idles its live voice session (net.rs handles this), which
        // is what actually hangs up their audio.
        let mut vs_targets = rest.clone();
        vs_targets.push(target.to_string());
        state.deliver(vs_targets, ServerMessage::VoiceStateUpdate(vs));
    }
    for (channel_id, sharers) in state.clear_user_screen_shares_in_guild(guild_id, target) {
        state.deliver(
            rest.clone(),
            ServerMessage::ScreenShareState {
                channel_id,
                sharers,
            },
        );
    }
}

/// Largest emoji upload accepted, as data-URL length. Emoji are rendered at
/// roughly text height and pushed to every member of the guild, so the cap is
/// far tighter than a message attachment's `MAX_IMAGE_LEN`.
const MAX_EMOJI_DATA_LEN: usize = 350_000; // ~256 KB of bytes once base64 is undone
/// Emoji images served per `FetchEmoji`, so one frame can't ask us to read the
/// whole blob directory. Clients batch and come back for the rest.
const MAX_EMOJI_FETCH: usize = 64;

/// Push a guild's whole emoji catalog to its members.
///
/// The full list rather than a delta: it is bounded by `MAX_EMOJIS_PER_GUILD`,
/// so a replace is cheap and cannot drift out of sync the way an add/remove
/// stream can. `deliver` (not `broadcast`) keeps it to the guild's members —
/// emoji are guild configuration, not public data.
fn broadcast_emojis(state: &crate::state::AppState, guild_id: Id) {
    let targets = state.guild_member_pubkeys(guild_id);
    let emojis = state.emojis_of(guild_id);
    state.deliver(targets, ServerMessage::GuildEmojis { guild_id, emojis });
}

/// Drop a user from every screen-share set and tell each affected channel's
/// guild members the updated sharer list.
fn broadcast_screen_clear(state: &crate::state::AppState, pubkey: &str) {
    for (channel_id, sharers) in state.clear_user_screen_shares(pubkey) {
        if let Some(gid) = state.channel_guild(channel_id) {
            let targets = state.guild_member_pubkeys(gid);
            state.deliver(
                targets,
                ServerMessage::ScreenShareState {
                    channel_id,
                    sharers,
                },
            );
        }
    }
}

/// The set of pubkeys allowed to see activity in a channel: a guild's members
/// for guild channels, or the two participants for a DM.
fn channel_audience(
    state: &crate::state::AppState,
    channel_id: crate::protocol::Id,
) -> Option<Vec<String>> {
    if let Some(gid) = state.channel_guild(channel_id) {
        Some(state.guild_member_pubkeys(gid))
    } else {
        state.dm_participants(channel_id).map(|p| p.to_vec())
    }
}

// Hex-color validation lives in `state::is_hex_color` (shared with role colors).
use crate::state::is_hex_color;

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
        // Channel topology matters to any installed bot (it's in Ready too) —
        // no intent needed, just an install in that guild.
        ServerMessage::ChannelCreate(ch) | ServerMessage::ChannelUpdate(ch) => state
            .bot_install(ch.guild_id, bot_pubkey)
            .map(|_| msg.clone()),
        ServerMessage::ChannelDelete { guild_id, .. } => state
            .bot_install(*guild_id, bot_pubkey)
            .map(|_| msg.clone()),
        // Errors are useful feedback for a bot (e.g. permission denied).
        ServerMessage::Error { .. } => Some(msg.clone()),
        // Everything else (typing, voice, screen share, profiles, the public
        // catalog, guild metadata, DMs, integrations) is outside a bot's event
        // surface — never delivered.
        _ => None,
    }
}

/// Common tail of both successful join paths: announce the new member to the
/// existing roster, hand the joiner their `GuildJoined` bundle, refresh the
/// public catalog, and run mass-join (raid) detection — auto-locking the guild
/// and broadcasting the change if a raid is underway. `Err(())` = the joiner's
/// socket closed (caller should break).
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
    tracing::info!(%guild_id, by = %joiner.username, "guild joined");

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
            // Rejoining resumes the level earned here before.
            xp: state.xp_of(guild_id, &joiner.pubkey),
        }),
    );
    let emojis = state.emojis_of(guild_id);
    if send(
        ws_tx,
        &ServerMessage::GuildJoined {
            guild,
            channels,
            members,
            roles,
            emojis,
        },
    )
    .await
    .is_err()
    {
        return Err(());
    }
    // Raid detection: if this join tips the guild over the rate threshold,
    // panic mode engages and everyone sees the guild update.
    if let Some(updated) = state.note_join_and_maybe_panic(guild_id).await {
        let members = state.guild_member_pubkeys(guild_id);
        state.deliver(members, ServerMessage::GuildUpdate(updated));
    }
    Ok(())
}

/// Proof-of-work difficulty (leading zero BITS) for the `Pow` join gate.
/// ~2^16 hashes ≈ sub-second for one joiner, brutal for a keygen raid.
const POW_BITS: u32 = 16;

/// Deterministic per-(guild,user) PoW challenge string. Includes both ids so a
/// solution can't be reused across guilds or by a different keypair — each
/// raid identity must redo the work.
fn pow_challenge(guild_id: crate::protocol::Id, pubkey: &str) -> String {
    format!("{guild_id}:{pubkey}")
}

/// True if SHA-256(challenge ++ nonce) has ≥ `bits` leading zero bits.
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

/// Outcome of the join-gate check.
// Built once per join attempt and consumed immediately, so boxing the large
// `ServerMessage` variant would cost an allocation to save nothing.
#[allow(clippy::large_enum_variant)]
enum Gate {
    Proceed,
    Challenge(ServerMessage),
    Reject(String),
}

/// Evaluate a guild's join gate for a would-be joiner. Members (incl. the
/// owner) bypass; panic mode rejects everyone; otherwise Rules/Pow must be
/// satisfied by `accept`/`pow_nonce`.
fn check_join_gate(
    state: &crate::state::AppState,
    guild_id: crate::protocol::Id,
    pubkey: &str,
    accept: bool,
    pow_nonce: Option<&str>,
    invite_code: Option<String>,
) -> Gate {
    use crate::protocol::JoinGate;
    // Already a member (owner included) — no gate.
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
            let challenge = pow_challenge(guild_id, pubkey);
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
