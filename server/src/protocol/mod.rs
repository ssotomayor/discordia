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
    /// Public key of the user who created the guild. The seeded "Lobby" guild
    /// has an empty owner and therefore cannot be deleted by anyone. Only the
    /// owner may delete their guild.
    #[serde(default)]
    pub owner_pubkey: String,
    /// Optional owner-chosen accent color (CSS hex) that tints the UI while
    /// viewing this guild.
    #[serde(default)]
    pub accent: Option<String>,
}

/// A guild as it appears in the public directory (browse-and-join). Carries
/// no channels/messages — just enough to show a row with a Join button.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuildSummary {
    pub id: Id,
    pub name: String,
    pub icon: Option<String>,
    pub member_count: u32,
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

/// A user's public profile, keyed by pubkey and distributed independently of
/// `User` (which is embedded in every message/member — putting avatar bytes
/// there would duplicate them everywhere). The avatar is a small
/// `data:image/...;base64,...` URL the user owns locally and uploads on
/// connect. Looked up by pubkey wherever a user is rendered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Profile {
    pub pubkey: String,
    #[serde(default)]
    pub avatar: Option<String>,
    /// Wide banner image (data URL) shown across the top of the profile card.
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    /// Presence: "online" | "away" | "dnd" (defaults to online when absent).
    #[serde(default)]
    pub status: Option<String>,
    /// Free-text custom status line shown on the profile card.
    #[serde(default)]
    pub custom_status: Option<String>,
}

/// An emoji reaction on a message and the pubkeys who added it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: String,
    pub users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub id: Id,
    pub channel_id: Id,
    pub author: User,
    pub content: String,
    /// Optional inline image, carried as a `data:image/...;base64,...` URL.
    /// Kept on the message itself (rather than a separate upload/store) to
    /// fit the in-memory, broadcast-everything design. Size-capped server-side.
    #[serde(default)]
    pub image: Option<String>,
    /// Emoji reactions, by emoji.
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    pub created_at: DateTime<Utc>,
}

/// A one-to-one direct-message channel, described from the viewpoint of one
/// participant: `other` is the *other* person in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DmInfo {
    pub channel_id: Id,
    pub other: User,
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
        /// Optional inline image as a `data:image/...;base64,...` URL.
        #[serde(default)]
        image: Option<String>,
    },
    /// Create a new guild. The server seeds it with a default text + voice
    /// channel and makes the requesting user its first (and only) member.
    CreateGuild {
        name: String,
    },
    /// Join an existing guild from the directory. The server adds the user as
    /// a member and replies with `GuildJoined`.
    JoinGuild {
        guild_id: Id,
    },
    /// Delete a guild. Only honoured if the requester is the guild's owner.
    DeleteGuild {
        guild_id: Id,
    },
    /// Open (or fetch the existing) direct-message channel with another user.
    /// The server replies with `DmReady` and notifies the other participant.
    OpenDm {
        user_pubkey: String,
    },
    /// Publish (or update) the sender's public profile. Sent on connect from
    /// the client's locally-owned `profile.json`, and again whenever edited.
    SetProfile {
        #[serde(default)]
        avatar: Option<String>,
        #[serde(default)]
        banner: Option<String>,
        #[serde(default)]
        bio: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        custom_status: Option<String>,
    },
    /// Toggle an emoji reaction on a message (add if absent, remove if present).
    React {
        channel_id: Id,
        message_id: Id,
        emoji: String,
    },
    /// Signal that the sender is typing in a channel (ephemeral, broadcast to
    /// the channel's audience; not stored).
    Typing {
        channel_id: Id,
    },
    /// Owner-only: set (or clear) a guild's accent color.
    SetGuildAccent {
        guild_id: Id,
        #[serde(default)]
        accent: Option<String>,
    },
    /// Announce that the sender started/stopped sharing their screen in a
    /// (voice) channel, so others can show a LIVE badge.
    SetScreenShare {
        channel_id: Id,
        sharing: bool,
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
        /// Only the guilds the user is a member of.
        guilds: Vec<Guild>,
        /// Channels of the user's guilds only.
        channels: Vec<Channel>,
        /// Members of the user's guilds only.
        members: Vec<Member>,
        voice_states: Vec<VoiceState>,
        /// The requesting user's existing direct-message conversations.
        #[serde(default)]
        dms: Vec<DmInfo>,
        /// Public directory of all guilds on the host (for browse-and-join).
        #[serde(default)]
        catalog: Vec<GuildSummary>,
        /// Known user profiles (avatars/bios), keyed implicitly by pubkey.
        #[serde(default)]
        profiles: Vec<Profile>,
    },
    MessageHistory {
        channel_id: Id,
        messages: Vec<Message>,
    },
    MessageCreate(Message),
    /// Sent to a user who just created or joined a guild: the guild, its
    /// channels and its current members, so their client can render and
    /// select it. Only the acting user receives this.
    GuildJoined {
        guild: Guild,
        channels: Vec<Channel>,
        members: Vec<Member>,
    },
    /// A guild was deleted by its owner. Delivered to the (former) members so
    /// they drop it from local state.
    GuildDelete {
        guild_id: Id,
    },
    /// Refreshed public directory of guilds. Broadcast whenever a guild is
    /// created, joined, or deleted so browse lists and member counts stay live.
    GuildCatalog {
        guilds: Vec<GuildSummary>,
    },
    /// Reply to `OpenDm` for the requester: the DM channel plus its history.
    DmReady {
        channel_id: Id,
        other: User,
        messages: Vec<Message>,
    },
    /// Sent to the *other* participant when someone opens a DM with them, so
    /// the conversation appears in their sidebar.
    DmCreate(DmInfo),
    /// A user published or changed their profile. Broadcast to everyone so
    /// avatars/bios stay current wherever that user is shown.
    ProfileUpdate(Profile),
    MemberJoin(Member),
    MemberLeave {
        guild_id: Id,
        user_pubkey: String,
    },
    /// Updated reaction set for a single message.
    ReactionUpdate {
        channel_id: Id,
        message_id: Id,
        reactions: Vec<Reaction>,
    },
    /// Someone is typing in a channel (ephemeral).
    TypingUpdate {
        channel_id: Id,
        user_pubkey: String,
        username: String,
    },
    /// A guild's metadata changed (e.g. its accent). Delivered to members.
    GuildUpdate(Guild),
    /// Current set of users sharing their screen in a channel.
    ScreenShareState {
        channel_id: Id,
        sharers: Vec<String>,
    },
    VoiceStateUpdate(VoiceState),
    VoiceToken {
        channel_id: Id,
        livekit_url: String,
        token: String,
    },
    /// Token for the webview JS client to join the screen-share room for a
    /// channel (sent alongside `VoiceToken` on join). Used to publish/view
    /// screen shares; never touches the native-audio path.
    ScreenToken {
        channel_id: Id,
        livekit_url: String,
        token: String,
    },
    Error {
        message: String,
    },
}
