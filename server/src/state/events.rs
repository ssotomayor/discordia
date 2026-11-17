use chrono::Utc;
use uuid::Uuid;

use super::{AppState, durable};
use crate::protocol::{
    ChannelKind, ClientMessage, EventNotice, GuildEvent, Id, Message, Permission, ServerMessage,
    User,
};

impl AppState {
    pub async fn handle_event_command(
        &self,
        user: &User,
        command: ClientMessage,
    ) -> Result<Option<ServerMessage>, String> {
        match command {
            ClientMessage::FetchGuildEvents { guild_id } => {
                if !self.is_guild_member(guild_id, &user.pubkey) {
                    return Err("you're not a member of this guild".into());
                }
                let events = self.events_of(guild_id).await?;
                Ok(Some(ServerMessage::GuildEvents {
                    guild_id,
                    events: self.events_seen_by(events, &user.pubkey),
                }))
            }
            ClientMessage::SaveGuildEvent {
                guild_id,
                event_id,
                draft,
            } => {
                let _write = self.durable_writes.lock().await;
                self.require_permission(guild_id, &user.pubkey, Permission::ManageEvents)?;
                let draft = draft.validated()?;
                if let Some(channel_id) = draft.channel_id {
                    let channel = self
                        .channel(channel_id)
                        .filter(|c| c.guild_id == guild_id)
                        .filter(|c| matches!(c.kind, ChannelKind::Text | ChannelKind::Voice))
                        .ok_or("an event is hosted in a text or voice channel of this guild")?;
                    if !self.can_see_channel(&user.pubkey, &channel) {
                        return Err("you can't host an event in a channel you can't see".into());
                    }
                }
                let existing = self.events_of(guild_id).await?;
                let previous = match event_id {
                    Some(id) => Some(
                        existing
                            .iter()
                            .find(|e| e.id == id)
                            .cloned()
                            .ok_or("unknown event")?,
                    ),
                    None if existing.len() >= crate::protocol::GUILD_MAX_EVENTS => {
                        return Err(format!(
                            "a guild holds at most {} events",
                            crate::protocol::GUILD_MAX_EVENTS
                        ));
                    }
                    None => None,
                };
                let cover = match draft.cover {
                    None => None,
                    Some(cover) => {
                        let policy = self.file_policy(guild_id).await?;
                        let limit = (policy.max_avatar_bytes as usize)
                            .min(crate::protocol::MAX_GUILD_UPLOAD_BYTES);
                        Some(
                            self.image_reference_with_limit(&user.pubkey, &cover, limit)
                                .map_err(|e| format!("event cover: {e}"))?,
                        )
                    }
                };
                let event = GuildEvent {
                    id: previous.as_ref().map_or_else(Uuid::new_v4, |p| p.id),
                    guild_id,
                    name: draft.name,
                    description: draft.description,
                    starts_at: draft.starts_at,
                    ends_at: draft.ends_at,
                    channel_id: draft.channel_id,
                    repeat: draft.repeat,
                    cover,
                    created_by: previous.map_or_else(|| user.pubkey.clone(), |p| p.created_by),
                };
                durable(self.store.upsert_guild_event(&event).await, "event save")?;
                drop(_write);
                self.share_events(guild_id).await;
                Ok(None)
            }
            ClientMessage::DeleteGuildEvent { guild_id, event_id } => {
                let _write = self.durable_writes.lock().await;
                self.require_permission(guild_id, &user.pubkey, Permission::ManageEvents)?;
                if !durable(
                    self.store.delete_guild_event(guild_id, event_id).await,
                    "event delete",
                )? {
                    return Err("unknown event".into());
                }
                drop(_write);
                self.share_events(guild_id).await;
                Ok(None)
            }
            _ => Err("unsupported event command".into()),
        }
    }

    async fn events_of(&self, guild_id: Id) -> Result<Vec<GuildEvent>, String> {
        self.store
            .guild_events(guild_id)
            .await
            .map_err(|e| format!("store error: {e}"))
    }

