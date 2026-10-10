use base64::Engine as _;

use super::{AppState, durable};
use crate::protocol::{
    Attachment, ClientMessage, GuildEmoji, Id, Message, Permission, ServerMessage, User,
};

impl AppState {
    pub async fn sweep_files(&self) {
        let _write = self.durable_writes.lock().await;
        match self.store.referenced_media().await {
            Ok(references) => {
                self.media.sweep_files(&references);
            }
            Err(error) => tracing::error!(%error, "file expiry sweep skipped"),
        }
    }
    pub async fn file_available(&self, pubkey: &str, media: &str) -> bool {
        let Ok(channels) = self
            .store
            .live_file_channels(media, chrono::Utc::now().timestamp_millis())
            .await
        else {
            return false;
        };
        channels.into_iter().any(|id| {
            self.channels.get(&id).is_some_and(|channel| {
                self.is_guild_member(channel.guild_id, pubkey)
                    && self.can_see_channel(pubkey, &channel)
                    && self
                        .effective_permissions(channel.guild_id, pubkey)
                        .contains(&Permission::ReadMessageHistory)
            })
        })
    }
    fn chat_channel(
        &self,
        channel_id: Id,
        user: &User,
        permission: Permission,
    ) -> Result<Id, String> {
        let channel = self
            .channels
            .get(&channel_id)
            .map(|c| c.clone())
            .ok_or("unknown channel")?;
        if !self.can_see_channel(&user.pubkey, &channel)
            || !self.is_guild_member(channel.guild_id, &user.pubkey)
        {
            return Err("you don't have access to that channel".into());
        }
        self.require_permission(channel.guild_id, &user.pubkey, permission)?;
        Ok(channel.guild_id)
    }

    fn chat_send_allowed(&self, channel_id: Id, user: &User) -> Result<Id, String> {
        let gid = self.chat_channel(channel_id, user, Permission::SendMessages)?;
        let perms = self.effective_permissions(gid, &user.pubkey);
        let exempt = perms.contains(&Permission::ManageMessages)
            || perms.contains(&Permission::ManageChannels);
        if self.channel_read_only(channel_id) && !exempt {
            return Err("this channel is read-only".into());
        }
        if !exempt {
            self.slowmode_check(channel_id, &user.pubkey)
                .map_err(|wait| format!("slowmode: wait {wait}s before posting again"))?;
        }
        Ok(gid)
    }

