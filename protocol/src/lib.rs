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

/// The address the client dialed is inside the login signature, so a nonce one
/// server hands out cannot be signed for it by a client that dialed another.
pub const IDENTIFY_DOMAIN: &str = "discordia-identify-v2";

pub fn identify_payload(nonce: &str, origin: &str, pubkey: &str, username: &str) -> Vec<u8> {
    format!("{IDENTIFY_DOMAIN}\n{nonce}\n{origin}\n{pubkey}\n{username}").into_bytes()
}

/// `host:port` with the host lowercased and the port made explicit, so every
/// spelling of one gateway URL yields the string the server listed for itself.
pub fn dial_origin(url: &str) -> Option<String> {
    let url = url::Url::parse(url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let port = url.port_or_known_default()?;
    Some(format!("{host}:{port}"))
}

/// A bare `host` or `host:port` as an operator or a router would write it.
pub fn host_origin(raw: &str, default_port: u16) -> Option<String> {
    let raw = raw.trim();
    if raw.contains("://") {
        return dial_origin(raw);
    }
    let url = url::Url::parse(&format!("ws://{raw}")).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    Some(format!("{host}:{}", url.port().unwrap_or(default_port)))
}

pub fn quic_origin(endpoint_id: &str) -> String {
    format!("quic:{}", endpoint_id.trim().to_ascii_lowercase())
}

pub const QUIC_SCHEME: &str = "quic://";

/// What a host hands a friend: the key the connection is authenticated by, and
/// where to try it. An address may also be a relay URL for hole punching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuicShare {
    pub key: String,
    pub addrs: Vec<String>,
}

pub fn format_quic_share(key: &str, addrs: &[String]) -> String {
    format!("{QUIC_SCHEME}{key}@{}", addrs.join(";"))
}

pub fn parse_quic_share(raw: &str) -> Option<QuicShare> {
    let trimmed = raw.trim();
    let (scheme, rest) = trimmed.split_at_checked(QUIC_SCHEME.len())?;
    if !scheme.eq_ignore_ascii_case(QUIC_SCHEME) {
        return None;
    }
    let (key, addrs) = rest.split_once('@').unwrap_or((rest, ""));
    let key = key.trim().to_ascii_lowercase();
    if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let addrs = addrs
        .split(';')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .collect();
    Some(QuicShare { key, addrs })
}

/// An address on the far side of no NAT a friend cannot already see past.
pub fn is_private_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
        }
        std::net::IpAddr::V6(v6) => {
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
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
    cap(raw.chars().filter(|c| !unsafe_to_display(*c)), max_chars)
}

/// A bio, a description and a set of rules are written in a `textarea`, so the
/// newline is the one control character they are allowed to keep.
pub fn sanitize_paragraph(raw: &str, max_chars: usize) -> String {
    cap(
        raw.chars().filter(|c| *c == '\n' || !unsafe_to_display(*c)),
        max_chars,
    )
}

/// Trimmed before the cap and not only after, or a leading blank line long
/// enough spends the budget the real text needed and the field comes back empty.
fn cap(kept: impl Iterator<Item = char>, max_chars: usize) -> String {
    kept.collect::<String>()
        .trim()
        .chars()
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
    #[serde(default)]
    pub leveling: Leveling,
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

/// A named rank, worn from `xp` until the next tier begins. Guild managers
/// write these; an empty list means levels are drawn as plain `Lv N`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LevelTier {
    pub xp: u64,
    pub name: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemberSort {
    /// Alphabetical inside each presence group. What every guild had before
    /// this was configurable, so it stays the default.
    #[default]
    Name,
    /// Most experienced first, ties broken alphabetically.
    Level,
}

/// How a guild turns activity into experience, and what it calls the result.
///
/// Defaults reproduce the behaviour every guild had before any of it was
/// settable: a point per message, one a minute, everywhere, unnamed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Leveling {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "one")]
    pub per_message: u32,
    #[serde(default)]
    pub per_reaction: u32,
    #[serde(default)]
    pub per_voice_minute: u32,
    /// Seconds between two awards for the same person. Voice is exempt: its
    /// tick is a minute wide already, so a cooldown could only cancel it.
    #[serde(default = "sixty")]
    pub cooldown_secs: u32,
    /// Empty means every channel earns. Otherwise only these do.
    #[serde(default)]
    pub channels: Vec<Id>,
    /// Ascending by `xp`. `sanitize_leveling` is what guarantees that, so
    /// `tier_at` may assume it.
    #[serde(default)]
    pub tiers: Vec<LevelTier>,
    #[serde(default)]
    pub member_sort: MemberSort,
}

