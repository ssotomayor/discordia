use uuid::Uuid;

use super::{AppState, durable};
use crate::poll::{NewPoll, StoredPoll};
use crate::protocol::{ClientMessage, Id, Message, Permission, ServerMessage, User};

impl AppState {
    pub async fn handle_poll(&self, user: &User, command: ClientMessage) -> Result<(), String> {
        match command {
            ClientMessage::CreatePoll {
                channel_id,
                question,
                options,
                anonymous,
                multiple,
                changeable,
                duration_secs,
            } => {
                let gid = self.chat_send_allowed(channel_id, user)?;
                let (question, options) = crate::protocol::validate_poll(&question, &options)?;
                let now = chrono::Utc::now();
                let poll = StoredPoll::new(
                    NewPoll {
                        options,
                        anonymous,
                        multiple,
                        changeable,
                        duration_secs,
                    },
                    now.timestamp_millis(),
                )?;
                let author = self.attributed_author(channel_id, user.clone());
                let message = Message {
                    poll: Some(poll.view(None)),
                    image_metadata: None,
                    attachment: None,
                    pinned: false,
                    id: Uuid::new_v4(),
                    channel_id,
                    author_bot: self.is_installed_bot(channel_id, &author.pubkey),
                    author,
                    content: question,
                    image: None,
                    reactions: Vec::new(),
                    reply_to: None,
                    created_at: now,
                    edited_at: None,
                };
                durable(
                    self.store
                        .insert_message_with_poll(&message, Some(&poll))
                        .await,
                    "poll insert",
                )?;
                self.deliver(
                    self.guild_member_pubkeys(gid),
                    ServerMessage::MessageCreate(message),
                );
                Ok(())
            }
            ClientMessage::VotePoll {
                channel_id,
                message_id,
                choices,
            } => {
                let gid = self.chat_channel(
                    channel_id,
                    user,
                    super::chat_tools::Needs::AppsOnly(Permission::AddReactions),
                )?;
                let _write = self.durable_writes.lock().await;
                let (_, mut poll) = self.stored_poll(channel_id, message_id).await?;
                let before = poll.clone();
                let mine = poll.vote(
                    &user.pubkey,
                    &choices,
                    chrono::Utc::now().timestamp_millis(),
                )?;
                if poll != before {
                    durable(
                        self.store.set_poll(channel_id, message_id, &poll).await,
                        "poll vote",
                    )?;
                }
                let others = self
                    .guild_member_pubkeys(gid)
                    .into_iter()
                    .filter(|p| *p != user.pubkey)
                    .collect();
                if poll != before {
                    self.deliver(others, poll_update(channel_id, message_id, &poll, None));
                }
                self.deliver(
                    vec![user.pubkey.clone()],
                    poll_update(channel_id, message_id, &poll, Some(mine)),
                );
                Ok(())
            }
            ClientMessage::ClosePoll {
                channel_id,
                message_id,
            } => {
                let gid = self.chat_channel(
                    channel_id,
                    user,
                    super::chat_tools::Needs::AppsOnly(Permission::SendMessages),
                )?;
                let _write = self.durable_writes.lock().await;
                let (author, mut poll) = self.stored_poll(channel_id, message_id).await?;
                if author != user.pubkey {
                    self.require_permission(gid, &user.pubkey, Permission::ManageMessages)?;
                }
                if !poll.close(chrono::Utc::now().timestamp_millis()) {
                    return Ok(());
                }
                durable(
                    self.store.set_poll(channel_id, message_id, &poll).await,
                    "poll close",
                )?;
                self.deliver(
                    self.guild_member_pubkeys(gid),
                    poll_update(channel_id, message_id, &poll, None),
                );
                Ok(())
            }
            _ => Err("unsupported poll command".into()),
        }
    }

    async fn stored_poll(
        &self,
        channel_id: Id,
        message_id: Id,
    ) -> Result<(String, StoredPoll), String> {
        self.store
            .poll(channel_id, message_id)
            .await
            .map_err(|e| format!("store error: {e}"))?
            .ok_or_else(|| "unknown poll".to_string())
    }
}

fn poll_update(
    channel_id: Id,
    message_id: Id,
    poll: &StoredPoll,
    mine: Option<Vec<u8>>,
) -> ServerMessage {
    ServerMessage::PollUpdate {
        channel_id,
        message_id,
        counts: poll.counts(),
        voters: poll.voters(),
        closed_at: poll.closed_at(),
        mine,
    }
}
