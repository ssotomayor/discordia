//! Wire protocol shared by the dioxusfun client, server, and bot SDK.
//!
//! This is the single source of truth for every frame on the gateway
//! WebSocket. It deliberately depends on nothing heavy (just serde + uuid +
//! chrono) so a bot author can pull it in without dragging axum/tokio/livekit
//! along.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod rendezvous;

/// Server-generated identifier (guild, channel, message ids). User identity
/// is NOT a Uuid — it's a Nostr-format secp256k1 public key (see
/// `User.pubkey`).
pub type Id = Uuid;

/// A user is identified universally by their x-only secp256k1 public key as
/// 64 hex chars (Nostr format). Display name is a cosmetic label and may
/// not be unique.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
    pub pubkey: String,
    pub username: String,
}

/// Whether a guild appears in the public directory or is joinable only by
/// invite code.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GuildVisibility {
    #[default]
    Public,
    Private,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Guild {
    pub id: Id,
    pub name: String,
    /// Short initials label used when no `icon_image` is set.
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
    /// Private guilds are hidden from the catalog and joinable only by invite.
    #[serde(default)]
    pub visibility: GuildVisibility,
    /// One-line description shown in the directory and settings.
    #[serde(default)]
    pub description: Option<String>,
    /// Image icon (http(s) or data URL), preferred over `icon` when set.
    #[serde(default)]
    pub icon_image: Option<String>,
    /// Wide banner image (http(s) or data URL) for the guild header.
    #[serde(default)]
    pub banner: Option<String>,
    /// Message retention in days (None = keep forever). Messages older than
    /// this are deleted by the server's hourly sweep. Set via
    /// `SetGuildRetention` (requires `ManageGuild`).
    #[serde(default)]
    pub retention_days: Option<u32>,
    /// Friction applied to new joiners (`SetJoinGate`, requires `ManageGuild`).
    #[serde(default)]
    pub join_gate: JoinGate,
    /// Rules text shown by the `Rules` gate; the joiner must accept.
    #[serde(default)]
    pub rules: Option<String>,
    /// Anti-raid lockdown: while true, ALL joins are rejected. Toggled by
    /// `SetPanicMode` (ManageGuild) and auto-enabled by mass-join detection.
    #[serde(default)]
    pub panic_mode: bool,
}

/// Join friction options (the ban-evasion counterweight — a fresh keypair is
/// free, but passing the gate isn't).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JoinGate {
    /// Anyone may join (public guilds) — today's behavior.
    #[default]
    Open,
    /// The joiner must read and accept the guild's `rules` text.
    Rules,
    /// The joiner must solve a proof-of-work challenge (SHA-256 leading zero
    /// bits) — cheap for one human, expensive for a keygen raid.
    Pow,
}

/// One row of a guild's moderation audit log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEntry {
    /// Unix milliseconds.
    pub at_ms: i64,
    pub actor_pubkey: String,
    /// Machine-readable action tag, e.g. "kick", "ban", "role_create".
    pub action: String,
    /// Target of the action (pubkey, role/channel name, …), if any.
    #[serde(default)]
    pub target: String,
    /// Free-form detail for display.
    #[serde(default)]
    pub detail: String,
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
    /// Read-only (announcements): only holders of `ManageMessages` or
    /// `ManageChannels` may post.
    #[serde(default)]
    pub read_only: bool,
    /// Per-user slowmode: seconds a member must wait between messages here
    /// (0 = off). Moderators (`ManageMessages`/`ManageChannels`) are exempt.
    #[serde(default)]
    pub slowmode_secs: u32,
    /// Sort order within the guild's channel list (lower renders first).
    #[serde(default)]
    pub position: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub user: User,
    pub guild_id: Id,
    pub online: bool,
    /// True if this member is an installed bot (an "application" identity
    /// rather than a human). Cosmetic on the wire; the server is the authority
    /// on what a bot may actually do (see `BotInstall`).
    #[serde(default)]
    pub bot: bool,
    /// Ids of the guild roles assigned to this member. Effective permissions
    /// are the union over these roles; roles never apply to bot connections.
    #[serde(default)]
    pub roles: Vec<Id>,
    /// Server-authoritative message-XP earned IN THIS GUILD, driving the level
    /// display. Per-guild by design: a level is standing in one community, not
    /// a cross-guild reputation. Stamped by the server on emit; see
    /// `level_progress`.
    #[serde(default)]
    pub xp: u64,
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

