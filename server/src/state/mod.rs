mod seed;

use std::sync::RwLock;

use dashmap::DashMap;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::protocol::{Channel, Guild, Id, Member, Message, ServerMessage, User, VoiceState};

const BROADCAST_CAPACITY: usize = 256;

pub struct AppState {
    pub guilds: DashMap<Id, Guild>,
    pub channels: DashMap<Id, Channel>,
    pub channels_by_guild: DashMap<Id, Vec<Id>>,
    pub messages: DashMap<Id, RwLock<Vec<Message>>>,
    /// Members per guild, by user pubkey.
    pub members: DashMap<Id, DashMap<String, Member>>,
    /// Voice state per user pubkey (global; a user can only be in one voice
    /// channel at a time across all guilds, same as Discord).
    pub voice_states: DashMap<String, VoiceState>,
    pub hub: broadcast::Sender<ServerMessage>,
}

impl AppState {
    pub fn seeded() -> Self {
        let (hub, _) = broadcast::channel(BROADCAST_CAPACITY);
        let state = Self {
            guilds: DashMap::new(),
            channels: DashMap::new(),
            channels_by_guild: DashMap::new(),
            messages: DashMap::new(),
            members: DashMap::new(),
            voice_states: DashMap::new(),
            hub,
        };
        seed::populate(&state);
        state
    }

    pub fn snapshot_for(&self, user: &User) -> ServerMessage {
        let guilds: Vec<Guild> = self.guilds.iter().map(|g| g.value().clone()).collect();
        let channels: Vec<Channel> = self.channels.iter().map(|c| c.value().clone()).collect();

        let mut members: Vec<Member> = Vec::new();
        for entry in self.members.iter() {
            for m in entry.value().iter() {
                members.push(m.value().clone());
            }
        }

        for guild in &guilds {
            let member = Member {
                user: user.clone(),
                guild_id: guild.id,
                online: true,
            };
            self.members
                .entry(guild.id)
                .or_insert_with(DashMap::new)
                .insert(user.pubkey.clone(), member.clone());
            members.push(member);
        }

        let voice_states: Vec<VoiceState> = self
            .voice_states
            .iter()
            .map(|v| v.value().clone())
            .collect();

        ServerMessage::Ready {
            user: user.clone(),
            guilds,
            channels,
            members,
            voice_states,
        }
    }

    pub fn history(&self, channel_id: Id, limit: u32) -> Vec<Message> {
        let Some(entry) = self.messages.get(&channel_id) else {
            return Vec::new();
        };
        let guard = entry.read().unwrap();
        let limit = limit as usize;
        if guard.len() <= limit {
            guard.clone()
        } else {
            guard[guard.len() - limit..].to_vec()
        }
    }

    pub fn push_message(&self, channel_id: Id, author: User, content: String) -> Option<Message> {
        let kind_ok = self
            .channels
            .get(&channel_id)
            .map(|c| matches!(c.kind, crate::protocol::ChannelKind::Text))
            .unwrap_or(false);
        if !kind_ok {
            return None;
        }
        let message = Message {
            id: Uuid::new_v4(),
            channel_id,
            author,
            content,
            created_at: chrono::Utc::now(),
        };
        self.messages
            .entry(channel_id)
            .or_insert_with(|| RwLock::new(Vec::new()))
            .write()
            .unwrap()
            .push(message.clone());
        Some(message)
    }

    /// Mark a user offline in every guild they're a member of. Returns the
    /// guild_id + pubkey pairs that were actually flipped (so callers know
    /// which broadcasts to send).
    pub fn mark_offline(&self, user_pubkey: &str) -> Vec<(Id, String)> {
        let mut affected = Vec::new();
        for entry in self.members.iter() {
            let guild_id = *entry.key();
            if let Some(mut m) = entry.value().get_mut(user_pubkey) {
                if m.online {
                    m.online = false;
                    affected.push((guild_id, user_pubkey.to_string()));
                }
            }
        }
        affected
    }

    pub fn voice_channel_guild(&self, channel_id: Id) -> Option<Id> {
        self.channels.get(&channel_id).and_then(|c| {
            matches!(c.kind, crate::protocol::ChannelKind::Voice).then_some(c.guild_id)
        })
    }

    pub fn set_voice_channel(
        &self,
        user_pubkey: &str,
        guild_id: Id,
        channel_id: Option<Id>,
    ) -> VoiceState {
        let prev = self.voice_states.get(user_pubkey).map(|v| v.clone());
        let state = VoiceState {
            user_pubkey: user_pubkey.to_string(),
            guild_id,
            channel_id,
            muted: prev.as_ref().map(|p| p.muted).unwrap_or(false),
            deafened: prev.as_ref().map(|p| p.deafened).unwrap_or(false),
            speaking: false,
        };
        if channel_id.is_some() {
            self.voice_states.insert(user_pubkey.to_string(), state.clone());
        } else {
            self.voice_states.remove(user_pubkey);
        }
        state
    }

    pub fn update_voice_flags(
        &self,
        user_pubkey: &str,
        muted: bool,
        deafened: bool,
    ) -> Option<VoiceState> {
        let mut entry = self.voice_states.get_mut(user_pubkey)?;
        entry.muted = muted;
        entry.deafened = deafened || muted;
        Some(entry.clone())
    }

    pub fn update_speaking(&self, user_pubkey: &str, speaking: bool) -> Option<VoiceState> {
        let mut entry = self.voice_states.get_mut(user_pubkey)?;
        if entry.speaking == speaking {
            return None;
        }
        entry.speaking = speaking;
        Some(entry.clone())
    }

    /// Tombstone voice state for a disconnecting user; returns the cleared state if any.
    pub fn clear_voice(&self, user_pubkey: &str) -> Option<VoiceState> {
        let prev = self.voice_states.remove(user_pubkey)?.1;
        Some(VoiceState {
            channel_id: None,
            speaking: false,
            ..prev
        })
    }
}
