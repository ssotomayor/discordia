use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type Id = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
    pub id: Id,
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Guild {
    pub id: Id,
    pub name: String,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Text,
    Voice,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Channel {
    pub id: Id,
    pub guild_id: Id,
    pub name: String,
    pub kind: ChannelKind,
    pub topic: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub user: User,
    pub guild_id: Id,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub id: Id,
    pub channel_id: Id,
    pub author: User,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

/// Voice presence: which voice channel a user is in and their audio flags.
/// `channel_id == None` means the user is not in any voice channel.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceState {
    pub user_id: Id,
    pub guild_id: Id,
    pub channel_id: Option<Id>,
    pub muted: bool,
    pub deafened: bool,
    pub speaking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum ClientMessage {
    Identify { username: String },
    FetchMessages { channel_id: Id, limit: u32 },
    SendMessage { channel_id: Id, content: String },
    /// Join a voice channel. Server replies with VoiceToken (LiveKit access
    /// token) and broadcasts VoiceStateUpdate.
    JoinVoice { channel_id: Id },
    /// Leave the current voice channel.
    LeaveVoice,
    /// Update mute/deafen flags for the current voice session.
    SetVoiceMute { muted: bool, deafened: bool },
    /// Local VAD says we're speaking (advisory; broadcast to others).
    SetSpeaking { speaking: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum ServerMessage {
    Ready {
        user: User,
        guilds: Vec<Guild>,
        channels: Vec<Channel>,
        members: Vec<Member>,
        voice_states: Vec<VoiceState>,
    },
    MessageHistory {
        channel_id: Id,
        messages: Vec<Message>,
    },
    MessageCreate(Message),
    MemberJoin(Member),
    MemberLeave {
        guild_id: Id,
        user_id: Id,
    },
    VoiceStateUpdate(VoiceState),
    /// LiveKit room access token, sent only to the user who joined.
    VoiceToken {
        channel_id: Id,
        livekit_url: String,
        token: String,
    },
    Error {
        message: String,
    },
}