    /// An event names its host channel, so it goes only where that channel does (trap 30).
    fn event_seen_by(&self, event: &GuildEvent, pubkey: &str) -> bool {
        event
            .channel_id
            .and_then(|c| self.channel(c))
            .is_none_or(|channel| self.can_see_channel(pubkey, &channel))
    }

    fn events_seen_by(&self, events: Vec<GuildEvent>, pubkey: &str) -> Vec<GuildEvent> {
        events
            .into_iter()
            .filter(|e| self.event_seen_by(e, pubkey))
            .collect()
    }

    /// Each member's own list: after any change to an event, or to who sees a channel.
    pub async fn share_events(&self, guild_id: Id) {
        let events = match self.events_of(guild_id).await {
            Ok(events) => events,
            Err(error) => {
                tracing::error!(%error, "event list not shared");
                return;
            }
        };
        for member in self.guild_member_pubkeys(guild_id) {
            let seen = self.events_seen_by(events.clone(), &member);
            self.deliver(
                vec![member],
                ServerMessage::GuildEvents {
                    guild_id,
                    events: seen,
                },
            );
        }
    }

    /// Posts one notice per run as it starts. A run already under way when the
    /// server comes back is still announced; one that ended while it was down is not.
    pub async fn announce_events(&self) {
        let rows = match self.store.announceable_events().await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, "event notices skipped");
                return;
            }
        };
        let now = Utc::now();
        for (event, announced_ms) in rows {
            let Some((start, end)) = event.occurrence(now) else {
                continue;
            };
            if now < start || now >= end || announced_ms >= start.timestamp_millis() {
                continue;
            }
            if let Err(error) = self.announce(&event, start.timestamp_millis()).await {
                tracing::warn!(%error, event = %event.id, "event notice not posted");
            }
        }
    }

    async fn announce(&self, event: &GuildEvent, run_start_ms: i64) -> Result<(), String> {
        let host = event.channel_id.and_then(|c| self.channel(c));
        // A hidden host would be named to everyone in a text channel.
        if host.as_ref().is_some_and(|c| c.access.is_some()) {
            return Ok(());
        }
        let notice_channel = host
            .filter(|c| c.kind == ChannelKind::Text && c.guild_id == event.guild_id)
            .or_else(|| self.first_text_channel(event.guild_id))
            .ok_or("no text channel to post in")?;
        let _write = self.durable_writes.lock().await;
        // Recorded first: a lost notice is better than one repeated every tick.
        if !durable(
            self.store.set_event_announced(event.id, run_start_ms).await,
            "event announce",
        )? {
            return Ok(());
        }
        let author = self
            .users
            .get(&event.created_by)
            .map(|u| u.clone())
            .unwrap_or_else(|| User {
                pubkey: event.created_by.clone(),
                username: String::new(),
            });
        let message = Message {
            poll: None,
            event_notice: Some(EventNotice {
                event_id: event.id,
                channel_id: event.channel_id,
            }),
            image_metadata: None,
            attachment: None,
            pinned: false,
            id: Uuid::new_v4(),
            channel_id: notice_channel.id,
            author_bot: false,
            author,
            content: event.name.clone(),
            image: None,
            reactions: Vec::new(),
            reply_to: None,
            created_at: Utc::now(),
            edited_at: None,
        };
        durable(self.store.insert_message(&message).await, "event notice")?;
        drop(_write);
        self.deliver(
            self.guild_member_pubkeys(event.guild_id),
            ServerMessage::MessageCreate(message),
        );
        Ok(())
    }

    fn first_text_channel(&self, guild_id: Id) -> Option<crate::protocol::Channel> {
        self.channels
            .iter()
            .filter(|c| c.guild_id == guild_id && c.kind == ChannelKind::Text)
            .map(|c| c.clone())
            .min_by_key(|c| (c.position, c.id))
    }
}
