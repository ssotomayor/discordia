use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Server-generated identifier (guild, channel, message ids). User identity
/// is NOT a Uuid — it's a Solana-format Ed25519 public key (see
/// `User.pubkey`).
pub type Id = Uuid;

/// A user is identified universally by their Ed25519 public key encoded as
/// base58 (Solana address format). Display name is a cosmetic label and may
/// not be unique.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
    pub pubkey: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VoiceState {
    pub user_pubkey: String,
    pub guild_id: Id,
    pub channel_id: Option<Id>,
    pub muted: bool,
    pub deafened: bool,
    pub speaking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Sent in response to a `Hello { nonce }`. Signature is base58
    /// `ed25519(nonce || pubkey || username)`.
    Identify {
        username: String,
        pubkey: String,
        signature: String,
    },
    FetchMessages {
        channel_id: Id,
        limit: u32,
    },
    SendMessage {
        channel_id: Id,
        content: String,
    },
    JoinVoice {
        channel_id: Id,
    },
    LeaveVoice,
    SetVoiceMute {
        muted: bool,
        deafened: bool,
    },
    SetSpeaking {
        speaking: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum ServerMessage {
    /// First frame the server sends after the WebSocket upgrades. Carries a
    /// random per-connection nonce the client must sign for `Identify`.
    Hello {
        nonce: String,
    },
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
        user_pubkey: String,
    },
    VoiceStateUpdate(VoiceState),
    VoiceToken {
        channel_id: Id,
        livekit_url: String,
        token: String,
    },
    Error {
        message: String,
    },
}
