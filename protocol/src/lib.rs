use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod rendezvous;

pub type Id = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
    pub pubkey: String,
    pub username: String,
}

pub const MAX_USERNAME_LEN: usize = 32;

/// Must stay idempotent: the server canonicalizes before verifying the
/// signature, and the bot SDK signs the canonicalized form.
pub fn canonical_username(raw: &str) -> String {
    let truncated = truncate_username(raw.trim());
    let trimmed = truncated.trim();
    if trimmed.is_empty() {
        "anonymous".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Filtering before the cap, not after: otherwise a run of control characters
/// spends the whole budget and every name canonicalizes to `anonymous`.
pub fn truncate_username(raw: &str) -> String {
    raw.chars()
        .filter(|c| !unsafe_to_display(*c))
        .take(MAX_USERNAME_LEN)
        .collect()
}

/// U+2028/2029 are not `is_control` but a log line treats them as breaks; the
/// bidi overrides reorder the text around them, and natural RTL needs none.
pub fn unsafe_to_display(c: char) -> bool {
    c.is_control()
        || c == '\u{2028}'
        || c == '\u{2029}'
        || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `kind` only names the thing in the error, so the three callers keep the
/// wording they had.
pub fn sanitize_name(kind: &str, raw: &str, max_chars: usize) -> Result<String, String> {
    let filtered: String = raw.chars().filter(|c| !unsafe_to_display(*c)).collect();
    let name = filtered.trim();
    if name.is_empty() || name.chars().count() > max_chars {
        return Err(format!("{kind} name must be 1..={max_chars} chars"));
    }
    Ok(name.to_string())
}

/// Free text truncates where a name would be rejected: these fields are
/// optional and nobody is waiting on an error to fix them.
pub fn sanitize_line(raw: &str, max_chars: usize) -> String {
    raw.chars()
        .filter(|c| !unsafe_to_display(*c))
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

/// A bio, a description and a set of rules are written in a `textarea`, so the
/// newline is the one control character they are allowed to keep.
pub fn sanitize_paragraph(raw: &str, max_chars: usize) -> String {
    raw.chars()
        .filter(|c| *c == '\n' || !unsafe_to_display(*c))
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

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
    pub icon: Option<String>,
    #[serde(default)]
    pub owner_pubkey: String,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub visibility: GuildVisibility,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon_image: Option<String>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub retention_days: Option<u32>,
    #[serde(default)]
    pub join_gate: JoinGate,
    #[serde(default)]
    pub rules: Option<String>,
    #[serde(default)]
    pub panic_mode: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JoinGate {
    #[default]
    Open,
    Rules,
    Pow,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEntry {
    pub at_ms: i64,
    pub actor_pubkey: String,
    pub action: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub detail: String,
}

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
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub slowmode_secs: u32,
    #[serde(default)]
    pub position: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Member {
    pub user: User,
    pub guild_id: Id,
    pub online: bool,
    #[serde(default)]
    pub bot: bool,
    #[serde(default)]
    pub roles: Vec<Id>,
    #[serde(default)]
    pub xp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Profile {
    pub pubkey: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub banner: Option<String>,
    #[serde(default)]
    pub bio: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub custom_status: Option<String>,
}

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
        assert_eq!(level_progress(0), (1, 0, 10));
        assert_eq!(level_progress(9), (1, 9, 10));
        assert_eq!(level_progress(10), (2, 0, 20));
        assert_eq!(level_progress(29), (2, 19, 20));
        assert_eq!(level_progress(30), (3, 0, 30));
        let mut last = 0;
        for xp in 0..1000 {
            let l = level_progress(xp).0;
            assert!(l >= last);
            last = l;
        }
    }
}

#[cfg(test)]
mod username_tests {
    use super::{MAX_USERNAME_LEN, canonical_username, truncate_username, unsafe_to_display};

    #[test]
    fn canonicalising_is_idempotent() {
        for raw in [
            "alice",
            "  bob  ",
            "",
            "   ",
            &"a".repeat(33),
            &"a".repeat(200),
            &format!("{} b", "a".repeat(31)),
            &format!("{}   tail", "x".repeat(30)),
            "🙂🙂🙂",
            &"🙂".repeat(40),
        ] {
            let once = canonical_username(raw);
            let twice = canonical_username(&once);
            assert_eq!(once, twice, "not idempotent for {raw:?}");
        }
    }

    #[test]
    fn canonical_output_is_always_wire_legal() {
        for raw in [
            "",
            "   ",
            "alice",
            &"a".repeat(99),
            &"🙂".repeat(99),
            "a\u{0}\u{2028}b",
            "a\u{202E}b\u{2069}c",
        ] {
            let out = canonical_username(raw);
            assert!(!out.is_empty(), "never empty (would be unnamed on screen)");
            assert_eq!(out.trim(), out, "no surrounding whitespace survives");
            assert!(
                out.chars().count() <= MAX_USERNAME_LEN,
                "counted in chars, not bytes — {out:?}"
            );
            assert!(
                !out.chars().any(unsafe_to_display),
                "a break or a reordering survived: {out:?}"
            );
        }
    }

    #[test]
    fn a_name_cannot_forge_a_log_line() {
        let forged = "alice\nINFO identified user=admin pubkey=deadbeef";
        let clean = canonical_username(forged);
        assert!(!clean.contains('\n'), "newline survived: {clean:?}");
        assert!(!clean.contains('\r'));
        assert!(!clean.contains('\u{0}'));
        assert!(clean.starts_with("alice"));

        for sep in ['\u{2028}', '\u{2029}'] {
            let forged = format!("alice{sep}INFO identified user=admin");
            let clean = canonical_username(&forged);
            assert!(
                !clean.contains(sep),
                "U+{:04X} survived: {clean:?}",
                sep as u32
            );
        }
    }

    #[test]
    fn control_characters_do_not_consume_the_cap() {
        let padded = format!("{}alice", "\u{0}".repeat(MAX_USERNAME_LEN * 2));
        assert_eq!(canonical_username(&padded), "alice");

        let interior = format!("al{}ice", "\u{7}".repeat(MAX_USERNAME_LEN * 2));
        assert_eq!(canonical_username(&interior), "alice");
    }

    #[test]
    fn filtering_does_not_leave_exposed_padding() {
        assert_eq!(canonical_username("\u{0} alice \u{0}"), "alice");
    }

    #[test]
    fn a_name_cannot_reorder_the_text_around_it() {
        for c in ['\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}'] {
            let out = canonical_username(&format!("alice{c}bob"));
            assert_eq!(out, "alicebob", "U+{:04X} survived", c as u32);
        }
        for c in ['\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}'] {
            let out = canonical_username(&format!("alice{c}bob"));
            assert_eq!(out, "alicebob", "U+{:04X} survived", c as u32);
        }
    }

    #[test]
    fn right_to_left_names_are_left_alone() {
        for raw in ["مرحبا", "שלום", "Ali ali", "علي"] {
            assert_eq!(
                canonical_username(raw),
                raw,
                "natural RTL needs no override and must survive whole"
            );
        }
    }

    #[test]
    fn a_name_that_needs_nothing_is_left_alone() {
        assert_eq!(canonical_username("alice"), "alice");
        assert_eq!(canonical_username(&"b".repeat(32)), "b".repeat(32));
    }

    #[test]
    fn an_empty_name_becomes_anonymous_rather_than_nothing() {
        assert_eq!(canonical_username("   "), "anonymous");
    }

    #[test]
    fn multibyte_names_are_cut_by_character() {
        let out = canonical_username(&"🙂".repeat(40));
        assert_eq!(out.chars().count(), MAX_USERNAME_LEN);
    }

    #[test]
    fn truncation_counts_characters_where_maxlength_counts_code_units() {
        let out = truncate_username(&"😀".repeat(40));
        assert_eq!(out.chars().count(), MAX_USERNAME_LEN);
        assert_eq!(out.encode_utf16().count(), MAX_USERNAME_LEN * 2);
    }

    #[test]
    fn truncation_leaves_whitespace_alone() {
        assert_eq!(truncate_username("john "), "john ");
        assert_eq!(truncate_username("  "), "  ");
    }

    #[test]
    fn a_truncated_name_is_not_cut_a_second_time_at_signing() {
        for raw in ["alice", &"a".repeat(60), &"😀".repeat(60), "ünïcøde"] {
            let typed = truncate_username(raw);
            assert_eq!(canonical_username(&typed), typed.trim());
        }
    }
}

#[cfg(test)]
mod name_tests {
    use super::sanitize_name;

    #[test]
    fn the_three_callers_keep_their_wording() {
        assert_eq!(
            sanitize_name("guild", "", 64).unwrap_err(),
            "guild name must be 1..=64 chars"
        );
        assert_eq!(
            sanitize_name("channel", "", 64).unwrap_err(),
            "channel name must be 1..=64 chars"
        );
        assert_eq!(
            sanitize_name("role", "", 32).unwrap_err(),
            "role name must be 1..=32 chars"
        );
    }

    /// Naming the character rather than asking `unsafe_to_display` again: an
    /// assertion built from the predicate under test cannot fail with it.
    #[test]
    fn a_name_cannot_forge_a_log_line_or_reorder_one() {
        for bad in [
            '\n', '\r', '\u{0}', '\u{2028}', '\u{2029}', '\u{202A}', '\u{202B}', '\u{202C}',
            '\u{202D}', '\u{202E}', '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
        ] {
            let raw = format!("gen{bad}eral");
            let out = sanitize_name("channel", &raw, 64).unwrap();
            assert_eq!(out, "general", "U+{:04X} survived", bad as u32);
        }
    }

    #[test]
    fn stripping_can_empty_a_name_and_that_is_a_rejection() {
        assert!(sanitize_name("channel", "\u{202E}\u{0}\n", 64).is_err());
    }

    #[test]
    fn filtering_happens_before_the_cap_is_judged() {
        let padded = format!("{}general", "\u{0}".repeat(200));
        assert_eq!(sanitize_name("channel", &padded, 64).unwrap(), "general");
    }

    #[test]
    fn an_ordinary_name_is_only_trimmed() {
        assert_eq!(
            sanitize_name("channel", "  general  ", 64).unwrap(),
            "general"
        );
        assert_eq!(sanitize_name("role", "مشرف", 32).unwrap(), "مشرف");
    }

    #[test]
    fn the_cap_still_counts_characters() {
        assert!(sanitize_name("role", &"🙂".repeat(32), 32).is_ok());
        assert!(sanitize_name("role", &"🙂".repeat(33), 32).is_err());
    }
}

#[cfg(test)]
mod free_text_tests {
    use super::{sanitize_line, sanitize_paragraph};

    /// The whole point of the split: a bio written in a `textarea` keeps the
    /// breaks the person typed, and a one-line field keeps none.
    #[test]
    fn a_paragraph_keeps_its_newlines_and_a_line_does_not() {
        assert_eq!(
            sanitize_paragraph("first\nsecond\nthird", 280),
            "first\nsecond\nthird"
        );
        assert_eq!(sanitize_line("first\nsecond", 80), "firstsecond");
    }

    #[test]
    fn a_paragraph_keeps_nothing_else_that_moves_the_cursor() {
        for bad in [
            '\r', '\u{0}', '\u{7}', '\u{2028}', '\u{2029}', '\u{202A}', '\u{202E}', '\u{2066}',
            '\u{2069}',
        ] {
            let out = sanitize_paragraph(&format!("ab{bad}cd"), 280);
            assert_eq!(out, "abcd", "U+{:04X} survived a paragraph", bad as u32);
        }
    }

    #[test]
    fn a_line_keeps_nothing_that_moves_the_cursor() {
        for bad in ['\n', '\r', '\u{0}', '\u{2028}', '\u{202E}', '\u{2069}'] {
            let out = sanitize_line(&format!("ab{bad}cd"), 80);
            assert_eq!(out, "abcd", "U+{:04X} survived a line", bad as u32);
        }
    }

    /// Free text truncates where a name is rejected — these are optional and
    /// nobody is waiting on an error.
    #[test]
    fn over_long_free_text_is_cut_not_refused() {
        assert_eq!(sanitize_line(&"a".repeat(500), 80).chars().count(), 80);
        assert_eq!(
            sanitize_paragraph(&"a".repeat(500), 280).chars().count(),
            280
        );
    }

    #[test]
    fn windows_line_endings_do_not_leave_a_stray_carriage_return() {
        assert_eq!(sanitize_paragraph("one\r\ntwo", 280), "one\ntwo");
    }

    #[test]
    fn surrounding_whitespace_goes_but_the_inside_is_left_alone() {
        assert_eq!(sanitize_paragraph("  a\n\n  b  ", 280), "a\n\n  b");
        assert_eq!(sanitize_line("  hello  ", 80), "hello");
    }

    #[test]
    fn text_that_was_only_junk_comes_back_empty() {
        assert_eq!(sanitize_line("\u{202E}\u{0}\n", 80), "");
        assert_eq!(sanitize_paragraph("\u{202E}\u{0}\r", 280), "");
    }

    #[test]
    fn right_to_left_text_is_left_alone() {
        assert_eq!(sanitize_paragraph("مرحبا\nبالعالم", 280), "مرحبا\nبالعالم");
        assert_eq!(sanitize_line("שלום", 80), "שלום");
    }
}

#[cfg(test)]
mod camera_wire_tests {
    use super::{ClientMessage, VoiceState};

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

    #[test]
    fn set_camera_round_trips_over_the_wire() {
        let json = serde_json::to_string(&ClientMessage::SetCamera { on: true }).unwrap();
        assert_eq!(json, r#"{"op":"set_camera","d":{"on":true}}"#);
        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ClientMessage::SetCamera { on: true }));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Reaction {
    pub emoji: String,
    pub users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyRef {
    pub message_id: Id,
    pub author_pubkey: String,
    pub author_username: String,
    pub excerpt: String,
}

pub const REPLY_EXCERPT_CHARS: usize = 120;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Message {
    pub id: Id,
    pub channel_id: Id,
    pub author: User,
    pub content: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    /// Filled by the server from its own row, never from what the client sent.
    #[serde(default)]
    pub reply_to: Option<ReplyRef>,
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
    #[serde(default)]
    pub camera_on: bool,
    #[serde(default)]
    pub screen_sharing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    SendMessages,
    ReadMessageHistory,
    AddReactions,
    ManageChannels,
    ManageMessages,
    KickMembers,
    BanMembers,
    ManageRoles,
    ManageGuild,
    CreateInvite,
    ManageEmojis,
}

impl Permission {
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

    pub const BOT_INSTALLABLE: &'static [Permission] = &[
        Permission::SendMessages,
        Permission::ReadMessageHistory,
        Permission::AddReactions,
        Permission::ManageMessages,
    ];

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Role {
    pub id: Id,
    pub guild_id: Id,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
    pub permissions: Vec<Permission>,
    #[serde(default)]
    pub position: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GuildEmoji {
    pub id: Id,
    pub guild_id: Id,
    pub shortcode: String,
    pub image: String,
    #[serde(default)]
    pub added_by: String,
    #[serde(default)]
    pub created_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmojiBlob {
    pub image: String,
    pub data_url: String,
}

pub const MAX_SHORTCODE_LEN: usize = 32;
pub const MAX_EMOJIS_PER_GUILD: usize = 100;

/// Narrower than NIP-30 on purpose: lowercase-only makes `:Tada:` and `:tada:`
/// one emoji rather than two.
pub fn valid_shortcode(s: &str) -> bool {
    (2..=MAX_SHORTCODE_LEN).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    GuildMessages,
    MessageContent,
    Reactions,
    Members,
}

impl Intent {
    pub const ALL: &'static [Intent] = &[
        Intent::GuildMessages,
        Intent::MessageContent,
        Intent::Reactions,
        Intent::Members,
    ];

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BotInstall {
    pub guild_id: Id,
    pub bot_pubkey: String,
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
    Identify {
        username: String,
        pubkey: String,
        signature: String,
        /// Outside the signature on purpose — inferring bot-ness from installs
        /// would let anyone strip a victim's account of human privileges.
        #[serde(default)]
        bot: bool,
        /// Attacker-chosen, so the server trims and strips it and gates nothing
        /// on it. It exists to be counted in a log.
        #[serde(default)]
        client_version: String,
    },
    FetchMessages {
        channel_id: Id,
        limit: u32,
        #[serde(default)]
        before_ms: Option<i64>,
    },
    SendMessage {
        channel_id: Id,
        content: String,
        #[serde(default)]
        image: Option<String>,
        #[serde(default)]
        reply_to: Option<Id>,
    },
    CreateGuild {
        name: String,
        #[serde(default)]
        template: Option<String>,
    },
    JoinGuild {
        guild_id: Id,
        #[serde(default)]
        accept: bool,
        #[serde(default)]
        pow_nonce: Option<String>,
    },
    DeleteGuild {
        guild_id: Id,
    },
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
    React {
        channel_id: Id,
        message_id: Id,
        emoji: String,
    },
    Typing {
        channel_id: Id,
    },
    CreateGuildEmoji {
        guild_id: Id,
        shortcode: String,
        image: String,
    },
    RenameGuildEmoji {
        guild_id: Id,
        emoji_id: Id,
        shortcode: String,
    },
    DeleteGuildEmoji {
        guild_id: Id,
        emoji_id: Id,
    },
    FetchEmoji {
        images: Vec<String>,
    },
    SetGuildAccent {
        guild_id: Id,
        #[serde(default)]
        accent: Option<String>,
    },
    InstallBot {
        guild_id: Id,
        bot_pubkey: String,
        name: String,
        permissions: Vec<Permission>,
        intents: Vec<Intent>,
    },
    UninstallBot {
        guild_id: Id,
        bot_pubkey: String,
    },
    FetchIntegrations {
        guild_id: Id,
    },
    CreateRole {
        guild_id: Id,
        name: String,
        #[serde(default)]
        color: Option<String>,
        permissions: Vec<Permission>,
    },
    UpdateRole {
        guild_id: Id,
        role_id: Id,
        name: String,
        #[serde(default)]
        color: Option<String>,
        permissions: Vec<Permission>,
    },
    DeleteRole {
        guild_id: Id,
        role_id: Id,
    },
    AssignRole {
        guild_id: Id,
        role_id: Id,
        user_pubkey: String,
    },
    UnassignRole {
        guild_id: Id,
        role_id: Id,
        user_pubkey: String,
    },
    SetGuildVisibility {
        guild_id: Id,
        visibility: GuildVisibility,
    },
    CreateInvite {
        guild_id: Id,
        #[serde(default)]
        rotate: bool,
        #[serde(default)]
        expires_in_secs: Option<u64>,
        #[serde(default)]
        max_uses: Option<u32>,
    },
    UpdateUsername {
        username: String,
    },
    /// Positions for the whole guild in one frame, never a delta: channels
    /// default to 0, so a never-reordered guild renumbers entirely.
    ReorderChannels {
        guild_id: Id,
        positions: Vec<(Id, u32)>,
    },
    JoinByInvite {
        code: String,
        #[serde(default)]
        accept: bool,
        #[serde(default)]
        pow_nonce: Option<String>,
    },
    KickMember {
        guild_id: Id,
        user_pubkey: String,
    },
    BanMember {
        guild_id: Id,
        user_pubkey: String,
    },
    UnbanMember {
        guild_id: Id,
        user_pubkey: String,
    },
    FetchBans {
        guild_id: Id,
    },
    LeaveGuild {
        guild_id: Id,
    },
    CreateChannel {
        guild_id: Id,
        name: String,
        kind: ChannelKind,
        #[serde(default)]
        topic: Option<String>,
    },
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
    DeleteChannel {
        channel_id: Id,
    },
    DeleteMessage {
        channel_id: Id,
        message_id: Id,
    },
    TransferOwnership {
        guild_id: Id,
        new_owner_pubkey: String,
    },
    SetGuildProfile {
        guild_id: Id,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        icon_image: Option<String>,
        #[serde(default)]
        banner: Option<String>,
    },
    SetGuildRetention {
        guild_id: Id,
        #[serde(default)]
        days: Option<u32>,
    },
    SetJoinGate {
        guild_id: Id,
        gate: JoinGate,
        #[serde(default)]
        rules: Option<String>,
    },
    SetPanicMode {
        guild_id: Id,
        on: bool,
    },
    FetchAuditLog {
        guild_id: Id,
    },
    FetchCatalog {
        #[serde(default)]
        offset: u32,
        #[serde(default)]
        limit: u32,
    },
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
    ShareMediaKey {
        channel_id: Id,
        to: String,
        epoch: u32,
        blob: String,
    },
    /// Publishes on the webview's existing screen-room identity. Do not "fix"
    /// this by minting a `#camera` one — see trap 10 in `CLAUDE.md`.
    SetCamera {
        on: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum ServerMessage {
    Hello {
        nonce: String,
    },
    Ready {
        user: User,
        guilds: Vec<Guild>,
        channels: Vec<Channel>,
        members: Vec<Member>,
        voice_states: Vec<VoiceState>,
        #[serde(default)]
        catalog: Vec<GuildSummary>,
        #[serde(default)]
        profiles: Vec<Profile>,
        #[serde(default)]
        roles: Vec<Role>,
        #[serde(default)]
        emojis: Vec<GuildEmoji>,
        #[serde(default)]
        operator: bool,
    },
    GuildEmojis {
        guild_id: Id,
        emojis: Vec<GuildEmoji>,
    },
    EmojiBlobs {
        blobs: Vec<EmojiBlob>,
    },
    MessageHistory {
        channel_id: Id,
        messages: Vec<Message>,
    },
    MessageCreate(Message),
    GuildJoined {
        guild: Guild,
        channels: Vec<Channel>,
        members: Vec<Member>,
        #[serde(default)]
        roles: Vec<Role>,
        #[serde(default)]
        emojis: Vec<GuildEmoji>,
        #[serde(default)]
        voice_states: Vec<VoiceState>,
    },
    GuildDelete {
        guild_id: Id,
    },
    GuildCatalog {
        guilds: Vec<GuildSummary>,
        #[serde(default)]
        offset: u32,
        #[serde(default)]
        total: u32,
    },
    ProfileUpdate(Profile),
    MemberJoin(Member),
    MemberLeave {
        guild_id: Id,
        user_pubkey: String,
    },
    MemberUpdate(Member),
    MemberRemove {
        guild_id: Id,
        user_pubkey: String,
    },
    GuildRoles {
        guild_id: Id,
        roles: Vec<Role>,
    },
    GuildInvite {
        guild_id: Id,
        code: String,
        #[serde(default)]
        expires_at_ms: Option<i64>,
        #[serde(default)]
        max_uses: Option<u32>,
        #[serde(default)]
        uses: u32,
    },
    GuildBans {
        guild_id: Id,
        users: Vec<User>,
    },
    JoinChallenge {
        guild_id: Id,
        gate: JoinGate,
        #[serde(default)]
        rules: Option<String>,
        #[serde(default)]
        pow_challenge: Option<String>,
        #[serde(default)]
        pow_difficulty: Option<u32>,
        #[serde(default)]
        invite_code: Option<String>,
    },
    AuditLog {
        guild_id: Id,
        entries: Vec<AuditEntry>,
    },
    ChannelCreate(Channel),
    ChannelUpdate(Channel),
    ChannelDelete {
        guild_id: Id,
        channel_id: Id,
    },
    MessageDelete {
        channel_id: Id,
        message_id: Id,
    },
    ReactionUpdate {
        channel_id: Id,
        message_id: Id,
        reactions: Vec<Reaction>,
    },
    TypingUpdate {
        channel_id: Id,
        user_pubkey: String,
        username: String,
    },
    GuildUpdate(Guild),
    GuildIntegrations {
        guild_id: Id,
        bots: Vec<BotInstall>,
    },
    ScreenShareState {
        channel_id: Id,
        sharers: Vec<String>,
    },
    VoiceStateUpdate(VoiceState),
    MediaKey {
        channel_id: Id,
        from: String,
        epoch: u32,
        blob: String,
    },
    VoiceToken {
        channel_id: Id,
        livekit_url: String,
        token: String,
    },
    ScreenToken {
        channel_id: Id,
        livekit_url: String,
        token: String,
        #[serde(default)]
        audio_token: String,
        #[serde(default)]
        video_token: String,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod identify_wire_tests {
    use super::ClientMessage;

    #[test]
    fn an_identify_without_a_version_still_parses() {
        let old = r#"{
            "op": "identify",
            "d": {
                "username": "alice",
                "pubkey": "ab",
                "signature": "cd",
                "bot": false
            }
        }"#;
        let msg: ClientMessage =
            serde_json::from_str(old).expect("an older client's handshake still parses");
        match msg {
            ClientMessage::Identify {
                username,
                client_version,
                ..
            } => {
                assert_eq!(username, "alice", "the fields that were there survive");
                assert!(
                    client_version.is_empty(),
                    "an absent version is empty — 'it did not say', not a version"
                );
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn a_version_survives_the_round_trip() {
        let json = serde_json::to_string(&ClientMessage::Identify {
            username: "alice".into(),
            pubkey: "ab".into(),
            signature: "cd".into(),
            bot: false,
            client_version: "v0.1.0-pre.223".into(),
        })
        .unwrap();
        assert!(json.contains("v0.1.0-pre.223"), "not on the wire: {json}");

        let back: ClientMessage = serde_json::from_str(&json).unwrap();
        match back {
            ClientMessage::Identify { client_version, .. } => {
                assert_eq!(client_version, "v0.1.0-pre.223")
            }
            other => panic!("parsed as {other:?}"),
        }
    }
}