/// Map total message-XP to `(level, xp_into_level, level_span)` for the level
/// badge + progress bar. Level 1 costs 10 XP, each level 10 more than the last
/// (10, 20, 30, …) — a gentle curve where early levels come fast. Shared by
/// client (render) and server (decide when to broadcast a level-up).
pub fn level_progress(xp: u64) -> (u32, u64, u64) {
    let mut level: u32 = 1;
    let mut remaining = xp;
    loop {
        let span = 10 + (level as u64 - 1) * 10;
        if remaining < span {
            return (level, remaining, span);
        }
        remaining -= span;
        level += 1;
    }
}

#[cfg(test)]
mod level_tests {
    use super::level_progress;

    #[test]
    fn level_curve() {
        assert_eq!(level_progress(0), (1, 0, 10)); // fresh: level 1, 0/10
        assert_eq!(level_progress(9), (1, 9, 10));
        assert_eq!(level_progress(10), (2, 0, 20)); // rolled into level 2
        assert_eq!(level_progress(29), (2, 19, 20));
        assert_eq!(level_progress(30), (3, 0, 30));
        // Level only ever increases with XP.
        let mut last = 0;
        for xp in 0..1000 {
            let l = level_progress(xp).0;
            assert!(l >= last);
            last = l;
        }
    }
}

#[cfg(test)]
mod camera_wire_tests {
    use super::{ClientMessage, VoiceState};

    /// A `VoiceState` from a server older than the camera has no `camera_on`
    /// key at all. `#[serde(default)]` is what stops that being a hard parse
    /// error that would take the whole voice roster down, so it is worth a test
    /// of its own: the attribute is one word, easily dropped in a refactor, and
    /// nothing else here would notice.
    #[test]
    fn a_voice_state_without_camera_on_still_parses() {
        let old = r#"{
            "user_pubkey": "abc",
            "guild_id": "00000000-0000-0000-0000-000000000001",
            "channel_id": null,
            "muted": false,
            "deafened": false,
            "speaking": true
        }"#;
        let vs: VoiceState = serde_json::from_str(old).expect("older server's frame still parses");
        assert!(vs.speaking, "the fields that were there survive");
        assert!(!vs.camera_on, "the absent one defaults to off, not on");
    }

    /// The tag/content envelope, not the field: a typo in the variant name is
    /// invisible to the compiler on both sides but silently unroutable.
    #[test]
    fn set_camera_round_trips_over_the_wire() {
        let json = serde_json::to_string(&ClientMessage::SetCamera { on: true }).unwrap();
        assert_eq!(json, r#"{"op":"set_camera","d":{"on":true}}"#);
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ClientMessage::SetCamera { on: true }));
    }
}

/// An emoji reaction on a message and the pubkeys who added it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: String,
    pub users: Vec<String>,
}

/// A denormalized snapshot of the message a reply points at.
///
/// Messages live only in the DB and are fetched a page at a time, so an id
/// alone would leave a reply unable to draw its quoted line whenever the parent
/// isn't in the page the client happens to hold — replying to something from
/// yesterday would render as a dangling stub. Carrying the excerpt makes a
/// reply self-contained.
///
/// The server fills this in from its own row, never from what the client sent,
/// so a client can't put words in someone else's mouth by forging the quote.
/// It's a snapshot: editing or deleting the parent later doesn't rewrite it,
/// which matches what the replier was actually answering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyRef {
    pub message_id: Id,
    pub author_pubkey: String,
    pub author_username: String,
    /// Plain-text excerpt of the parent, truncated server-side.
    pub excerpt: String,
}