fn yes() -> bool {
    true
}
fn one() -> u32 {
    1
}
fn sixty() -> u32 {
    60
}

impl Default for Leveling {
    fn default() -> Self {
        Self {
            enabled: true,
            per_message: 1,
            per_reaction: 0,
            per_voice_minute: 0,
            cooldown_secs: 60,
            channels: Vec::new(),
            tiers: Vec::new(),
            member_sort: MemberSort::Name,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XpAction {
    Message,
    Reaction,
    VoiceMinute,
}

pub const MAX_TIERS: usize = 20;
pub const MAX_TIER_NAME: usize = 24;
/// A hundred a message is already absurd; the cap only stops a typo turning
/// one message into a number the level curve walks for a very long time.
pub const MAX_XP_PER_ACTION: u32 = 100;
pub const MAX_XP_COOLDOWN: u32 = 3600;

impl Leveling {
    pub fn amount_for(&self, action: XpAction) -> u32 {
        match action {
            XpAction::Message => self.per_message,
            XpAction::Reaction => self.per_reaction,
            XpAction::VoiceMinute => self.per_voice_minute,
        }
    }

    /// An empty allowlist earns everywhere; a non-empty one earns nowhere else.
    pub fn channel_earns(&self, channel_id: Id) -> bool {
        self.channels.is_empty() || self.channels.contains(&channel_id)
    }

    /// The highest tier reached at `xp`. Assumes the ascending order that
    /// `sanitize_leveling` imposes.
    pub fn tier_at(&self, xp: u64) -> Option<&LevelTier> {
        self.tiers.iter().rev().find(|t| xp >= t.xp)
    }

    /// What to draw beside a name: the tier if one is reached, else `Lv N`.
    pub fn label_at(&self, xp: u64) -> String {
        match self.tier_at(xp) {
            Some(tier) => tier.name.clone(),
            None => format!("Lv{}", level_progress(xp).0),
        }
    }
}

/// The gateway's filter, and the same one a row coming back from disk gets.
/// Sorting here rather than at every read is what lets `tier_at` walk backwards.
pub fn sanitize_leveling(mut raw: Leveling) -> Leveling {
    raw.per_message = raw.per_message.min(MAX_XP_PER_ACTION);
    raw.per_reaction = raw.per_reaction.min(MAX_XP_PER_ACTION);
    raw.per_voice_minute = raw.per_voice_minute.min(MAX_XP_PER_ACTION);
    raw.cooldown_secs = raw.cooldown_secs.min(MAX_XP_COOLDOWN);
    raw.channels.sort_unstable();
    raw.channels.dedup();

    let mut tiers: Vec<LevelTier> = raw
        .tiers
        .into_iter()
        .filter_map(|t| {
            let name = sanitize_line(&t.name, MAX_TIER_NAME);
            (!name.is_empty()).then_some(LevelTier {
                xp: t.xp,
                name,
                color: t.color.filter(|c| is_hex_color(c)),
            })
        })
        .collect();
    tiers.sort_by_key(|t| t.xp);
    // One rank per threshold: two names at the same number is a tie nothing
    // downstream can break, and the later one silently never showed.
    tiers.dedup_by_key(|t| t.xp);
    tiers.truncate(MAX_TIERS);
    raw.tiers = tiers;
    raw
}

/// `#rgb` or `#rrggbb`. The one definition: a role colour, a guild accent and a
/// tier colour are all drawn by the same CSS and must all mean the same thing.
pub fn is_hex_color(s: &str) -> bool {
    let s = s.trim();
    let Some(hex) = s.strip_prefix('#') else {
        return false;
    };
    (hex.len() == 3 || hex.len() == 6) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// What someone is doing right now. Never persisted and never written to the
/// database: it lives in server memory for exactly as long as the socket does.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Activity {
    #[serde(default)]
    pub kind: ActivityKind,
    pub name: String,
    #[serde(default)]
    pub details: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub started_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    #[default]
    Playing,
    Listening,
    Watching,
    Competing,
}

impl ActivityKind {
    pub fn verb(&self) -> &'static str {
        match self {
            ActivityKind::Playing => "Playing",
            ActivityKind::Listening => "Listening to",
            ActivityKind::Watching => "Watching",
            ActivityKind::Competing => "Competing in",
        }
    }
}

/// An activity with its owner. `None` is how a stop is spelled, so the wire
/// carries a clear as explicitly as it carries a start.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserActivity {
    pub pubkey: String,
    #[serde(default)]
    pub activity: Option<Activity>,
}

pub const MAX_ACTIVITY_NAME: usize = 128;
pub const MAX_ACTIVITY_TEXT: usize = 128;

/// The gateway's filter for one, mirroring what `sanitize_line` does for every
/// other free-text field. An unnameable activity is dropped, not renamed.
pub fn sanitize_activity(raw: Activity) -> Option<Activity> {
    let name = sanitize_line(&raw.name, MAX_ACTIVITY_NAME);
    if name.is_empty() {
        return None;
    }
    let text = |v: Option<String>| {
        v.map(|t| sanitize_line(&t, MAX_ACTIVITY_TEXT))
            .filter(|t| !t.is_empty())
    };
    Some(Activity {
        kind: raw.kind,
        name,
        details: text(raw.details),
        state: text(raw.state),
        started_ms: raw.started_ms.filter(|ms| *ms > 0),
    })
}

/// The presences a client can draw; the server refuses anything else so no
/// member can wear a status nobody's screen knows how to show.
pub const PRESENCES: [&str; 4] = ["online", "away", "dnd", "offline"];

/// An x-only BIP-340 key as every pubkey field carries it.
pub fn is_pubkey_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
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

