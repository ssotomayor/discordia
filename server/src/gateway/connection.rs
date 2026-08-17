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
                    ClientMessage::Identify {
                        username,
                        pubkey,
                        signature,
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
                        // The version is logged and stored nowhere. That is the
                        // whole feature: an operator can count what is connected
                        // without the server growing a field it never reads —
                        // the shape three entries in docs/AUDIT-2026-08-17.md already complain
                        // about. "unknown" rather than empty so the log line
                        // does not read as a truncation.
                        let client_version = sanitize_client_version(&client_version);
                        let client_version = if client_version.is_empty() {
                            "unknown".to_string()
                        } else {
                            client_version
                        };
                        // `?` and not `%`. The rule across this server: a value
                        // is formatted with `%` only if its *type* cannot
                        // contain a line break — `Uuid`, `SocketAddr`,
                        // `StatusCode`. Every free-text value uses `?`, which
                        // quotes and escapes: usernames, guild/channel/role
                        // names, pubkeys, and the request log's method and path.
                        //
                        // Two different attacks need both defences. Stripping
                        // control characters stops a peer starting a *new* log
                        // line; but on one line a value like `1 user=admin`
                        // still reads as two fields to anything parsing this
                        // format, and quoting is what makes the boundary
                        // unambiguous. Only `client_version` gets both — the
                        // rest cannot be filtered without changing a signed
                        // preimage or a stored name. See docs/AUDIT-2026-08-17.md.
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
                        // Never hand DM history to a non-participant, nor guild
                        // channel history to a non-member.
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
                        tracing::info!(guild = ?guild.name, by = ?creator.username, "guild created");
                        // The creator is the only member — hand the guild
                        // (channels + any template roles) to them directly. The
                        // public directory is fetched on demand (FetchCatalog),
                        // so no catalog push here.
                        if send(&mut ws_tx, &ServerMessage::GuildJoined {
                            guild,
                            channels,
                            members: vec![member],
                            roles,
                            // A brand-new guild has no custom emoji yet, and
                            // nobody has had the chance to be in its voice
                            // channels either.
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
                            reject_rate_limited(&mut ws_tx).await;
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
                                tracing::info!(%guild_id, bot = ?bot_pubkey, by = ?u.username, "bot installed");
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
                                tracing::info!(%guild_id, bot = ?bot_pubkey, by = ?u.username, "bot uninstalled");
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
                    // `channel_id` is accepted and ignored. The flag lives on the
                    // sender's own `VoiceState` now, so the channel comes from
                    // there — a client cannot claim a share in a channel its
                    // voice state says it is not in. The field stays on the wire
                    // so a client older than this still talks to a new server;
                    // serde drops what the struct does not name.
                    //
                    // No `is_guild_member` check for the same reason `SetCamera`
                    // needs none: `update_screen_share` can only touch a voice
                    // state that exists, and only `JoinVoice` creates one, and it
                    // checks.
                    ClientMessage::SetScreenShare { channel_id: _, sharing } => {
                        let Some(u) = user.as_ref() else { continue };
                        let Some(vs) = ctx.state.update_screen_share(&u.pubkey, sharing) else {
                            continue;
                        };
                        let targets = ctx.state.guild_member_pubkeys(vs.guild_id);
                        let channel = vs.channel_id;
                        ctx.state
                            .deliver(targets.clone(), ServerMessage::VoiceStateUpdate(vs));
                        // And the legacy frame, derived from the same source, for
                        // clients older than `VoiceState::screen_sharing`.
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
                                //
                                // An empty token on the wire means "this server
                                // predates that identity" — see ScreenToken's
                                // docs — and the client degrades on it, which is
                                // the right answer for an old server and the
                                // wrong one for a mint that failed a moment ago.
                                // The client cannot tell those apart: a failed
                                // video mint surfaces on macOS as "This server is
                                // too old to accept a natively captured screen
                                // share", a diagnosis rather than a fact. So a
                                // failure is logged where the operator can see
                                // it, and the ones that leave a path with no
                                // fallback also reach the user. Which of the two
                                // optional identities that applies to is decided
                                // once, in `OptionalScreen`, rather than at each
                                // call. The main token is handled here because
                                // its failure sends no frame at all.
                                let screen_name = format!("{} (screen)", u.username);
                                let screen_token = livekit::screen_token_as(
                                    &ctx.livekit,
                                    &u.pubkey,
                                    &screen_name,
                                    channel_id,
                                    // The webview both renders and, on Windows,
                                    // captures.
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
                                        // The one with no fallback at all: the
                                        // webview renders every share from this
                                        // room, so without it the user sees
                                        // nobody's screen and can publish none of
                                        // their own on the webview path either.
                                        // It used to be swallowed by an `if let
                                        // Ok`, which left voice working and screen
                                        // sharing quietly absent.
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
                        // Captured before the clear: that is what ends the share.
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
                    // No `is_guild_member` check, unlike `SetScreenShare` — and
                    // that asymmetry is deliberate. `SetScreenShare` writes into
                    // a channel-keyed map that nothing else gates, so it has to
                    // check. `update_camera` can only touch a voice state that
                    // already exists, and the only thing that creates one is
                    // `JoinVoice`, which checks membership. Revocation is
                    // covered too: kick/ban/leave run through `clear_voice`, so
                    // the entry is gone before a stale flag could outlive the
                    // membership. A non-member gets `None` and no frames.
                    ClientMessage::ShareMediaKey { channel_id, to, epoch, blob } => {
                        let Some(u) = user.as_ref() else { continue };
                        // Both ends must be in the guild that owns the channel.
                        // The blob is sealed, so this is not what keeps it
                        // secret — it is what stops the gateway being a way to
                        // push payloads at people who never asked.
                        let Some(guild_id) = ctx.state.channel_guild(channel_id) else {
                            tracing::warn!(%channel_id, "media key for a channel with no guild");
                            continue;
                        };
                        let members = ctx.state.guild_member_pubkeys(guild_id);
                        if !members.contains(&u.pubkey) || !members.contains(&to) {
                            // Logged rather than dropped in silence: this is the
                            // one place a key can vanish without either client
                            // being able to tell, and a member list that does
                            // not contain somebody who is plainly in the channel
                            // is worth seeing.
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
                                // Visibility changes show up in the directory on
                                // the next FetchCatalog.
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
                            reject_rate_limited(&mut ws_tx).await;
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
                                tracing::info!(%guild_id, to = ?new_owner_pubkey, by = ?u.username, "ownership transferred");
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
                            reject_rate_limited(&mut ws_tx).await;
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
        let was_sharing = sharing_in(&ctx.state, &u.pubkey);
        if let Some(cleared) = ctx.state.clear_voice(&u.pubkey) {
            let targets = ctx.state.guild_member_pubkeys(cleared.guild_id);
            ctx.state
                .deliver(targets, ServerMessage::VoiceStateUpdate(cleared));
        }
        if let Some((gid, cid)) = was_sharing {
            broadcast_screen_state(&ctx.state, gid, cid);
        }
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

/// Tell the client its action was refused for rate, in one place.
///
/// **Fourteen of the seventeen rate-limited arms used to `continue` in
/// silence** — the client asked for something, nothing happened, and nothing
/// said why. That is worst for the actions that come in bursts: a channel
/// reorder emits one `UpdateChannel` per row it renumbers, so a guild that has
/// never been reordered spends its whole budget in one drag and the list is
/// left visibly half-sorted, with no message and the same window blocking the
/// user's next ten seconds of unrelated actions.
///
/// One helper rather than fourteen copies of the literal, because the message
/// is the contract: a client that wants to distinguish "refused" from "lost"
/// has to be able to match on something stable.
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

/// What every rate-limit refusal says. Public so a test can assert on the
/// refusal rather than on a spelling.
pub const RATE_LIMITED: &str = "rate limited: slow down";

/// The two screen-room identities a client can do without.
///
/// Each one answers three questions the same way every time — which identity,
/// whether it ever publishes, and what its absence costs — so they live here
/// together instead of being spelled out at each call site. Adding a fourth
/// identity to the screen room means adding a variant, which is the point.
#[derive(Clone, Copy)]
enum OptionalScreen {
    /// `{pubkey}#audio`. Subscribes to stream audio so it plays through the same
    /// cpal device as voice, and sends nothing, ever. Losing it degrades to
    /// webview playback: still audible, just on the system's output device
    /// rather than the chosen one — a real fallback, so the user is not told.
    Audio,
    /// `{pubkey}#video`. Publishes natively captured screen video. Losing it has
    /// no fallback at all — on macOS this is the only capture path — so the user
    /// is told rather than left with a share button that does nothing.
    Video,
}

/// Mint one of the optional screen-room tokens, or report why not.
///
/// Empty is a real value on this wire: it is what a server predating either
/// identity sends, and the client degrades on it. That makes an empty string the
/// right thing to return on failure too — the client behaves identically — as
/// long as the failure is not silent, which is what this exists to guarantee.
///
/// The **main** screen token deliberately does not come through here. Its
/// failure has to suppress the whole `ScreenToken` frame rather than empty one
/// field of it: `net.rs` stores that field unconditionally, so a frame carrying
/// `token: ""` would have the client try to join the room with an empty token.
/// A helper returning `String` cannot express "send nothing at all".
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

/// The delivery recipe after a membership removal (kick/ban/leave): the
/// removed user gets a targeted `GuildDelete` (their client already tears the
/// guild down cleanly and reselects), the remaining members drop the roster
/// row, any voice/screen presence in that guild is broadcast clear, and the
/// public catalog refreshes (member count changed).
/// `was_sharing` is read by the caller *before* the removal, because removing
/// the member clears their voice state and the tombstone no longer names the
/// channel their share was in — which the legacy `ScreenShareState` frame needs.
fn removal_broadcasts(
    state: &crate::state::AppState,
    guild_id: crate::protocol::Id,
    target: &str,
    was_member: bool,
    cleared_voice: Option<crate::protocol::VoiceState>,
    was_sharing: Option<(Id, Id)>,
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
    if let Some((gid, cid)) = was_sharing {
        broadcast_screen_state(state, gid, cid);
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

/// Where a user is sharing right now, if they are — `(guild, channel)`.
///
/// Read *before* their voice state is cleared, because clearing it is what ends
/// the share and the tombstone no longer names the channel. Returns `None` when
/// they were not sharing, which is what keeps a plain leave from broadcasting a
/// sharer list that has not changed.
fn sharing_in(state: &crate::state::AppState, pubkey: &str) -> Option<(Id, Id)> {
    let vs = state.voice_states.get(pubkey)?;
    if !vs.screen_sharing {
        return None;
    }
    vs.channel_id.map(|cid| (vs.guild_id, cid))
}

/// Tell a channel who is still sharing, after someone stopped.
///
/// The state itself needs no clearing — the flag rides `VoiceState`, so
/// `clear_voice` already took it. This exists only to re-derive the legacy
/// `ScreenShareState` frame for clients older than `VoiceState::screen_sharing`,
/// which have no other way to drop the LIVE badge.
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

/// The set of pubkeys allowed to see activity in a channel: a guild's members.
///
/// Every channel this server knows about belongs to a guild. Direct messages
/// used to be the exception — a channel with two participants and no guild —
/// and are now Nostr gift wraps that never reach this process at all.
fn channel_audience(
    state: &crate::state::AppState,
    channel_id: crate::protocol::Id,
) -> Option<Vec<String>> {
    state
        .channel_guild(channel_id)
        .map(|gid| state.guild_member_pubkeys(gid))
}

// Hex-color validation lives in `state::is_hex_color` (shared with role colors).
use crate::state::is_hex_color;

/// Canonicalise a username the same way the client does before it signs.
///
/// This used to be its own trim-and-truncate here, and that was the bug: the
/// server canonicalised before verifying while the client signed the raw
/// string, so a name this altered could never authenticate. The definition now
/// lives in `protocol` so there is exactly one of it — see
/// `protocol::canonical_username`.
fn sanitize_username(raw: &str) -> String {
    crate::protocol::canonical_username(raw)
}

/// Longest self-declared version we will repeat, **in bytes**. Generous against
/// the ~20 the real ones use (`v0.1.0-pre.223`, `bot-sdk/0.1.0`), tight enough
/// that a peer cannot write a paragraph to our disk on every connect.
///
/// Bytes and not characters, because bytes are what the justification above is
/// about. Counting characters let 64 four-byte codepoints through as 256 bytes
/// — four times the budget, for a string nobody reads as text anyway. Every
/// real value is ASCII, so the two units agree wherever it matters.
const MAX_CLIENT_VERSION_BYTES: usize = 64;

/// Make a self-declared version string safe to repeat into a log line.
///
/// **This comment used to claim `username` had the same treatment via
/// `sanitize_username`, and that was wrong.** `canonical_username` trims and
/// truncates; it has never filtered control characters, and `verify_identify`
/// takes the name as opaque bytes — so `"al\nice"` reaches the log intact. The
/// signature commits to it either way. Every username log site is therefore
/// formatted with `?` rather than `%`; see the note there.
///
/// `client_version` needs its own cap regardless: it is unauthenticated by
/// design, written on every identify, and unlike a username it has no length
/// bound anywhere upstream. The server's subscriber is the plain-text
/// `tracing_subscriber::fmt()`, which escapes nothing, so an embedded newline
/// turns one field into a forged log line — CWE-117 — and a forged line defeats
/// the only thing this field exists for, which is counting what is connected.
///
/// Control characters are stripped *before* the cap, so a run of them cannot
/// consume the budget and leave nothing behind.
fn sanitize_client_version(raw: &str) -> String {
    let mut out = String::with_capacity(MAX_CLIENT_VERSION_BYTES);
    for c in raw.trim().chars().filter(|c| !breaks_a_line(*c)) {
        if out.len() + c.len_utf8() > MAX_CLIENT_VERSION_BYTES {
            // `break`, not `continue`. A shorter character later in the string
            // would still fit the budget, but taking it would make the result a
            // filtered subset rather than a prefix — a truncated string should
            // read as the start of what was sent, not as a scramble of it.
            break;
        }
        out.push(c);
    }
    // Trimmed a second time, in place, because filtering can *expose* padding
    // the first trim could not see: `"\u{0} v1.0"` has no leading whitespace
    // until the NUL is removed. Done with `truncate`/`drain` rather than
    // `.trim().to_string()` so this allocates once instead of twice.
    out.truncate(out.trim_end().len());
    out.drain(..out.len() - out.trim_start().len());
    out
}

/// Characters that can end a line somewhere downstream.
///
/// `char::is_control()` alone is not that set, which is the trap. It covers
/// category Cc — `\n`, `\r`, ESC, and U+0085 — and stops there. **U+2028 LINE
/// SEPARATOR and U+2029 PARAGRAPH SEPARATOR are Zl and Zp**, so `is_control()`
/// returns false for them and they would survive a filter built on it alone.
///
/// Whether they *render* as a break depends on the consumer — some viewers and
/// log processors honour them, `tracing_subscriber::fmt()` does not — which is
/// exactly why they belong here rather than in a judgement call at each sink.
/// The point of this function is that a peer cannot end a line; a character
/// that ends one for *somebody* qualifies.
fn breaks_a_line(c: char) -> bool {
    c.is_control() || c == '\u{2028}' || c == '\u{2029}'
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
            // Rejoining resumes the level earned here before.
            xp: state.xp_of(guild_id, &joiner.pubkey),
        }),
    );
    let emojis = state.emojis_of(guild_id);
    // Taken after `MemberJoin` has gone out but before the joiner's own bundle
    // is sent, so it cannot miss someone who was already in a voice channel —
    // the case this exists for.
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

#[cfg(test)]
mod tests {
    use super::{MAX_CLIENT_VERSION_BYTES, sanitize_client_version};

    /// The injection this exists to stop. A peer choosing its own version
    /// string must not be able to write a second log line, or a second field.
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

        // The Unicode separators, which `is_control()` does *not* cover — they
        // are categories Zl and Zp, not Cc, so a filter written on
        // `is_control()` alone lets them straight through.
        for sep in ['\u{2028}', '\u{2029}'] {
            let forged = format!("v1.0{sep}INFO identified user=admin");
            let clean = sanitize_client_version(&forged);
            assert!(
                !clean.contains(sep),
                "U+{:04X} survived: {clean:?}",
                sep as u32
            );
        }
        // What is left is one line, and the log site quotes it — so the
        // remaining `key=value` text cannot read as fields of its own.
        assert!(clean.starts_with("v1.0"));
    }

    /// Unbounded is the other half: this is written on every identify, by
    /// anyone who can open a socket.
    #[test]
    fn a_version_is_capped() {
        let long = "a".repeat(10_000);
        assert_eq!(
            sanitize_client_version(&long).len(),
            MAX_CLIENT_VERSION_BYTES
        );
    }

    /// Control characters are stripped *before* the cap. Filtering after it
    /// would let a run of them eat the whole budget and leave nothing, turning
    /// a hostile string into an indistinguishable "unknown".
    ///
    /// **The padding must not be whitespace.** An earlier version of this test
    /// padded with `\n`, which the `raw.trim()` at the top of the function
    /// removes before `filter`/`take` ever run — so it passed with the two
    /// steps in either order and pinned nothing. Verified by swapping them:
    /// the newline version stayed green, this one fails. NUL is not Unicode
    /// whitespace, so it survives the trim and actually reaches the cap.
    #[test]
    fn control_characters_do_not_consume_the_cap() {
        let padded = format!(
            "{}v0.1.0-pre.223",
            "\u{0}".repeat(MAX_CLIENT_VERSION_BYTES * 2)
        );
        assert_eq!(sanitize_client_version(&padded), "v0.1.0-pre.223");

        // The other way to survive the first trim: be whitespace, but interior.
        // Same branch, reached from the opposite direction — `trim()` leaves
        // these alone because they are not at an edge.
        let interior = format!("v0.1.0{}pre.223", "\n".repeat(MAX_CLIENT_VERSION_BYTES * 2));
        assert_eq!(sanitize_client_version(&interior), "v0.1.0pre.223");
    }

    /// Why the function trims twice. Filtering can *expose* padding the first
    /// trim could not see, because a control character is not whitespace and
    /// stands between it and the edge.
    #[test]
    fn filtering_does_not_leave_exposed_padding() {
        assert_eq!(sanitize_client_version("\u{0} v0.1.0 \u{0}"), "v0.1.0");
    }

    /// The cap is bytes, and this is the input that tells the two units apart.
    /// `Chars::take(64)` would have let 64 four-byte codepoints through as 256
    /// bytes — four times the budget the constant's own doc justifies in terms
    /// of what a peer can write to disk per connection.
    #[test]
    fn the_cap_counts_bytes_not_characters() {
        let wide = "🙂".repeat(1000);
        let clean = sanitize_client_version(&wide);
        assert!(
            clean.len() <= MAX_CLIENT_VERSION_BYTES,
            "{} bytes, over the {MAX_CLIENT_VERSION_BYTES}-byte budget",
            clean.len()
        );
        // Spent in whole characters: the budget divides exactly here, and a
        // string that stopped mid-codepoint could not exist as a `String` at
        // all — the loop breaks rather than slicing.
        assert_eq!(clean.chars().count(), MAX_CLIENT_VERSION_BYTES / 4);
    }

    /// The ordinary values still pass through untouched — a sanitiser that
    /// mangles the real input answers nothing.
    #[test]
    fn real_versions_survive_unchanged() {
        for v in ["v0.1.0-pre.223", "0.1.0-dev+a1b2c3d", "bot-sdk/0.1.0"] {
            assert_eq!(sanitize_client_version(v), v);
        }
        // Absent stays absent, so the caller's "unknown" branch still fires.
        assert_eq!(sanitize_client_version("   "), "");
    }
}