/// How much of the parent a reply quotes. Long enough to identify the message,
/// short enough that a reply to a wall of text stays one line.
pub const REPLY_EXCERPT_CHARS: usize = 120;

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
    /// Set when this message is a reply. Server-populated; see `ReplyRef`.
    #[serde(default)]
    pub reply_to: Option<ReplyRef>,
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
    /// True while this user is publishing a webcam into the `screen-{channel}`
    /// room for the channel they are in.
    ///
    /// A per-user voice flag, alongside the others, rather than a whole-set
    /// message shaped like `ScreenShareState`. That is not only tidier: `Ready`
    /// carries `voice_states` and carries *no* screen-share snapshot, so a
    /// client that connects while someone is already sharing learns nothing
    /// until that person next toggles it. Riding here is what makes a
    /// reconnecting client see cameras that were already on — and reconnects
    /// are designed for, since a slow consumer is dropped on purpose.
    ///
    /// It also inherits every teardown the other flags have: leaving, being
    /// disconnected, being kicked or banned, and channel deletion all clear the
    /// whole `VoiceState`.
    #[serde(default)]
    pub camera_on: bool,
    /// True while this user is sharing their screen in the channel they are in.
    ///
    /// The authoritative record — `ScreenShareState` is still sent alongside it,
    /// but derived from this rather than from a map of its own. Moving it here
    /// fixed three things at once: `Ready` carries `voice_states` and carried no
    /// screen-share snapshot, so a client connecting while someone was already
    /// sharing saw no LIVE badge until that person next toggled it; the old
    /// setter re-broadcast its whole sorted list even when nothing had changed;
    /// and its `entry().or_default()` left an empty `HashSet` per channel behind
    /// even on *stop*.
    #[serde(default)]
    pub screen_sharing: bool,
}

// ---------------------------------------------------------------------------
// Bot platform (Tier 1)
// ---------------------------------------------------------------------------

/// What an installed bot is allowed to *do* in a guild. Granted by the guild
/// owner at install time (Discord's model: the host decides, not the bot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Post messages to the guild's text channels.
    SendMessages,
    /// Fetch channel history (`FetchMessages`).
    ReadMessageHistory,
    /// Add/remove emoji reactions.
    AddReactions,
    /// Create/rename/delete channels, edit topics, toggle read-only.
    ManageChannels,
    /// Delete other people's messages; post in read-only channels.
    ManageMessages,
    /// Remove members from the guild (they may rejoin).
    KickMembers,
    /// Ban/unban members (bans block rejoining, even by invite).
    BanMembers,
    /// Create/edit/delete/assign roles — bounded by the grant-subset rule
    /// (you can never hand out authority you don't hold yourself).
    ManageRoles,
    /// Guild administration: accent, branding, visibility, integrations.
    ManageGuild,
    /// Mint/rotate the guild's invite code.
    CreateInvite,
    /// Add, rename and remove the guild's custom emoji.
    ManageEmojis,
}

impl Permission {
    /// Every permission, for the role editor.
    pub const ALL: &'static [Permission] = &[
        Permission::SendMessages,
        Permission::ReadMessageHistory,
        Permission::AddReactions,
        Permission::ManageChannels,
        Permission::ManageMessages,
        Permission::KickMembers,
        Permission::BanMembers,
        Permission::ManageRoles,
        Permission::ManageGuild,
        Permission::CreateInvite,
        Permission::ManageEmojis,
    ];

    /// The subset offered in the bot installer. Management permissions are
    /// human-only, with one exception: `ManageMessages` lets an announcement
    /// bot post into read-only channels (bots can never delete messages —
    /// `DeleteMessage` isn't on the bot action allowlist).
    pub const BOT_INSTALLABLE: &'static [Permission] = &[
        Permission::SendMessages,
        Permission::ReadMessageHistory,
        Permission::AddReactions,
        Permission::ManageMessages,
    ];

    /// Human-readable label for UI.
    pub fn label(self) -> &'static str {
        match self {
            Permission::SendMessages => "Send messages",
            Permission::ReadMessageHistory => "Read message history",
            Permission::AddReactions => "Add reactions",
            Permission::ManageChannels => "Manage channels",
            Permission::ManageMessages => "Manage messages",
            Permission::KickMembers => "Kick members",
            Permission::BanMembers => "Ban members",
            Permission::ManageRoles => "Manage roles",
            Permission::ManageGuild => "Manage guild",
            Permission::CreateInvite => "Create invites",
            Permission::ManageEmojis => "Manage emojis",
        }
    }
}

/// A named permission bundle a guild owner (or `ManageRoles` holder, within
/// the grant-subset rule) defines and assigns to members. A member's effective
/// permissions are the union over their roles; the owner implicitly holds
/// everything. There is no hierarchy — safety comes from the subset rule:
/// nobody can create, edit, or assign a role carrying permissions they don't
/// hold themselves, and roles carrying `ManageRoles`/`ManageGuild` are
/// owner-touch-only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Role {
    pub id: Id,
    pub guild_id: Id,
    /// 1..=32 chars.
    pub name: String,
    /// Optional CSS hex color used to tint member names.
    #[serde(default)]
    pub color: Option<String>,
    pub permissions: Vec<Permission>,
    /// Display seniority (lower renders first). Cosmetic — never authority.
    #[serde(default)]
    pub position: u32,
}