    /// Reported on the PR: `set_guild_profile` trimmed before it capped, so a
    /// description behind a long blank run survived. Capping first dropped it.
    #[test]
    fn a_leading_blank_run_does_not_eat_the_budget() {
        let padded = format!("{}Real text", " ".repeat(300));
        assert_eq!(sanitize_paragraph(&padded, 280), "Real text");
        assert_eq!(sanitize_line(&padded, 80), "Real text");

        let blank_lines = format!("{}Real text", "\n".repeat(300));
        assert_eq!(sanitize_paragraph(&blank_lines, 280), "Real text");
    }

    /// The trailing trim is not redundant with the leading one: the cap can cut
    /// mid-whitespace and leave the tail exposed.
    #[test]
    fn the_cap_does_not_leave_a_trailing_space() {
        let out = sanitize_line(&format!("{} b", "a".repeat(279)), 280);
        assert_eq!(out, "a".repeat(279));
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
        /// What the client dialed, as `dial_origin` spells it. Signed, and the
        /// server accepts only the addresses it knows it answers to.
        #[serde(default)]
        origin: String,
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
    /// Its own message and not a `SetProfile` field: an activity turns over on
    /// every map load, and a profile is edited twice a year.
    SetActivity {
        #[serde(default)]
        activity: Option<Activity>,
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
        /// Alone among these fields, `None` keeps the current name rather than
        /// clearing it — a guild with no name is not a state that exists.
        #[serde(default)]
        name: Option<String>,
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
    SetGuildLeveling {
        guild_id: Id,
        leveling: Leveling,
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
        activities: Vec<UserActivity>,
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
    ActivityUpdate(UserActivity),
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
            origin: String::new(),
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

#[cfg(test)]
mod origin_tests {
    use super::*;

    #[test]
    fn every_spelling_of_one_gateway_is_one_origin() {
        assert_eq!(
            dial_origin("ws://Example.COM/gateway").as_deref(),
            Some("example.com:80")
        );
        assert_eq!(
            dial_origin("wss://example.com").as_deref(),
            Some("example.com:443")
        );
        assert_eq!(
            dial_origin("ws://192.168.0.5:9000/gateway").as_deref(),
            Some("192.168.0.5:9000")
        );
        assert_eq!(
            dial_origin("http://[::1]:9000").as_deref(),
            Some("[::1]:9000")
        );
        assert_eq!(
            dial_origin("example.com:9000"),
            None,
            "a scheme is required"
        );
    }

    #[test]
    fn an_operator_may_write_a_bare_host() {
        assert_eq!(
            host_origin("Chat.Example.com", 9000).as_deref(),
            Some("chat.example.com:9000")
        );
        assert_eq!(
            host_origin("chat.example.com:443", 9000).as_deref(),
            Some("chat.example.com:443")
        );
        assert_eq!(
            host_origin("wss://chat.example.com", 9000).as_deref(),
            Some("chat.example.com:443")
        );
        assert_eq!(host_origin("[::1]", 9000).as_deref(), Some("[::1]:9000"));
        assert_eq!(host_origin("", 9000), None);
    }

    #[test]
    fn a_quic_origin_is_case_insensitive() {
        assert_eq!(quic_origin(" ABCDEF "), "quic:abcdef");
    }

    #[test]
    fn a_share_string_round_trips_and_tolerates_sloppy_input() {
        let key = "ab".repeat(32);
        let addrs = vec![
            "192.168.1.5:4433".to_string(),
            "https://relay.example/".into(),
        ];
        let share = format_quic_share(&key, &addrs);
        assert_eq!(
            share,
            format!("quic://{key}@192.168.1.5:4433;https://relay.example/")
        );
        assert_eq!(
            parse_quic_share(&format!(
                "  {} ",
                share
                    .to_uppercase()
                    .replace("HTTPS://RELAY.EXAMPLE/", "https://relay.example/")
            )),
            Some(QuicShare {
                key: key.clone(),
                addrs: addrs.clone()
            })
        );
        assert_eq!(
            parse_quic_share(&format!("quic://{key}")),
            Some(QuicShare {
                key: key.clone(),
                addrs: vec![]
            }),
            "a bare key is a share that needs a relay"
        );
        assert_eq!(parse_quic_share("quic://@1.2.3.4:1"), None);
        assert_eq!(parse_quic_share("quic://not a key@1.2.3.4:1"), None);
        assert_eq!(parse_quic_share("ws://1.2.3.4:9000"), None);
    }

    #[test]
    fn private_addresses_are_told_from_public_ones() {
        for ip in [
            "192.168.1.1",
            "10.0.0.1",
            "172.16.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fe80::1",
        ] {
            assert!(is_private_ip(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["203.0.113.5", "8.8.8.8", "100.128.0.1", "2001:db8::1"] {
            assert!(!is_private_ip(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn the_payload_separates_its_fields() {
        let a = identify_payload("n", "host:1", "pk", "user");
        let b = identify_payload("n", "host:1p", "k", "user");
        assert_ne!(
            a, b,
            "moving a byte across a field boundary changes the payload"
        );
        assert!(a.starts_with(IDENTIFY_DOMAIN.as_bytes()));
    }
}

#[cfg(test)]
mod leveling_tests {
    use super::*;

    fn tier(xp: u64, name: &str) -> LevelTier {
        LevelTier {
            xp,
            name: name.into(),
            color: None,
        }
    }

    #[test]
    fn tiers_are_sorted_and_a_repeated_threshold_keeps_one() {
        let l = sanitize_leveling(Leveling {
            tiers: vec![
                tier(100, "Veteran"),
                tier(0, "Newcomer"),
                tier(100, "Ghost"),
            ],
            ..Default::default()
        });
        assert_eq!(
            l.tiers.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["Newcomer", "Veteran"]
        );
    }

    #[test]
    fn a_nameless_tier_is_dropped_rather_than_drawn_blank() {
        let l = sanitize_leveling(Leveling {
            tiers: vec![tier(0, "  "), tier(10, "Regular")],
            ..Default::default()
        });
        assert_eq!(l.tiers.len(), 1);
        assert_eq!(l.tiers[0].name, "Regular");
    }

    #[test]
    fn a_member_wears_the_highest_rank_reached() {
        let l = sanitize_leveling(Leveling {
            tiers: vec![
                tier(0, "Newcomer"),
                tier(50, "Regular"),
                tier(500, "Veteran"),
            ],
            ..Default::default()
        });
        assert_eq!(l.tier_at(0).map(|t| t.name.as_str()), Some("Newcomer"));
        assert_eq!(l.tier_at(49).map(|t| t.name.as_str()), Some("Newcomer"));
        assert_eq!(l.tier_at(50).map(|t| t.name.as_str()), Some("Regular"));
        assert_eq!(l.tier_at(9_000).map(|t| t.name.as_str()), Some("Veteran"));
    }

    #[test]
    fn a_first_tier_above_zero_leaves_the_bottom_unnamed() {
        let l = sanitize_leveling(Leveling {
            tiers: vec![tier(50, "Regular")],
            ..Default::default()
        });
        assert_eq!(l.tier_at(5), None);
        assert_eq!(l.label_at(5), "Lv1", "unranked falls back to the number");
        assert_eq!(
            l.label_at(30),
            "Lv3",
            "and keeps counting up under the tier"
        );
        assert_eq!(l.label_at(50), "Regular");
    }

    #[test]
    fn an_unnamed_guild_still_labels_by_level() {
        let l = Leveling::default();
        assert_eq!(l.label_at(0), "Lv1");
        assert_eq!(l.label_at(30), "Lv3");
    }

    #[test]
    fn an_empty_allowlist_earns_everywhere_and_a_full_one_does_not() {
        let only = Id::new_v4();
        let other = Id::new_v4();
        let open = Leveling::default();
        assert!(open.channel_earns(other));

        let closed = sanitize_leveling(Leveling {
            channels: vec![only],
            ..Default::default()
        });
        assert!(closed.channel_earns(only));
        assert!(!closed.channel_earns(other));
    }

    #[test]
    fn amounts_and_cooldowns_are_capped_not_rejected() {
        let l = sanitize_leveling(Leveling {
            per_message: 10_000,
            cooldown_secs: 999_999,
            ..Default::default()
        });
        assert_eq!(l.per_message, MAX_XP_PER_ACTION);
        assert_eq!(l.cooldown_secs, MAX_XP_COOLDOWN);
    }

    #[test]
    fn a_colour_the_client_cannot_draw_is_dropped() {
        let l = sanitize_leveling(Leveling {
            tiers: vec![
                LevelTier {
                    xp: 0,
                    name: "Fine".into(),
                    color: Some("#abc".into()),
                },
                LevelTier {
                    xp: 10,
                    name: "Bad".into(),
                    color: Some("red; content: evil".into()),
                },
            ],
            ..Default::default()
        });
        assert_eq!(l.tiers[0].color.as_deref(), Some("#abc"));
        assert_eq!(l.tiers[1].color, None);
    }

    #[test]
    fn defaults_are_what_every_guild_had_before_this_was_settable() {
        let l = Leveling::default();
        assert!(l.enabled);
        assert_eq!(l.amount_for(XpAction::Message), 1);
        assert_eq!(l.amount_for(XpAction::Reaction), 0);
        assert_eq!(l.amount_for(XpAction::VoiceMinute), 0);
        assert_eq!(l.cooldown_secs, 60);
        assert_eq!(l.member_sort, MemberSort::Name);
    }
}