    pub async fn handle_chat_tool(
        &self,
        user: &User,
        command: ClientMessage,
    ) -> Result<Option<ServerMessage>, String> {
        match command {
            ClientMessage::FetchGuildFilePolicy { guild_id } => {
                if !self.is_guild_member(guild_id, &user.pubkey) {
                    return Err("you don't belong to that community".into());
                }
                Ok(Some(ServerMessage::GuildFilePolicy {
                    guild_id,
                    policy: self
                        .store
                        .file_policy(guild_id)
                        .await
                        .map_err(|e| e.to_string())?,
                }))
            }
            ClientMessage::SetGuildFilePolicy { guild_id, policy } => {
                let _write = self.durable_writes.lock().await;
                self.require_permission(guild_id, &user.pubkey, Permission::ManageGuild)?;
                if policy.max_bytes == 0
                    || policy.max_bytes > 2_000_000
                    || !(1..=365).contains(&policy.retention_days)
                {
                    return Err(
                        "file limit must be 1 byte to 2 MB; expiry must be 1 to 365 days".into(),
                    );
                }
                durable(
                    self.store.set_file_policy(guild_id, &policy).await,
                    "file policy",
                )?;
                self.deliver(
                    self.guild_member_pubkeys(guild_id),
                    ServerMessage::GuildFilePolicy { guild_id, policy },
                );
                Ok(None)
            }
            ClientMessage::SearchMessages {
                channel_id,
                request_id,
                mut query,
                offset,
            } => {
                self.chat_channel(channel_id, user, Permission::ReadMessageHistory)?;
                if query.text.len() > 200
                    || query
                        .author
                        .as_ref()
                        .is_some_and(|a| !crate::protocol::is_pubkey_hex(a))
                {
                    return Err("invalid search filter".into());
                }
                query.text = query.text.trim().to_owned();
                let messages = self
                    .store
                    .search_messages(channel_id, &query, offset)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(Some(ServerMessage::MessageSearchResults {
                    channel_id,
                    request_id,
                    messages,
                }))
            }
            ClientMessage::PinMessage {
                channel_id,
                message_id,
                pinned,
            } => {
                let _write = self.durable_writes.lock().await;
                let gid = self.chat_channel(channel_id, user, Permission::ManageMessages)?;
                if pinned {
                    let pins = self
                        .store
                        .search_messages(
                            channel_id,
                            &crate::protocol::MessageSearch {
                                pinned_only: true,
                                ..Default::default()
                            },
                            0,
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    if pins.len() >= 50 && !pins.iter().any(|m| m.id == message_id) {
                        return Err("this channel already has 50 pinned messages".into());
                    }
                }
                if !durable(
                    self.store
                        .set_message_pin(channel_id, message_id, pinned)
                        .await,
                    "message pin",
                )? {
                    return Err("unknown message".into());
                }
                self.deliver(
                    self.guild_member_pubkeys(gid),
                    ServerMessage::MessagePin {
                        channel_id,
                        message_id,
                        pinned,
                    },
                );
                Ok(None)
            }
            ClientMessage::SendFile {
                request_id,
                channel_id,
                name,
                data_url,
                content,
                reply_to,
            } => {
                let gid = self.chat_send_allowed(channel_id, user)?;
                let _write = self.durable_writes.lock().await;
                let policy = self
                    .store
                    .file_policy(gid)
                    .await
                    .map_err(|e| e.to_string())?;
                let name = file_name(&name)?;
                if content.len() > 2000 || data_url.len() > 2_700_000 {
                    return Err("file messages allow 2000 characters and a file up to 2 MB".into());
                }
                let payload = data_url
                    .strip_prefix("data:application/octet-stream;base64,")
                    .ok_or("invalid file data")?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(payload)
                    .map_err(|_| "invalid file data")?;
                if bytes.is_empty() || bytes.len() as u64 > policy.max_bytes {
                    return Err(format!(
                        "this community's file limit is {} KB",
                        policy.max_bytes / 1000
                    ));
                }
                self.charge_upload(&user.pubkey, data_url.len() as u64)?;
                let media = self
                    .media
                    .store_file_data_url(&data_url)
                    .map_err(|e| e.to_string())?;
                let reply_to = match reply_to {
                    Some(id) => self
                        .store
                        .reply_ref(channel_id, id)
                        .await
                        .map_err(|e| e.to_string())?,
                    None => None,
                };
                let message = Message {
                    id: uuid::Uuid::new_v4(),
                    channel_id,
                    author: user.clone(),
                    content: content.trim().to_owned(),
                    image: None,
                    attachment: Some(Attachment {
                        name,
                        media,
                        bytes: bytes.len() as u64,
                        expires_ms: Some(
                            chrono::Utc::now().timestamp_millis()
                                + i64::from(policy.retention_days) * 86_400_000,
                        ),
                    }),
                    pinned: false,
                    reactions: Vec::new(),
                    reply_to,
                    created_at: chrono::Utc::now(),
                };
                durable(
                    self.store.insert_message(&message).await,
                    "file message insert",
                )?;
                self.deliver(
                    self.guild_member_pubkeys(gid),
                    ServerMessage::MessageCreate(message),
                );
                Ok(Some(ServerMessage::FileUploadResult {
                    request_id,
                    error: None,
                }))
            }
            ClientMessage::FetchGuildStickers { guild_id } => {
                if !self.is_guild_member(guild_id, &user.pubkey) {
                    return Err("you don't belong to that community".into());
                }
                let stickers = self
                    .store
                    .stickers(guild_id)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(Some(ServerMessage::GuildStickers { guild_id, stickers }))
            }
            ClientMessage::CreateGuildSticker {
                guild_id,
                name,
                image,
            } => {
                let _write = self.durable_writes.lock().await;
                self.require_permission(guild_id, &user.pubkey, Permission::ManageEmojis)?;
                let name = name.trim().to_ascii_lowercase();
                if !super::valid_shortcode(&name) {
                    return Err("sticker name must be 2-32 characters: a-z, 0-9 or _".into());
                }
                let stickers = self
                    .store
                    .stickers(guild_id)
                    .await
                    .map_err(|e| e.to_string())?;
                if stickers.len() >= 128 || stickers.iter().any(|s| s.shortcode == name) {
                    return Err(
                        "sticker name already exists or the community has 128 stickers".into(),
                    );
                }
                let image = self.store_upload(&user.pubkey, &image)?;
                let sticker = GuildEmoji {
                    id: uuid::Uuid::new_v4(),
                    guild_id,
                    shortcode: name,
                    image,
                    added_by: user.pubkey.clone(),
                    created_ms: chrono::Utc::now().timestamp_millis(),
                };
                durable(self.store.insert_sticker(&sticker).await, "sticker insert")?;
                let mut stickers = stickers;
                stickers.push(sticker);
                self.deliver(
                    self.guild_member_pubkeys(guild_id),
                    ServerMessage::GuildStickers { guild_id, stickers },
                );
                Ok(None)
            }
            ClientMessage::DeleteGuildSticker {
                guild_id,
                sticker_id,
            } => {
                let _write = self.durable_writes.lock().await;
                self.require_permission(guild_id, &user.pubkey, Permission::ManageEmojis)?;
                if !durable(
                    self.store.delete_sticker(guild_id, sticker_id).await,
                    "sticker delete",
                )? {
                    return Err("unknown sticker".into());
                }
                let stickers = self
                    .store
                    .stickers(guild_id)
                    .await
                    .map_err(|e| e.to_string())?;
                self.deliver(
                    self.guild_member_pubkeys(guild_id),
                    ServerMessage::GuildStickers { guild_id, stickers },
                );
                Ok(None)
            }
            ClientMessage::SendSticker {
                channel_id,
                sticker_id,
            } => {
                let gid = self.chat_send_allowed(channel_id, user)?;
                let sticker = self
                    .store
                    .stickers(gid)
                    .await
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .find(|s| s.id == sticker_id)
                    .ok_or("unknown sticker")?;
                let image = self
                    .media
                    .inline(&sticker.image)
                    .ok_or("sticker image is unavailable")?;
                let message = self
                    .push_message(channel_id, user.clone(), String::new(), Some(image), None)
                    .await?;
                self.deliver(
                    self.guild_member_pubkeys(gid),
                    ServerMessage::MessageCreate(message),
                );
                Ok(None)
            }
            _ => Err("unsupported chat command".into()),
        }
    }
}

fn file_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    if name.is_empty()
        || name.len() > 240
        || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        || name.chars().any(char::is_control)
        || matches!(name, "." | "..")
    {
        return Err("invalid file name".into());
    }
    Ok(crate::protocol::sanitize_line(name, 240))
}