/// A guild's custom emoji, referenced in message text and reactions as
/// `:shortcode:` (NIP-30's notation and character set).
///
/// The image is NOT carried here — only its content address. Emoji render dozens
/// of times per screen, so inlining the bytes into every catalog push (or every
/// message) would be wasteful, and a plain URL wouldn't work either: a friend
/// arriving through the rendezvous proxy has a WebSocket to the gateway and no
/// HTTP route to it at all. So clients fetch bytes once via `FetchEmoji` and
/// cache them under `image` forever — safe because the name *is* the SHA-256 of
/// the bytes, so it can never go stale.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuildEmoji {
    pub id: Id,
    pub guild_id: Id,
    /// 2..=32 chars of `[a-z0-9_]`, unique within the guild. Validated by
    /// `valid_shortcode`; the server is the authority.
    pub shortcode: String,
    /// Content address of the image: `<sha256>.<ext>`.
    pub image: String,
    /// Pubkey of whoever uploaded it.
    #[serde(default)]
    pub added_by: String,
    #[serde(default)]
    pub created_ms: i64,
}

/// One emoji image, in reply to `FetchEmoji`. `data_url` is empty when the
/// server has no such blob (the client caches that as "don't ask again").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmojiBlob {
    pub image: String,
    pub data_url: String,
}

/// Longest a custom-emoji shortcode may be. Short enough to stay readable
/// inline in a sentence.
pub const MAX_SHORTCODE_LEN: usize = 32;
/// Custom emoji per guild. A ceiling matters here because there is no account
/// system to fall back on: anyone can self-host, so quotas are the anti-abuse
/// mechanism (same reasoning as slowmode and the join gates).
pub const MAX_EMOJIS_PER_GUILD: usize = 100;

/// Whether `s` is a legal emoji shortcode.
///
/// Deliberately narrower than NIP-30 (which also allows uppercase and hyphens):
/// lowercase-only makes `:Tada:` and `:tada:` the same emoji rather than two
/// that look identical in a list, and excluding `-` keeps the scanner in the
/// message renderer from having to guess about hyphenated words.
pub fn valid_shortcode(s: &str) -> bool {
    (2..=MAX_SHORTCODE_LEN).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// What event streams an installed bot *receives*. This is the data-minimization
/// boundary: a bot only gets the categories it was granted, and message text is
/// withheld unless the privileged `MessageContent` intent is granted (matching
/// Discord's privileged-intents design — by default a bot sees that a message
/// happened, not what it said).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    /// Receive `MessageCreate` events for the bot's installed guilds. Without
    /// `MessageContent`, the `content`/`image` fields are blanked.
    GuildMessages,
    /// PRIVILEGED: include the actual message text + image in delivered events.
    MessageContent,
    /// Receive `ReactionUpdate` events.
    Reactions,
    /// PRIVILEGED: receive `MemberJoin`/`MemberLeave` events for the guild.
    Members,
}

impl Intent {
    pub const ALL: &'static [Intent] = &[
        Intent::GuildMessages,
        Intent::MessageContent,
        Intent::Reactions,
        Intent::Members,
    ];

    /// Privileged intents expose sensitive data (message text, the full member
    /// roster) and should be surfaced distinctly in the install UI.
    pub fn is_privileged(self) -> bool {
        matches!(self, Intent::MessageContent | Intent::Members)
    }

    pub fn label(self) -> &'static str {
        match self {
            Intent::GuildMessages => "Message events (no content)",
            Intent::MessageContent => "Message content",
            Intent::Reactions => "Reaction events",
            Intent::Members => "Member join/leave events",
        }
    }
}

/// A bot installed into a guild, with the grants the owner gave it. Keyed by
/// `(guild_id, bot_pubkey)`. The bot connects as a normal signed identity; this
/// record is what elevates that identity to an installed application and bounds
/// what it can see and do.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotInstall {
    pub guild_id: Id,
    pub bot_pubkey: String,
    /// Display name chosen by the installer (the bot's own `username` on
    /// connect is cosmetic and may differ).
    pub name: String,
    pub permissions: Vec<Permission>,
    pub intents: Vec<Intent>,
}

