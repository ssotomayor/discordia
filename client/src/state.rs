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
    Private,
    Direct,
    Relayed,
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
    pub other: User,
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
    pub nostr_event_ids: HashMap<Id, String>,
    pub contacts: crate::nostr::nip02::ContactList,
    pub nostr_relays_up: std::collections::HashSet<String>,
    pub dm_mode: bool,
    pub catalog: Vec<GuildSummary>,
    pub catalog_total: u32,
    pub profiles: HashMap<String, Profile>,
    pub profile_card: Option<String>,
    pub image_viewer: Option<String>,
    pub guild_dialog: Option<GuildDialog>,
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
            nostr_event_ids: HashMap::new(),
            contacts: Default::default(),
            nostr_relays_up: std::collections::HashSet::new(),
            dm_mode: false,
            catalog: Vec::new(),
            catalog_total: 0,
            profiles: HashMap::new(),
            profile_card: None,
            image_viewer: None,
            guild_dialog: None,
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
        self.profiles.get(pubkey).and_then(|p| p.avatar.as_deref())
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

    pub fn display_name(&self, pubkey: &str) -> String {
        self.user_of(pubkey)
            .map(|u| u.username.clone())
            .unwrap_or_else(|| crate::identity::truncate_pubkey(pubkey))
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

pub fn use_gateway() -> GatewayTx {
    use_context::<GatewayTx>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Member, Profile, User};

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
