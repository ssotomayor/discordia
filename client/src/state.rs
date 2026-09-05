use std::collections::{BTreeMap, HashMap, HashSet};

use dioxus::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::host::HostInfo;
use crate::protocol::{
    BotInstall, Channel, ClientMessage, Guild, GuildSummary, Id, Member, Message, Permission,
    Profile, Role, User, VoiceState,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionMode {
    Remote {
        server_url: String,
    },
    SelfHost {
        allow_lan: bool,
        rendezvous_url: Option<String>,
        publish_name: Option<String>,
        description: Option<String>,
        publish_public: bool,
    },
    ByCode {
        rendezvous_url: String,
        code: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionParams {
    pub mode: SessionMode,
    pub username: String,
    pub identity: crate::identity::Identity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Loopback,
    Quic,
    QuicRelayed,
    Proxied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Ready,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoicePhase {
    Idle,
    Connecting,
    Connected,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionHealth {
    Excellent,
    Good,
    Poor,
    Lost,
}

impl ConnectionHealth {
    pub fn dot(self, is_self: bool) -> Option<(&'static str, &'static str)> {
        match (self, is_self) {
            (Self::Excellent | Self::Good, _) => None,
            (Self::Poor, false) => {
                Some(("var(--warn)", "Weak connection — their audio may drop out"))
            }
            (Self::Poor, true) => Some((
                "var(--warn)",
                "Your connection is weak — others may hear you drop out",
            )),
            (Self::Lost, false) => Some((
                "var(--danger)",
                "Connection lost — the server has stopped hearing them",
            )),
            (Self::Lost, true) => Some((
                "var(--danger)",
                "Connection lost — the server has stopped hearing you",
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackStats {
    Inbound {
        loss_pct: f32,
        jitter_ms: f32,
        buffer_ms: f32,
        concealment_events: u64,
    },
    Outbound {
        bitrate_kbps: Option<u32>,
        packets_per_sec: Option<u32>,
        target_kbps: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceSession {
    pub phase: VoicePhase,
    pub channel_id: Option<Id>,
    pub muted: bool,
    pub deafened: bool,
    pub muted_before_deafen: bool,
    pub speaking: bool,
    pub error: Option<String>,
}

impl Default for VoiceSession {
    fn default() -> Self {
        Self {
            phase: VoicePhase::Idle,
            channel_id: None,
            muted: false,
            deafened: false,
            muted_before_deafen: false,
            speaking: false,
            error: None,
        }
    }
}

impl VoiceSession {
    pub fn toggle_deafen(&mut self) -> (bool, bool) {
        if self.deafened {
            (self.muted_before_deafen, false)
        } else {
            self.muted_before_deafen = self.muted;
            (true, true)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuildDialog {
    Settings(Id),
    Integrations(Id),
    Roles(Id),
}
/// The guild is not in `guilds` yet — we have not joined — so the name is
/// looked up in the catalog and may be absent on an invite-code join.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RulesPrompt {
    pub guild_id: Id,
    pub guild_name: Option<String>,
    pub rules: String,
    pub invite_code: Option<String>,
}

impl RulesPrompt {
    /// A challenge raised by an invite has to be answered through the invite:
    /// the code is what the server matched, and a private guild refuses the
    /// plain join.
    pub fn accept(&self) -> ClientMessage {
        match &self.invite_code {
            Some(code) => ClientMessage::JoinByInvite {
                code: code.clone(),
                accept: true,
                pow_nonce: None,
            },
            None => ClientMessage::JoinGuild {
                guild_id: self.guild_id,
                accept: true,
                pow_nonce: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyDraft {
    pub message_id: Id,
    pub channel_id: Id,
    pub author_username: String,
    pub excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraDevice {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmInfo {
    pub channel_id: Id,
    pub other_pubkey: String,
}

#[derive(Clone)]
pub struct AppState {
    pub status: ConnectionStatus,
    pub self_user: Option<User>,
    pub guilds: Vec<Guild>,
    pub channels: Vec<Channel>,
    pub members: Vec<Member>,
    pub messages: BTreeMap<Id, Vec<Message>>,
    pub voice_states: Vec<VoiceState>,
    pub voice: VoiceSession,
    pub voice_session_epoch: u64,
    pub selected_guild: Option<Id>,
    pub selected_channel: Option<Id>,
    pub dms: Vec<DmInfo>,
    pub dm_unread: HashMap<Id, u32>,
    /// Seeded from `ClientSettings::dm_cleared_at`; see it for why a delete is
    /// a watermark. Read on every insert, so replayed history stays hidden.
    pub dm_cleared_at: HashMap<String, i64>,
    /// Seeded from `ClientSettings::dm_read_at`. Read on every insert, so the
    /// history the relays replay at launch does not raise an alert twice.
    pub dm_read_at: HashMap<String, i64>,
    /// Seeded from `ClientSettings`. Local and personal: muting is never sent
    /// anywhere, and a muted channel is silent for its mentions too — otherwise
    /// the word would mean two things.
    pub muted_channels: HashSet<Id>,
    pub muted_guilds: HashSet<Id>,
    pub nostr_event_ids: HashMap<Id, String>,
    pub contacts: crate::nostr::nip02::ContactList,
    pub nostr_relays_up: std::collections::HashSet<String>,
    /// Names peers published for themselves (kind 0), by pubkey, each with the
    /// `created_at` it came with. Kept because kind 0 is replaceable and the
    /// pool dedupes by event id: a rename is a new id, so the old copy still
    /// arrives, and last-writer-wins would show whichever relay was slower.
    pub nostr_names: HashMap<String, (String, i64)>,
    pub dm_mode: bool,
    /// Whether the surface holding the selected conversation is on screen. The
    /// home drawer closes over a DM that stays selected, and a message arriving
    /// behind it is unread however selected it is.
    pub dm_pane_open: bool,
    pub catalog: Vec<GuildSummary>,
    pub catalog_total: u32,
    pub profiles: HashMap<String, Profile>,
    pub profile_card: Option<String>,
    pub image_viewer: Option<String>,
    pub guild_dialog: Option<GuildDialog>,
    pub rules_prompt: Option<RulesPrompt>,
    /// Opened from the title bar, rendered by the voice panel that owns the
    /// device signals — the flag is the only thing the two need to share.
    pub audio_settings: bool,
    pub typing: HashMap<Id, HashMap<String, (String, std::time::Instant)>>,
    pub notify_tick: u64,
    pub screen_token: Option<(String, String)>,
    pub screen_audio_token: Option<(String, String)>,
    /// Whether the native side is *actually in*, not merely holding a token:
    /// a failed join must hand playback back to the webview, not go silent.
    pub screen_audio_joined: bool,
    pub screen_video_token: Option<(String, String)>,
    pub screen_share_target: Option<crate::sysvideo::Target>,
    pub screen_picker: Option<Result<Vec<crate::sysvideo::Source>, String>>,
    pub screen_sharing: bool,
    pub screen_native_audio: bool,
    pub screen_shares: HashMap<Id, Vec<String>>,
    pub screen_viewing: Option<String>,
    pub replying_to: Option<ReplyDraft>,
    pub screen_capture_available: bool,

    pub camera_on: bool,
    pub camera_starting: bool,
    pub available_cameras: Vec<CameraDevice>,
    pub cameras_watching: HashSet<String>,
    pub camera_capture_available: bool,

    pub available_input_devices: Vec<String>,
    pub available_output_devices: Vec<String>,
    pub selected_input_device: Option<String>,
    pub selected_output_device: Option<String>,
    pub mic_sensitivity: u32,
    pub mic_volume: u16,
    pub auto_gain_control: bool,
    pub mic_level: u32,
    pub mic_level_pre: u32,
    pub noise_cancellation: bool,
    pub denoise_atten_lim_db: u32,
    pub bypass_system_audio_processing: bool,
    pub mic_bypass_error: Option<String>,
    pub voice_bitrate_kbps: u32,
    pub voice_quality: HashMap<String, ConnectionHealth>,
    pub voice_stats: HashMap<String, TrackStats>,
    pub user_volumes: HashMap<String, u32>,
    pub user_muted: HashSet<String>,
    pub stream_volumes: HashMap<String, u32>,
    pub stream_muted: HashSet<String>,
    pub stream_has_audio: HashSet<String>,
    pub media_undecryptable: bool,
    pub pending_rekey: bool,
    pub identity: Option<crate::identity::Identity>,
    pub media_keys: HashMap<Id, (u32, [u8; 32])>,
    pub host_info: Option<HostInfo>,
    pub transport: Transport,
    pub integrations: HashMap<Id, Vec<BotInstall>>,
    pub guild_emojis: HashMap<Id, Vec<crate::protocol::GuildEmoji>>,
    pub emoji_images: HashMap<String, String>,
    pub emoji_requested: HashSet<String>,
    pub roles: HashMap<Id, Vec<Role>>,
    pub bans: HashMap<Id, Vec<User>>,
    pub invites: HashMap<Id, String>,
    pub error_toast: Option<String>,
    pub is_operator: bool,
    pub audit_logs: HashMap<Id, Vec<crate::protocol::AuditEntry>>,
}

impl AppState {
    pub fn empty() -> Self {
        Self {
            status: ConnectionStatus::Connecting,
            self_user: None,
            guilds: Vec::new(),
            channels: Vec::new(),
            members: Vec::new(),
            messages: BTreeMap::new(),
            voice_states: Vec::new(),
            voice: VoiceSession::default(),
            voice_session_epoch: 0,
            selected_guild: None,
            selected_channel: None,
            dms: Vec::new(),
            dm_unread: HashMap::new(),
            dm_cleared_at: HashMap::new(),
            dm_read_at: HashMap::new(),
            muted_channels: HashSet::new(),
            muted_guilds: HashSet::new(),
            nostr_event_ids: HashMap::new(),
            contacts: Default::default(),
            nostr_relays_up: std::collections::HashSet::new(),
            nostr_names: HashMap::new(),
            dm_mode: false,
            dm_pane_open: true,
            catalog: Vec::new(),
            catalog_total: 0,
            profiles: HashMap::new(),
            profile_card: None,
            image_viewer: None,
            guild_dialog: None,
            rules_prompt: None,
            audio_settings: false,
            typing: HashMap::new(),
            notify_tick: 0,
            screen_token: None,
            screen_audio_token: None,
            screen_video_token: None,
            screen_share_target: None,
            screen_picker: None,
            screen_audio_joined: false,
            screen_sharing: false,
            screen_native_audio: false,
            screen_shares: HashMap::new(),
            screen_viewing: None,
            replying_to: None,
            screen_capture_available: false,
            camera_on: false,
            camera_starting: false,
            available_cameras: Vec::new(),
            cameras_watching: HashSet::new(),
            camera_capture_available: false,
            available_input_devices: Vec::new(),
            available_output_devices: Vec::new(),
            selected_input_device: None,
            selected_output_device: None,
            mic_sensitivity: 50,
            mic_volume: 100,
            auto_gain_control: true,
            mic_level: 0,
            mic_level_pre: 0,
            noise_cancellation: false,
            denoise_atten_lim_db: 30,
            bypass_system_audio_processing: false,
            mic_bypass_error: None,
            voice_bitrate_kbps: 48,
            voice_quality: HashMap::new(),
            voice_stats: HashMap::new(),
            user_volumes: HashMap::new(),
            user_muted: HashSet::new(),
            stream_volumes: HashMap::new(),
            stream_muted: HashSet::new(),
            stream_has_audio: HashSet::new(),
            media_undecryptable: false,
            pending_rekey: false,
            identity: None,
            media_keys: HashMap::new(),
            host_info: None,
            transport: Transport::Loopback,
            integrations: HashMap::new(),
            guild_emojis: HashMap::new(),
            emoji_images: HashMap::new(),
            emoji_requested: HashSet::new(),
            roles: HashMap::new(),
            bans: HashMap::new(),
            invites: HashMap::new(),
            error_toast: None,
            is_operator: false,
            audit_logs: HashMap::new(),
        }
    }

    pub fn is_owner(&self, guild_id: Id) -> bool {
        let Some(me) = self.self_user.as_ref() else {
            return false;
        };
        self.guilds
            .iter()
            .find(|g| g.id == guild_id)
            .map(|g| {
                if g.owner_pubkey.is_empty() {
                    self.is_operator
                } else {
                    g.owner_pubkey == me.pubkey
                }
            })
            .unwrap_or(false)
    }

    /// Advisory only — it hides dead-end UI. The server re-checks everything.
    pub fn can(&self, guild_id: Id, perm: Permission) -> bool {
        if self.is_owner(guild_id) {
            return true;
        }
        let Some(me) = self.self_user.as_ref() else {
            return false;
        };
        let Some(member) = self
            .members
            .iter()
            .find(|m| m.guild_id == guild_id && m.user.pubkey == me.pubkey)
        else {
            return false;
        };
        let Some(roles) = self.roles.get(&guild_id) else {
            return false;
        };
        member.roles.iter().any(|rid| {
            roles
                .iter()
                .find(|r| r.id == *rid)
                .is_some_and(|r| r.permissions.contains(&perm))
        })
    }

    pub fn emoji_image(&self, guild_id: Id, shortcode: &str) -> Option<&str> {
        let image = self
            .guild_emojis
            .get(&guild_id)?
            .iter()
            .find(|e| e.shortcode == shortcode)
            .map(|e| e.image.as_str())?;
        self.emoji_images
            .get(image)
            .map(String::as_str)
            .filter(|u| !u.is_empty())
    }

    pub fn emojis_of(&self, guild_id: Id) -> &[crate::protocol::GuildEmoji] {
        self.guild_emojis
            .get(&guild_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn roles_of(&self, guild_id: Id) -> &[Role] {
        self.roles
            .get(&guild_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn screen_sharers_in(&self, channel_id: Id) -> Vec<String> {
        let mut out: Vec<String> = self
            .voice_states
            .iter()
            .filter(|v| v.screen_sharing && v.channel_id == Some(channel_id))
            .map(|v| v.user_pubkey.clone())
            .collect();
        if let Some(legacy) = self.screen_shares.get(&channel_id) {
            for pk in legacy {
                if !out.contains(pk) {
                    out.push(pk.clone());
                }
            }
        }
        out.sort();
        out
    }

    pub fn cameras_in(&self, channel_id: Id) -> Vec<String> {
        let mut out: Vec<String> = self
            .voice_states
            .iter()
            .filter(|v| v.camera_on && v.channel_id == Some(channel_id))
            .map(|v| v.user_pubkey.clone())
            .collect();
        out.sort();
        out
    }

    pub fn typers_in(&self, channel_id: Id) -> Vec<String> {
        let mut names: Vec<String> = self
            .typing
            .get(&channel_id)
            .map(|m| m.values().map(|(name, _)| name.clone()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    pub fn profile_of(&self, pubkey: &str) -> Option<&Profile> {
        self.profiles.get(pubkey)
    }

    pub fn presence_of(&self, pubkey: &str) -> &str {
        let label = self
            .profiles
            .get(pubkey)
            .and_then(|p| p.status.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("online");
        if self.self_user.as_ref().is_some_and(|u| u.pubkey == pubkey) {
            return label;
        }
        let mut seen = false;
        for m in self.members.iter().filter(|m| m.user.pubkey == pubkey) {
            if m.online {
                return label;
            }
            seen = true;
        }
        if seen { "offline" } else { label }
    }

    pub fn voice_gain_of(&self, pubkey: &str) -> f32 {
        if self.user_muted.contains(pubkey) {
            return 0.0;
        }
        self.user_volumes.get(pubkey).copied().unwrap_or(100) as f32 / 100.0
    }

    pub fn stream_gain_of(&self, pubkey: &str) -> f32 {
        if self.stream_muted.contains(pubkey) {
            return 0.0;
        }
        self.stream_volumes.get(pubkey).copied().unwrap_or(100) as f32 / 100.0
    }

    pub fn avatar_of(&self, pubkey: &str) -> Option<&str> {
        self.profiles
            .get(pubkey)
            .and_then(|p| p.avatar.as_deref())
            .and_then(|a| self.media_src(a))
    }

    pub fn banner_of(&self, pubkey: &str) -> Option<&str> {
        self.profiles
            .get(pubkey)
            .and_then(|p| p.banner.as_deref())
            .and_then(|b| self.media_src(b))
    }

    /// The server sends pictures as `media:` addresses and the bytes arrive
    /// separately, so a lookup can miss while the blob is still in flight.
    pub fn media_src<'a>(&'a self, raw: &'a str) -> Option<&'a str> {
        match raw.strip_prefix("media:") {
            None => Some(raw),
            Some(address) => self
                .emoji_images
                .get(address)
                .map(String::as_str)
                .filter(|u| !u.is_empty()),
        }
    }

    pub fn default_channel_of(&self, guild_id: Id) -> Option<Id> {
        self.channels
            .iter()
            .filter(|c| {
                c.guild_id == guild_id && matches!(c.kind, crate::protocol::ChannelKind::Text)
            })
            .min_by(|a, b| {
                a.position
                    .cmp(&b.position)
                    .then_with(|| a.name.cmp(&b.name))
            })
            .map(|c| c.id)
    }

    /// A roster name is one on a server you share, a petname is one you typed,
    /// and kind 0 is the peer's own — in that order of who stands behind it.
    pub fn display_name(&self, pubkey: &str) -> String {
        if let Some(u) = self.user_of(pubkey) {
            return u.username.clone();
        }
        if let Some(pet) = self.contacts.petname(pubkey) {
            return pet.to_string();
        }
        if let Some((published, _)) = self.nostr_names.get(pubkey) {
            return published.clone();
        }
        crate::identity::truncate_pubkey(pubkey)
    }

    pub fn dm_of(&self, channel_id: Id) -> Option<&DmInfo> {
        self.dms.iter().find(|d| d.channel_id == channel_id)
    }

    pub fn dm_last_message(&self, channel_id: Id) -> Option<&Message> {
        self.messages.get(&channel_id).and_then(|m| m.last())
    }

    /// Empty conversations sort last: one just opened by pasting a key has no
    /// activity to be recent about.
    pub fn dms_by_recency(&self) -> Vec<DmInfo> {
        let mut v = self.dms.clone();
        v.sort_by(|a, b| {
            let at = self.dm_last_message(a.channel_id).map(|m| m.created_at);
            let bt = self.dm_last_message(b.channel_id).map(|m| m.created_at);
            bt.cmp(&at)
        });
        v
    }

    pub fn dm_unread_total(&self) -> u32 {
        self.dm_unread.values().copied().sum()
    }

    pub fn is_muted(&self, channel_id: Id) -> bool {
        if self.muted_channels.contains(&channel_id) {
            return true;
        }
        self.channels
            .iter()
            .find(|c| c.id == channel_id)
            .is_some_and(|c| self.muted_guilds.contains(&c.guild_id))
    }

    /// Puts one message in its conversation in time order, and says whether it
    /// was new. Arriving twice is ordinary here, not exceptional: relays replay
    /// their whole history and a fetched page overlaps what was delivered live.
    pub fn insert_message(&mut self, channel_id: Id, message: Message) -> bool {
        let held = self.messages.entry(channel_id).or_default();
        if held.iter().any(|m| m.id == message.id) {
            return false;
        }
        held.push(message);
        held.sort_by_key(|m| m.created_at);
        true
    }

    /// Merges a fetched page into what is already held.
    pub fn merge_history(&mut self, channel_id: Id, page: Vec<Message>) {
        let held = self.messages.entry(channel_id).or_default();
        let mut seen: HashSet<Id> = held.iter().map(|m| m.id).collect();
        for m in page {
            // Recorded as we go rather than sampled once: a page that repeats
            // an id would otherwise be believed twice.
            if seen.insert(m.id) {
                held.push(m);
            }
        }
        held.sort_by_key(|m| m.created_at);
    }

    /// Only this channel's own flag, for the menu that toggles it: `is_muted`
    /// also answers yes when the whole guild is muted, and a menu reading that
    /// would offer to unmute something it cannot.
    pub fn channel_muted(&self, channel_id: Id) -> bool {
        self.muted_channels.contains(&channel_id)
    }

    pub fn set_channel_muted(&mut self, channel_id: Id, muted: bool) {
        if muted {
            self.muted_channels.insert(channel_id);
        } else {
            self.muted_channels.remove(&channel_id);
        }
    }

    pub fn guild_muted(&self, guild_id: Id) -> bool {
        self.muted_guilds.contains(&guild_id)
    }

    pub fn set_guild_muted(&mut self, guild_id: Id, muted: bool) {
        if muted {
            self.muted_guilds.insert(guild_id);
        } else {
            self.muted_guilds.remove(&guild_id);
        }
    }

    /// Whether an arriving message should make a sound.
    ///
    /// One rule for both arrival paths, because the gateway and the relays each
    /// used to decide for themselves and only one of them ever decided yes.
    pub fn should_ring(&self, channel_id: Id, author_is_self: bool, viewing: bool) -> bool {
        !author_is_self && !viewing && !self.is_muted(channel_id)
    }

    /// Records an arriving DM against the read watermark: counted when it is
    /// not on screen, and covered by the mark when it is.
    ///
    /// The mark has to move while you watch, too. Relays replay the whole
    /// history on every launch and `dm_unread` is rebuilt from it, so a message
    /// only ever read live would come back as unread on the next one.
    pub fn note_dm_arrival(&mut self, channel_id: Id, peer: &str, at: i64) {
        let viewing =
            self.dm_pane_open && self.selected_channel == Some(channel_id) && self.dm_mode;
        if viewing {
            let mark = self.dm_read_at.entry(peer.to_string()).or_insert(at);
            *mark = (*mark).max(at);
        } else if self.dm_read_at.get(peer).is_none_or(|mark| at > *mark) {
            *self.dm_unread.entry(channel_id).or_insert(0) += 1;
            // Tied to the counter and not merely to `viewing`: the relays replay
            // the whole history on every launch, and ringing for that would be a
            // burst of sound for messages read days ago.
            if self.should_ring(channel_id, false, viewing) {
                self.notify_tick = self.notify_tick.wrapping_add(1);
            }
        }
    }

    /// Marks a conversation read up to the newest message it holds.
    ///
    /// Dropping the counter alone is not enough — it is rebuilt from the replay
    /// on the next launch — so this leaves the watermark `note_dm_arrival`
    /// reads.
    pub fn mark_dm_read(&mut self, channel_id: Id) {
        self.dm_unread.remove(&channel_id);
        let Some(peer) = self.dm_of(channel_id).map(|d| d.other_pubkey.clone()) else {
            return;
        };
        let newest = self
            .messages
            .get(&channel_id)
            .into_iter()
            .flatten()
            .map(|m| m.created_at.timestamp())
            .max();
        if let Some(at) = newest {
            let mark = self.dm_read_at.entry(peer).or_insert(at);
            *mark = (*mark).max(at);
        }
    }

    /// Records a name a peer published for themselves, keeping the newest.
    ///
    /// Ties keep what is already there: two relays serving the same event is
    /// the ordinary case, and re-inserting it would churn the signal for
    /// nothing. Returns whether the map changed.
    pub fn note_name(&mut self, pubkey: &str, name: String, at: i64) -> bool {
        match self.nostr_names.get(pubkey) {
            Some((_, seen)) if *seen >= at => false,
            _ => {
                self.nostr_names.insert(pubkey.to_string(), (name, at));
                true
            }
        }
    }

    /// Forgets a conversation here. The relays and the other person keep their
    /// copies, so this drops what is below the watermark and nothing more.
    pub fn clear_dm(&mut self, peer: &str, at: i64) {
        let cid = crate::nostr::service::conversation_id(peer);
        self.dm_cleared_at
            .entry(peer.to_string())
            .and_modify(|t| *t = (*t).max(at))
            .or_insert(at);
        self.dms.retain(|d| d.channel_id != cid);
        self.messages.remove(&cid);
        self.dm_unread.remove(&cid);
        if self.selected_channel == Some(cid) {
            self.selected_channel = None;
        }
    }

    pub fn members_of(&self, guild_id: Id) -> Vec<&Member> {
        let mut v: Vec<&Member> = self
            .members
            .iter()
            .filter(|m| m.guild_id == guild_id)
            .collect();
        v.sort_by(|a, b| {
            b.online.cmp(&a.online).then_with(|| {
                a.user
                    .username
                    .to_lowercase()
                    .cmp(&b.user.username.to_lowercase())
            })
        });
        v
    }

    pub fn user_of(&self, pubkey: &str) -> Option<&User> {
        if self
            .self_user
            .as_ref()
            .map(|u| u.pubkey == pubkey)
            .unwrap_or(false)
        {
            return self.self_user.as_ref();
        }
        self.members
            .iter()
            .find(|m| m.user.pubkey == pubkey)
            .map(|m| &m.user)
    }
}

#[derive(Clone)]
pub struct GatewayTx(pub UnboundedSender<ClientMessage>);

impl GatewayTx {
    pub fn send(&self, msg: ClientMessage) {
        let _ = self.0.send(msg);
    }
}

pub fn use_app_state() -> Signal<AppState> {
    use_context::<Signal<AppState>>()
}

/// Both halves or neither: the map is what the screen reads and the file is
/// what survives a restart, and a conversation forgotten in only one of them
/// walks back in when the relays replay.
pub fn forget_dm(
    mut state: Signal<AppState>,
    mut settings: Signal<crate::settings::ClientSettings>,
    peer: &str,
) {
    let at = chrono::Utc::now().timestamp();
    state.write().clear_dm(peer, at);
    let mut next = settings.peek().clone();
    next.clear_dm(peer, at);
    settings.set(next.clone());
    crate::settings::save(&next);
}

/// Same two halves, for the same reason.
pub fn set_dm_muted(
    mut state: Signal<AppState>,
    mut settings: Signal<crate::settings::ClientSettings>,
    channel_id: Id,
    muted: bool,
) {
    state.write().set_channel_muted(channel_id, muted);
    let mut next = settings.peek().clone();
    next.set_muted_channel(channel_id, muted);
    settings.set(next.clone());
    crate::settings::save(&next);
}

/// Mirrors the read watermarks into the settings file as they move.
///
/// A hook rather than a line at each read site: the counter is cleared from four
/// places and `HomeView` and `WorkspaceView` each build their own `AppState`, so
/// one writer is what keeps the file from drifting from the map.
pub fn use_dm_read_persistence(state: Signal<AppState>) {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let marks = use_memo(move || {
        let mut v: Vec<(String, i64)> = state
            .read()
            .dm_read_at
            .iter()
            .map(|(peer, at)| (peer.clone(), *at))
            .collect();
        v.sort();
        v
    });
    use_effect(move || {
        let marks = marks();
        if marks.is_empty() {
            return;
        }
        // `peek`, not `read`: writing back what this effect subscribes to is a
        // loop.
        let mut next = settings.peek().clone();
        let changed = marks
            .into_iter()
            .fold(false, |acc, (peer, at)| next.mark_dm_read(&peer, at) || acc);
        if changed {
            settings.set(next.clone());
            crate::settings::save(&next);
        }
    });
}

pub fn use_gateway() -> GatewayTx {
    use_context::<GatewayTx>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Member, Profile, User};

    fn prompt(invite_code: Option<&str>) -> RulesPrompt {
        RulesPrompt {
            guild_id: Id::new_v4(),
            guild_name: None,
            rules: "be nice".into(),
            invite_code: invite_code.map(str::to_string),
        }
    }

    /// A challenge raised by an invite must be answered through the invite: a
    /// private guild refuses the plain join, so answering with `JoinGuild`
    /// would bounce the person straight back out.
    #[test]
    fn an_invite_challenge_is_accepted_through_the_invite() {
        match prompt(Some("purple-fox-42")).accept() {
            ClientMessage::JoinByInvite {
                code,
                accept,
                pow_nonce,
            } => {
                assert_eq!(code, "purple-fox-42");
                assert!(accept, "the whole point is that the person accepted");
                assert!(pow_nonce.is_none(), "a rules gate carries no work");
            }
            other => panic!("answered an invite with {other:?}"),
        }
    }

    #[test]
    fn a_catalog_challenge_is_accepted_by_guild_id() {
        let p = prompt(None);
        match p.accept() {
            ClientMessage::JoinGuild {
                guild_id, accept, ..
            } => {
                assert_eq!(guild_id, p.guild_id);
                assert!(accept);
            }
            other => panic!("answered a catalog join with {other:?}"),
        }
    }

    /// The defect this replaced: relays are asked in parallel and deduped by
    /// event id, so a rename and the old copy both arrive, in any order.
    #[test]
    fn a_stale_kind_0_cannot_undo_a_rename() {
        let mut s = AppState::empty();
        assert!(s.note_name("abcd", "Bob".into(), 200));
        assert!(!s.note_name("abcd", "Alice".into(), 100));
        assert_eq!(s.display_name("abcd"), "Bob");
    }

    /// The rename itself must land whichever way round the two arrive.
    #[test]
    fn a_newer_kind_0_replaces_the_name() {
        let mut s = AppState::empty();
        assert!(s.note_name("abcd", "Alice".into(), 100));
        assert!(s.note_name("abcd", "Bob".into(), 200));
        assert_eq!(s.display_name("abcd"), "Bob");
    }

    /// Two relays serving the same event is the ordinary case, not a change.
    #[test]
    fn the_same_event_twice_changes_nothing() {
        let mut s = AppState::empty();
        assert!(s.note_name("abcd", "Alice".into(), 100));
        assert!(!s.note_name("abcd", "Alice".into(), 100));
    }

    /// The row must go and the mark must stay: without the mark, the relays
    /// replay the same conversation back on the next launch.
    #[test]
    fn clearing_a_dm_drops_the_row_and_leaves_a_watermark() {
        let mut s = AppState::empty();
        let peer = "abcd";
        let cid = crate::nostr::service::conversation_id(peer);
        s.dms.push(DmInfo {
            channel_id: cid,
            other_pubkey: peer.to_string(),
        });
        s.dm_unread.insert(cid, 3);
        s.selected_channel = Some(cid);

        s.clear_dm(peer, 100);

        assert!(s.dms.is_empty());
        assert!(s.dm_unread.is_empty());
        assert_eq!(s.selected_channel, None);
        assert_eq!(s.dm_cleared_at.get(peer), Some(&100));
    }

    /// Clearing again after new messages must not un-hide the older ones.
    #[test]
    fn a_second_clear_keeps_the_later_mark() {
        let mut s = AppState::empty();
        s.clear_dm("abcd", 200);
        s.clear_dm("abcd", 100);
        assert_eq!(s.dm_cleared_at.get("abcd"), Some(&200));
    }

    fn at(id: Id, channel_id: Id, secs: i64) -> Message {
        Message {
            id,
            channel_id,
            author: user("abcd"),
            content: format!("t{secs}"),
            image: None,
            reactions: Vec::new(),
            reply_to: None,
            created_at: chrono::DateTime::from_timestamp(secs, 0).unwrap(),
        }
    }

    /// A relay replays and a fetch overlaps what was already delivered live, so
    /// the same message arriving twice is the ordinary case.
    #[test]
    fn a_message_that_arrives_twice_is_kept_once() {
        let mut s = AppState::empty();
        let cid = Id::new_v4();
        let id = Id::new_v4();

        assert!(s.insert_message(cid, at(id, cid, 100)));
        assert!(!s.insert_message(cid, at(id, cid, 100)));
        assert_eq!(s.messages[&cid].len(), 1);
    }

    /// Out of order is normal: relays answer in whatever order they like.
    #[test]
    fn messages_are_held_in_time_order_however_they_arrive() {
        let mut s = AppState::empty();
        let cid = Id::new_v4();
        for secs in [300, 100, 200] {
            s.insert_message(cid, at(Id::new_v4(), cid, secs));
        }
        let order: Vec<i64> = s.messages[&cid]
            .iter()
            .map(|m| m.created_at.timestamp())
            .collect();
        assert_eq!(order, vec![100, 200, 300]);
    }

    /// The page and the memory overlap by design — the fetch re-reads what a
    /// live message already delivered.
    #[test]
    fn a_page_does_not_duplicate_what_is_already_held() {
        let mut s = AppState::empty();
        let cid = Id::new_v4();
        let shared = Id::new_v4();
        s.insert_message(cid, at(shared, cid, 200));

        s.merge_history(cid, vec![at(shared, cid, 200), at(Id::new_v4(), cid, 100)]);

        assert_eq!(s.messages[&cid].len(), 2);
    }

    /// And the page can repeat itself. Sampling the ids once before the loop
    /// believed such a page twice.
    #[test]
    fn a_page_that_repeats_an_id_is_believed_once() {
        let mut s = AppState::empty();
        let cid = Id::new_v4();
        let twice = Id::new_v4();

        s.merge_history(cid, vec![at(twice, cid, 100), at(twice, cid, 100)]);

        assert_eq!(s.messages[&cid].len(), 1);
    }

    fn text_channel(s: &mut AppState, guild_id: Id) -> Id {
        let id = Id::new_v4();
        s.channels.push(Channel {
            id,
            guild_id,
            name: "general".into(),
            kind: crate::protocol::ChannelKind::Text,
            topic: None,
            read_only: false,
            slowmode_secs: 0,
            position: 0,
        });
        id
    }

    /// The whole complaint: an ordinary message used to ring only if it named
    /// you, so a channel nobody mentioned you in was silent.
    #[test]
    fn a_message_in_a_channel_you_are_not_reading_rings() {
        let mut s = AppState::empty();
        let cid = text_channel(&mut s, Id::new_v4());
        assert!(s.should_ring(cid, false, false));
        assert!(!s.should_ring(cid, false, true), "you are looking at it");
        assert!(!s.should_ring(cid, true, false), "you wrote it");
    }

    /// Muting means one thing, so it silences everything in that channel. A
    /// mention that still rang would make the word mean two.
    #[test]
    fn muting_a_channel_silences_it() {
        let mut s = AppState::empty();
        let cid = text_channel(&mut s, Id::new_v4());
        s.set_channel_muted(cid, true);
        assert!(!s.should_ring(cid, false, false));

        s.set_channel_muted(cid, false);
        assert!(s.should_ring(cid, false, false));
    }

    /// And muting the guild reaches every channel in it, including ones that
    /// arrive after the mute.
    #[test]
    fn muting_a_guild_silences_the_channels_under_it() {
        let mut s = AppState::empty();
        let guild = Id::new_v4();
        let cid = text_channel(&mut s, guild);
        s.set_guild_muted(guild, true);
        assert!(!s.should_ring(cid, false, false));

        let later = text_channel(&mut s, guild);
        assert!(!s.should_ring(later, false, false));
        assert!(
            !s.channel_muted(cid),
            "the channel's own flag is untouched, or the menu would offer to              unmute something it cannot"
        );

        let elsewhere = text_channel(&mut s, Id::new_v4());
        assert!(s.should_ring(elsewhere, false, false));
    }

    /// Relays replay the whole history at every launch. Ringing for that would
    /// be a burst of sound for messages read days ago, so the sound is tied to
    /// the unread counter rather than to `viewing` alone.
    #[test]
    fn a_replayed_dm_neither_counts_nor_rings() {
        let mut s = AppState::empty();
        let peer = "abcd";
        let cid = dm_with(&mut s, peer, &[100]);

        s.note_dm_arrival(cid, peer, 100);
        assert_eq!(s.dm_unread.get(&cid), Some(&1));
        assert_eq!(s.notify_tick, 1);

        s.mark_dm_read(cid);
        let after_reading = s.notify_tick;
        s.note_dm_arrival(cid, peer, 100);
        assert!(s.dm_unread.is_empty());
        assert_eq!(s.notify_tick, after_reading, "the replay rang again");
    }

    fn dm_with(s: &mut AppState, peer: &str, ats: &[i64]) -> Id {
        let cid = crate::nostr::service::conversation_id(peer);
        if !s.dms.iter().any(|d| d.channel_id == cid) {
            s.dms.push(DmInfo {
                channel_id: cid,
                other_pubkey: peer.to_string(),
            });
        }
        let entry = s.messages.entry(cid).or_default();
        for at in ats {
            entry.push(Message {
                id: Id::new_v4(),
                channel_id: cid,
                author: user(peer),
                content: "hi".into(),
                image: None,
                reactions: Vec::new(),
                reply_to: None,
                created_at: chrono::DateTime::from_timestamp(*at, 0).unwrap(),
            });
        }
        cid
    }

    /// The bug this whole watermark exists for: relays replay the history on
    /// every launch, so without a persisted mark the same messages raise the
    /// same alert again and reading never sticks.
    #[test]
    fn a_replayed_history_does_not_raise_the_alert_twice() {
        let mut s = AppState::empty();
        let peer = "abcd";
        let cid = dm_with(&mut s, peer, &[100, 200]);
        s.note_dm_arrival(cid, peer, 100);
        s.note_dm_arrival(cid, peer, 200);
        assert_eq!(s.dm_unread.get(&cid), Some(&2));

        s.mark_dm_read(cid);
        assert!(s.dm_unread.is_empty());

        s.note_dm_arrival(cid, peer, 100);
        s.note_dm_arrival(cid, peer, 200);
        assert!(s.dm_unread.is_empty());
        assert_eq!(s.dm_read_at.get(peer), Some(&200));
    }

    /// A message read live has to leave the mark too, or the next launch counts
    /// it as never seen.
    #[test]
    fn watching_a_conversation_moves_the_mark() {
        let mut s = AppState::empty();
        let peer = "abcd";
        let cid = dm_with(&mut s, peer, &[]);
        s.dm_mode = true;
        s.selected_channel = Some(cid);

        s.note_dm_arrival(cid, peer, 300);

        assert!(s.dm_unread.is_empty());
        assert_eq!(s.dm_read_at.get(peer), Some(&300));
    }

    /// Selected is not the same as on screen: the home drawer closes over the
    /// conversation, and a message arriving behind it was never read.
    #[test]
    fn a_closed_drawer_is_not_watching() {
        let mut s = AppState::empty();
        let peer = "abcd";
        let cid = dm_with(&mut s, peer, &[]);
        s.dm_mode = true;
        s.selected_channel = Some(cid);
        s.dm_pane_open = false;

        s.note_dm_arrival(cid, peer, 300);

        assert_eq!(s.dm_unread.get(&cid), Some(&1));
        assert!(s.dm_read_at.is_empty());
    }

    /// The mark must not swallow what came after it.
    #[test]
    fn a_message_newer_than_the_mark_still_counts() {
        let mut s = AppState::empty();
        let peer = "abcd";
        let cid = dm_with(&mut s, peer, &[100]);
        s.mark_dm_read(cid);

        s.note_dm_arrival(cid, peer, 400);

        assert_eq!(s.dm_unread.get(&cid), Some(&1));
    }

    fn user(pk: &str) -> User {
        User {
            pubkey: pk.into(),
            username: format!("u-{pk}"),
        }
    }

    fn member(pk: &str, online: bool) -> Member {
        Member {
            user: user(pk),
            guild_id: uuid::Uuid::nil(),
            online,
            bot: false,
            roles: Vec::new(),
            xp: 0,
        }
    }

    fn profile(pk: &str, status: Option<&str>) -> Profile {
        Profile {
            pubkey: pk.into(),
            status: status.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn presence_prefers_connection_state_over_the_self_set_label() {
        let mut s = AppState::empty();
        s.members.push(member("alice", false));
        s.profiles
            .insert("alice".into(), profile("alice", Some("online")));
        assert_eq!(s.presence_of("alice"), "offline");
    }

    #[test]
    fn presence_uses_the_label_while_connected() {
        let mut s = AppState::empty();
        s.members.push(member("bob", true));
        s.profiles.insert("bob".into(), profile("bob", Some("dnd")));
        assert_eq!(s.presence_of("bob"), "dnd");

        s.members.push(member("carol", true));
        assert_eq!(s.presence_of("carol"), "online");
    }

    #[test]
    fn presence_is_online_if_any_member_row_is() {
        let mut s = AppState::empty();
        s.members.push(member("dave", false));
        let mut second = member("dave", true);
        second.guild_id = uuid::Uuid::from_u128(1);
        s.members.push(second);
        assert_eq!(s.presence_of("dave"), "online");
    }

    #[test]
    fn presence_falls_back_to_the_label_for_unknown_users() {
        let mut s = AppState::empty();
        s.profiles
            .insert("erin".into(), profile("erin", Some("away")));
        assert_eq!(s.presence_of("erin"), "away");
        assert_eq!(s.presence_of("nobody"), "online");
    }

    #[test]
    fn presence_of_self_uses_the_chosen_label() {
        let mut s = AppState::empty();
        s.self_user = Some(user("me"));
        s.profiles.insert("me".into(), profile("me", Some("dnd")));
        s.members.push(member("me", false));
        assert_eq!(s.presence_of("me"), "dnd");
    }

    #[test]
    fn local_gains_default_to_unity_and_mute_wins_over_volume() {
        let mut s = AppState::empty();
        assert_eq!(s.voice_gain_of("x"), 1.0);
        assert_eq!(s.stream_gain_of("x"), 1.0);

        s.user_volumes.insert("x".into(), 150);
        assert_eq!(s.voice_gain_of("x"), 1.5);
        s.user_muted.insert("x".into());
        assert_eq!(s.voice_gain_of("x"), 0.0);
        s.user_muted.remove("x");
        assert_eq!(s.voice_gain_of("x"), 1.5);

        s.stream_volumes.insert("x".into(), 50);
        assert_eq!(s.stream_gain_of("x"), 0.5);
        assert_eq!(s.voice_gain_of("x"), 1.5);
    }

    #[test]
    fn deafening_mutes_and_undeafening_restores_the_previous_mute() {
        let mut v = VoiceSession::default();
        assert_eq!(v.toggle_deafen(), (true, true));
        v.muted = true;
        v.deafened = true;
        assert_eq!(v.toggle_deafen(), (false, false));

        let mut v = VoiceSession {
            muted: true,
            ..VoiceSession::default()
        };
        assert_eq!(v.toggle_deafen(), (true, true));
        v.deafened = true;
        assert_eq!(v.toggle_deafen(), (true, false));
    }

    #[test]
    fn display_name_falls_back_to_the_truncated_key() {
        let mut s = AppState::empty();
        let known = "a".repeat(64);
        let stranger = "b".repeat(64);
        s.members.push(member(&known, true));

        assert_eq!(s.display_name(&known), format!("u-{known}"));
        assert_eq!(
            s.display_name(&stranger),
            crate::identity::truncate_pubkey(&stranger)
        );
    }

    #[test]
    fn display_name_resolves_the_logged_in_user_without_a_roster() {
        let mut s = AppState::empty();
        let me = "c".repeat(64);
        s.self_user = Some(user(&me));

        assert!(s.members.is_empty());
        assert_eq!(s.display_name(&me), format!("u-{me}"));
    }
}