impl BotInstall {
    pub fn has_permission(&self, p: Permission) -> bool {
        self.permissions.contains(&p)
    }

    pub fn has_intent(&self, i: Intent) -> bool {
        self.intents.contains(&i)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Sent in response to a `Hello { nonce }`. Signature is hex
    /// `schnorr(SHA256(nonce || pubkey || username))` (BIP-340).
    Identify {
        username: String,
        pubkey: String,
        signature: String,
        /// Self-declared application connection. Only connections that set
        /// this are bot-gated (scoped Ready, intent-filtered events, narrow
        /// action surface). A `BotInstall` against a pubkey never restricts a
        /// human connection — otherwise anyone could strip a victim's account
        /// by "installing" their pubkey in a throwaway guild.
        #[serde(default)]
        bot: bool,
    },
    FetchMessages {
        channel_id: Id,
        limit: u32,
        /// Cursor: only messages strictly older than this unix-ms timestamp
        /// (for scrolling further back). None = the newest page.
        #[serde(default)]
        before_ms: Option<i64>,
    },
    SendMessage {
        channel_id: Id,
        content: String,
        /// Optional inline image as a `data:image/...;base64,...` URL.
        #[serde(default)]
        image: Option<String>,
        /// The message being replied to. Only the id travels — the server looks
        /// the parent up in this same channel and builds the `ReplyRef` itself,
        /// so a reply can neither quote text that was never written nor point
        /// at a message in a channel the sender can't read. An id that doesn't
        /// resolve is dropped: the message still sends, just without a quote.
        #[serde(default)]
        reply_to: Option<Id>,
    },
    /// Create a new guild. The server seeds it with a default text + voice
    /// channel and makes the requesting user its first (and only) member.
    /// `template` picks a community preset ("friend" | "foss" | "community");
    /// None/unknown = the plain default.
    CreateGuild {
        name: String,
        #[serde(default)]
        template: Option<String>,
    },
    /// Join an existing guild from the directory. If the guild has a join
    /// gate, the server replies `JoinChallenge`; the client resends with
    /// `accept` (rules) or `pow_nonce` (proof-of-work). On success the server
    /// replies `GuildJoined`.
    JoinGuild {
        guild_id: Id,
        #[serde(default)]
        accept: bool,
        #[serde(default)]
        pow_nonce: Option<String>,
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
    /// Requires `ManageEmojis`: add a custom emoji to a guild. The image is a
    /// `data:image/...;base64,...` URL, same as message attachments — the
    /// server decodes it into the content-addressed blob store and replies with
    /// the refreshed catalog.
    CreateGuildEmoji {
        guild_id: Id,
        shortcode: String,
        image: String,
    },
    /// Requires `ManageEmojis`: rename an existing emoji. The image is
    /// untouched, so clients already holding the bytes keep them.
    RenameGuildEmoji {
        guild_id: Id,
        emoji_id: Id,
        shortcode: String,
    },
    /// Requires `ManageEmojis`: remove an emoji from the guild's catalog. The
    /// blob itself stays (it's content-addressed and may be shared) — see the
    /// blob-GC item in TODO.md.
    DeleteGuildEmoji {
        guild_id: Id,
        emoji_id: Id,
    },
    /// Fetch emoji images by content address. Sent when the client meets a
    /// shortcode whose bytes aren't in its cache yet; the server replies with
    /// `EmojiBlobs`.
    FetchEmoji {
        images: Vec<String>,
    },
    /// Requires `ManageGuild`: set (or clear) a guild's accent color.
    SetGuildAccent {
        guild_id: Id,
        #[serde(default)]
        accent: Option<String>,
    },
    /// Requires `ManageGuild`: install a bot into a guild (or update its
    /// grants if it's already installed). The bot is identified by its
    /// secp256k1 pubkey (64 hex chars).
    InstallBot {
        guild_id: Id,
        bot_pubkey: String,
        name: String,
        permissions: Vec<Permission>,
        intents: Vec<Intent>,
    },
    /// Requires `ManageGuild`: remove a bot from a guild.
    UninstallBot {
        guild_id: Id,
        bot_pubkey: String,
    },
    /// Requires `ManageGuild`: fetch a guild's bot installs (to render the
    /// Integrations panel). Replied to with `GuildIntegrations`.
    FetchIntegrations {
        guild_id: Id,
    },
    /// Requires `ManageRoles` (subset rule; `ManageRoles`/`ManageGuild` roles
    /// are owner-touch-only): create a role. Replied to with `GuildRoles`.
    CreateRole {
        guild_id: Id,
        name: String,
        #[serde(default)]
        color: Option<String>,
        permissions: Vec<Permission>,
    },
    /// Requires `ManageRoles` (same bounds): full-replace a role's
    /// name/color/permissions.
    UpdateRole {
        guild_id: Id,
        role_id: Id,
        name: String,
        #[serde(default)]
        color: Option<String>,
        permissions: Vec<Permission>,
    },
    /// Requires `ManageRoles` (same bounds): delete a role. The server strips
    /// it from every member and pushes `MemberUpdate`s.
    DeleteRole {
        guild_id: Id,
        role_id: Id,
    },
    /// Requires `ManageRoles` (same bounds): grant a role to a member.
    AssignRole {
        guild_id: Id,
        role_id: Id,
        user_pubkey: String,
    },
    /// Requires `ManageRoles` (same bounds): revoke a role from a member.
    UnassignRole {
        guild_id: Id,
        role_id: Id,
        user_pubkey: String,
    },
    /// Requires `ManageGuild`: toggle the guild between the public directory
    /// and invite-only.
    SetGuildVisibility {
        guild_id: Id,
        visibility: GuildVisibility,
    },
    /// Requires `CreateInvite` or `ManageGuild`: fetch the guild's invite code,
    /// minting one if absent. `rotate` replaces (invalidates) the current code.
    /// Replied to with `GuildInvite`.
    CreateInvite {
        guild_id: Id,
        #[serde(default)]
        rotate: bool,
    },
    /// Join a guild by invite code (works for private guilds). Gates apply
    /// exactly as for `JoinGuild` (reply may be `JoinChallenge`).
    JoinByInvite {
        code: String,
        #[serde(default)]
        accept: bool,
        #[serde(default)]
        pow_nonce: Option<String>,
    },
    /// Requires `KickMembers`: remove a member (they may rejoin). Rejected for
    /// the owner, yourself, bots (uninstall instead), and — unless you're the
    /// owner — anyone holding moderation permissions.
    KickMember {
        guild_id: Id,
        user_pubkey: String,
    },
    /// Requires `BanMembers`: kick + block rejoining (same target rules as
    /// kick). Works on non-members too (pre-ban).
    BanMember {
        guild_id: Id,
        user_pubkey: String,
    },
    /// Requires `BanMembers`: lift a ban.
    UnbanMember {
        guild_id: Id,
        user_pubkey: String,
    },
    /// Requires `BanMembers`: list the guild's bans. Replied to with `GuildBans`.
    FetchBans {
        guild_id: Id,
    },
    /// Voluntarily leave a guild. Rejected for system guilds (auto-rejoined on
    /// reconnect) and for the owner (transfer or delete instead).
    LeaveGuild {
        guild_id: Id,
    },
    /// Requires `ManageChannels`: add a channel to a guild.
    CreateChannel {
        guild_id: Id,
        name: String,
        kind: ChannelKind,
        #[serde(default)]
        topic: Option<String>,
    },
    /// Requires `ManageChannels`: full-replace a channel's
    /// name/topic/read_only/position.
    UpdateChannel {
        channel_id: Id,
        name: String,
        #[serde(default)]
        topic: Option<String>,
        #[serde(default)]
        read_only: bool,
        #[serde(default)]
        position: u32,
        #[serde(default)]
        slowmode_secs: u32,
    },
    /// Requires `ManageChannels`: delete a channel (a guild's last text
    /// channel can't be deleted).
    DeleteChannel {
        channel_id: Id,
    },
    /// Delete a message: authors always may; others need `ManageMessages`
    /// (DM messages: author-only).
    DeleteMessage {
        channel_id: Id,
        message_id: Id,
    },
    /// Owner-only: hand the guild to another (human) member.
    TransferOwnership {
        guild_id: Id,
        new_owner_pubkey: String,
    },
    /// Requires `ManageGuild`: full-replace the guild's description and
    /// icon/banner images (None clears).
    SetGuildProfile {
        guild_id: Id,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        icon_image: Option<String>,
        #[serde(default)]
        banner: Option<String>,
    },
    /// Requires `ManageGuild`: set (or clear, with None) the guild's message
    /// retention in days. Old messages are swept hourly.
    SetGuildRetention {
        guild_id: Id,
        #[serde(default)]
        days: Option<u32>,
    },
    /// Requires `ManageGuild`: configure the join gate (+ rules text for the
    /// `Rules` gate).
    SetJoinGate {
        guild_id: Id,
        gate: JoinGate,
        #[serde(default)]
        rules: Option<String>,
    },
    /// Requires `ManageGuild`: toggle anti-raid lockdown (joins rejected).
    SetPanicMode {
        guild_id: Id,
        on: bool,
    },
    /// Requires `ManageGuild`: fetch the guild's recent moderation audit log.
    /// Replied to with `AuditLog`.
    FetchAuditLog {
        guild_id: Id,
    },
    /// Request a page of the public guild directory (browse-and-join). Sent
    /// when the browse dialog opens instead of relying on a broadcast push.
    /// Replied to with `GuildCatalog` (requester only).
    FetchCatalog {
        #[serde(default)]
        offset: u32,
        /// 0 means "server default page size".
        #[serde(default)]
        limit: u32,
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
    /// Announce that the sender turned their webcam on or off.
    ///
    /// No `channel_id`, unlike `SetScreenShare`: the server reads it from the
    /// sender's own voice state, so a client cannot claim a camera in a channel
    /// it is not in. Ignored outright when the sender is not in voice — which
    /// is also what gates it on guild membership, since only `JoinVoice`
    /// creates a voice state and it checks.
    SetCamera {
        on: bool,
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
        /// Roles of the user's guilds only.
        #[serde(default)]
        roles: Vec<Role>,
        /// Custom-emoji catalogs of the user's guilds only. Images are fetched
        /// separately and on demand — see `GuildEmoji`.
        #[serde(default)]
        emojis: Vec<GuildEmoji>,
        /// True if this connection is a configured operator — i.e. it owns
        /// system guilds (the seeded Lobby) for permission purposes. Lets the
        /// client surface management controls there even though the guild has
        /// no `owner_pubkey`.
        #[serde(default)]
        operator: bool,
    },
    /// A guild's full custom-emoji catalog. Sent on any change (add, rename,
    /// delete) to that guild's members — the whole list, not a delta, because
    /// it is small and bounded by `MAX_EMOJIS_PER_GUILD`.
    GuildEmojis {
        guild_id: Id,
        emojis: Vec<GuildEmoji>,
    },
    /// Emoji images, in reply to `FetchEmoji`.
    EmojiBlobs {
        blobs: Vec<EmojiBlob>,
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
        /// The guild's roles, so the joiner can render badges/permissions.
        #[serde(default)]
        roles: Vec<Role>,
        /// The guild's custom-emoji catalog (images fetched on demand).
        #[serde(default)]
        emojis: Vec<GuildEmoji>,
    },
    /// A guild was deleted by its owner. Delivered to the (former) members so
    /// they drop it from local state.
    GuildDelete {
        guild_id: Id,
    },
    /// A page of the public guild directory — delivered on demand (reply to
    /// `FetchCatalog`), not broadcast. `offset` echoes the request so the
    /// client knows whether to replace (page 0) or append; `total` is the full
    /// public-guild count so the client can page. Removing the old
    /// broadcast-to-everyone kills the per-guild-change catalog storm.
    GuildCatalog {
        guilds: Vec<GuildSummary>,
        #[serde(default)]
        offset: u32,
        #[serde(default)]
        total: u32,
    },
    /// Reply to `OpenDm` for the requester: the DM channel plus its history.
    DmReady {
        channel_id: Id,
        other: User,
        messages: Vec<Message>,
    },
    /// A user published or changed their profile. Broadcast to everyone so
    /// avatars/bios stay current wherever that user is shown.
    ProfileUpdate(Profile),
    MemberJoin(Member),
    /// A member went offline (presence, NOT removal — see `MemberRemove`).
    MemberLeave {
        guild_id: Id,
        user_pubkey: String,
    },
    /// A member's role set changed. Clients upsert the row.
    MemberUpdate(Member),
    /// A member is gone from the guild (kicked, banned, left, or a bot was
    /// uninstalled). Clients drop the roster row entirely.
    MemberRemove {
        guild_id: Id,
        user_pubkey: String,
    },
    /// A guild's full role list (reply/broadcast on any role change; also
    /// carried by `Ready`/`GuildJoined`). Delivered to members.
    GuildRoles {
        guild_id: Id,
        roles: Vec<Role>,
    },
    /// The guild's current invite code (reply to `CreateInvite`; requester only).
    GuildInvite {
        guild_id: Id,
        code: String,
    },
    /// The guild's ban list (reply to `FetchBans`; requester only).
    GuildBans {
        guild_id: Id,
        users: Vec<User>,
    },
    /// A join attempt hit the guild's gate. The client satisfies it and
    /// resends the join with `accept` / `pow_nonce`.
    JoinChallenge {
        guild_id: Id,
        gate: JoinGate,
        /// Rules text (Rules gate).
        #[serde(default)]
        rules: Option<String>,
        /// Challenge string (Pow gate): find `nonce` such that
        /// SHA-256(challenge ++ nonce) has `pow_difficulty` leading zero BITS.
        #[serde(default)]
        pow_challenge: Option<String>,
        #[serde(default)]
        pow_difficulty: Option<u32>,
        /// Set when the join arrived via invite code, so the client retries
        /// on the same path.
        #[serde(default)]
        invite_code: Option<String>,
    },
    /// Recent moderation actions (reply to `FetchAuditLog`; requester only).
    AuditLog {
        guild_id: Id,
        entries: Vec<AuditEntry>,
    },
    /// A channel was added to a guild the recipient belongs to.
    ChannelCreate(Channel),
    /// A channel's name/topic/read_only/position changed.
    ChannelUpdate(Channel),
    /// A channel was deleted; clients drop it (and its messages) and reselect
    /// if it was open.
    ChannelDelete {
        guild_id: Id,
        channel_id: Id,
    },
    /// A message was deleted (by its author or a moderator).
    MessageDelete {
        channel_id: Id,
        message_id: Id,
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
    /// A guild's bot installs (reply to `FetchIntegrations`, and pushed to the
    /// owner whenever an install changes). Owner-only.
    GuildIntegrations {
        guild_id: Id,
        bots: Vec<BotInstall>,
    },
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
    /// channel (sent alongside `VoiceToken` on join). Used to publish and to
    /// render screen *video*.
    ///
    /// It is also the token the **webcam** publishes under. Camera video
    /// deliberately adds no fourth identity: this connection already holds
    /// `can_publish` under the bare pubkey, and LiveKit tells a camera track
    /// from a screen track by `TrackSource`, not by identity. That is why the
    /// camera needed no new token, no new grant and no change to
    /// `server::livekit` — do not "fix" it by minting one.
    ///
    /// The cost of sharing the identity is a mixed-version hazard the server
    /// cannot mitigate: a client older than the camera keys video tracks by
    /// identity alone, so it may render someone's webcam in the screen tile.
    /// Inherent to reusing the room, and the same property that made this cheap.
    ScreenToken {
        channel_id: Id,
        livekit_url: String,
        token: String,
        /// Second token for the same room, used by the *native* client to
        /// subscribe to screen-share **audio** only — so stream sound plays
        /// through the same cpal device as voice instead of the webview's.
        ///
        /// It carries a different LiveKit identity from `token` (same room,
        /// and LiveKit evicts duplicate identities). Empty from servers that
        /// predate it, in which case the client keeps playing stream audio in
        /// the webview.
        #[serde(default)]
        audio_token: String,
        /// Third token for the same room, used by the native client to *publish*
        /// screen **video** where the webview cannot capture it at all.
        ///
        /// WKWebView does not expose `navigator.mediaDevices`, so on macOS
        /// `getDisplayMedia` is not merely restricted — the API is absent, and
        /// the webview capture path in `features::screenshare` can never run.
        /// The native client captures via ScreenCaptureKit instead and needs its
        /// own identity to publish under, for the same reason `audio_token`
        /// does: one connection per identity per room.
        ///
        /// Empty from servers that predate it, in which case a client with no
        /// webview capture simply cannot share.
        #[serde(default)]
        video_token: String,
    },
    Error {
        message: String,
    },
}
