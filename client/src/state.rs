use std::collections::{BTreeMap, HashMap, HashSet};

use dioxus::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::host::HostInfo;
use crate::protocol::{
    BotInstall, Channel, ClientMessage, Guild, GuildSummary, Id, Member, Message, Permission,
    Profile, Role, User, VoiceState,
};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionMode {
    Remote {
        server_url: String,
    },
    SelfHost {
        allow_lan: bool,
        #[serde(default)]
        manual_ip: Option<std::net::IpAddr>,
        rendezvous_url: Option<String>,
        publish_name: Option<String>,
        description: Option<String>,
        publish_public: bool,
        #[serde(default)]
        location: Option<crate::protocol::rendezvous::GeoPoint>,
    },
    ByCode {
        rendezvous_url: String,
        code: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionParams {
    pub mode: SessionMode,
    pub identity: crate::identity::Identity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Loopback,
    Quic,
    QuicRelayed,
    Proxied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRoutes {
    pub gateway: String,
    pub sent: Option<String>,
    pub received: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Reconnecting,
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

/// The ring round a person's avatar: green while they talk, yellow or red when
/// their connection is weak or gone, which stays visible between words.
pub fn talk_ring(speaking: bool, health: Option<ConnectionHealth>) -> &'static str {
    match (speaking, health) {
        (true, Some(ConnectionHealth::Lost)) => "ring-2 ring-[var(--danger)]",
        (true, Some(ConnectionHealth::Poor)) => "ring-2 ring-[var(--warn)]",
        (true, _) => "ring-2 ring-[var(--success)]",
        (false, Some(ConnectionHealth::Lost)) => "ring-1 ring-[var(--danger)]",
        (false, Some(ConnectionHealth::Poor)) => "ring-1 ring-[var(--warn)]",
        (false, _) => "",
    }
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

#[derive(Debug, Clone, PartialEq)]
pub struct ScreenShareStats {
    pub outbound: bool,
    pub capture_width: Option<u32>,
    pub capture_height: Option<u32>,
    pub capture_fps: Option<f64>,
    pub capture_processing_ms: Option<f64>,
    pub encode_ms: Option<f64>,
    pub encoded_width: Option<u32>,
    pub encoded_height: Option<u32>,
    pub encoded_fps: Option<f64>,
    pub bitrate_kbps: Option<u32>,
    pub target_bitrate_kbps: Option<u32>,
    pub codec: Option<String>,
    pub codec_implementation: Option<String>,
    pub power_efficient: Option<bool>,
    pub quality_limitation_reason: Option<String>,
    pub frames: Option<u64>,
    pub packets: Option<u64>,
    pub packets_lost: Option<i64>,
    pub jitter_ms: Option<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceSession {
    pub phase: VoicePhase,
    pub channel_id: Option<Id>,
    pub joining_channel_id: Option<Id>,
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
            joining_channel_id: None,
            muted: false,
            deafened: false,
            muted_before_deafen: false,
            speaking: false,
            error: None,
        }
    }
}

impl VoiceSession {
    pub fn join_message(&self, channel_id: Id) -> ClientMessage {
        ClientMessage::JoinVoice {
            channel_id,
            preferences: Some(crate::protocol::VoicePreferences {
                muted: self.muted || self.deafened,
                deafened: self.deafened,
            }),
        }
    }

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
    ImportDiscord,
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
pub struct CommandNote {
    pub invocation_id: Id,
    pub bot_pubkey: String,
    pub channel_id: Id,
    pub content: String,
}

/// Enough to scroll back through a burst of presses, not a history.
pub const MAX_COMMAND_NOTES: usize = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DmInfo {
    pub channel_id: Id,
    pub other_pubkey: String,
}

/// A right-click on someone in a voice channel. Mounted at the workspace root
/// like the soundboard (trap 22), so it carries where it was opened.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceMenu {
    pub pubkey: String,
    pub name: String,
    pub x: f64,
    pub y: f64,
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
    /// The NIP-30 `emoji` tags a DM carried, by message: `:code:` in its text
    /// resolves here, since a DM has no guild to ask. Memory only — relays
    /// replay the tags with the message.
    pub dm_emoji: HashMap<Id, Vec<(String, String)>>,
    /// Seeded from settings and written back by `use_emoji_catalog_persistence`.
    pub emoji_catalog: Vec<crate::settings::CatalogEmoji>,
    pub dm_unread: HashMap<Id, u32>,
    /// Seeded from `ClientSettings::dm_cleared_at`; see it for why a delete is
    /// a watermark. Read on every insert, so replayed history stays hidden.
    pub dm_cleared_at: HashMap<String, i64>,
    /// Seeded from `ClientSettings::dm_read_at`. Read on every insert, so the
    /// history the relays replay at launch does not raise an alert twice.
    pub dm_read_at: HashMap<String, i64>,
    /// Author pubkey → seconds that author's clock runs ahead of ours, as
    /// measured on a live message. A DM carries the time its *sender* wrote on
    /// it, so without this a peer whose clock is a minute out has every reply
    /// filed a minute away from where it belongs. See `note_clock_offset`.
    pub dm_clock_offset: HashMap<String, i64>,
    /// Seeded from `ClientSettings`. Local and personal: muting is never sent
    /// anywhere, and a muted channel is silent for its mentions too — otherwise
    /// the word would mean two things.
    pub muted_channels: HashSet<Id>,
    pub muted_guilds: HashSet<Id>,
    pub nostr_event_ids: HashMap<Id, String>,
    pub dm_delivery: HashMap<Id, crate::nostr::delivery::Delivery>,
    pub dm_call: Option<crate::features::dm_call::CallView>,
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
    /// Self-asserted and never authoritative — see `nostr::xp`. Keyed by the
    /// pubkey that signed it.
    pub global_xp: HashMap<String, crate::nostr::xp::GlobalXp>,
    /// The dial origin of the server this session is on, which is the key the
    /// local experience ledger files this server's total under.
    pub server_origin: Option<String>,
    /// The rendezvous this session used, when it used one at all: to resolve a
    /// join code, to register a self-host, or to relay the connection. A
    /// `Remote` dial to a plain address uses none, and this stays `None`.
    pub rendezvous_url: Option<String>,
    /// Never persisted and never merged into `profiles`: it is only true while
    /// the person holds a socket, and the server clears it when they drop.
    pub activities: HashMap<String, crate::protocol::Activity>,
    pub profile_card: Option<String>,
    pub image_viewer: Option<String>,
    pub guild_dialog: Option<GuildDialog>,
    pub rules_prompt: Option<RulesPrompt>,
    /// Opened from the title bar, rendered by the voice panel that owns the
    /// device signals — the flag is the only thing the two need to share.
    pub audio_settings: bool,
    /// The "where does my data go" panel, opened from the transport chip.
    pub topology_open: bool,
    /// The voice channel whose access dialog is open.
    pub channel_access_open: Option<Id>,
    pub typing: HashMap<Id, HashMap<String, (String, std::time::Instant)>>,
    pub notify_tick: u64,
    /// Its own counter, so a DM can have its own sound. A channel message and
    /// somebody messaging you directly are not the same event.
    pub dm_notify_tick: u64,
    pub screen_token: Option<(String, String)>,
    pub voice_endpoint: Option<String>,
    pub voice_location: Option<String>,
    /// `ws://127.0.0.1:<port>/sfu` while a gateway session is up: the host's
    /// SFU signaling through that session, tried after every direct address.
    pub sfu_tunnel_url: Option<String>,
    pub voice_send_route: Option<crate::connection_routes::MediaRoute>,
    pub voice_receive_route: Option<crate::connection_routes::MediaRoute>,
    pub gateway_route: Option<String>,
    pub peer_routes: HashMap<String, PeerRoutes>,
    pub voice_route_revision: u32,
    pub screen_audio_token: Option<(String, String)>,
    /// Whether the native side is *actually in*, not merely holding a token:
    /// a failed join must hand playback back to the webview, not go silent.
    pub screen_audio_joined: bool,
    pub screen_video_token: Option<(String, String)>,
    pub screen_viewer_token: Option<(String, String)>,
    /// What every LiveKit room of this session hands its ICE agent; a host
    /// behind NAT sends its rendezvous's relay here so its SFU can be reached.
    pub ice_servers: Vec<crate::protocol::IceServer>,
    pub screen_share_target: Option<crate::sysvideo::Target>,
    pub screen_picker: Option<Result<Vec<crate::sysvideo::Source>, String>>,
    pub screen_sharing: bool,
    pub screen_native_audio: bool,
    pub screen_shares: HashMap<Id, Vec<String>>,
    pub screen_viewing: HashSet<String>,
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
    pub mic_gate_level: u32,
    pub mic_level_pre: u32,
    pub noise_cancellation: bool,
    pub denoise_atten_lim_db: u32,
    pub bypass_system_audio_processing: bool,
    pub mic_bypass_error: Option<String>,
    pub voice_bitrate_kbps: u32,
    pub voice_quality: HashMap<String, ConnectionHealth>,
    pub voice_stats: HashMap<String, TrackStats>,
    pub screen_share_stats: Option<ScreenShareStats>,
    pub screen_share_in_stats: Option<ScreenShareStats>,
    pub user_volumes: HashMap<String, u32>,
    pub user_muted: HashSet<String>,
    pub stream_volumes: HashMap<String, u32>,
    pub stream_muted: HashSet<String>,
    pub soundboard_volume: u32,
    pub soundboard_open: bool,
    /// The board shows its volume slider instead of the sounds.
    pub soundboard_adjusting: bool,
    /// The click's x and the toggle's top edge, on screen. The popover is mounted
    /// at the root (trap 22), so it cannot sit beside the button in the DOM.
    pub soundboard_anchor: (f64, f64),
    /// Who pressed what, for the few seconds a voice row shows it.
    pub recent_sounds: HashMap<String, (String, std::time::Instant)>,
    pub voice_menu: Option<VoiceMenu>,
    pub stream_has_audio: crate::stream_audio::Presence,
    pub media_undecryptable: bool,
    pub pending_rekey: bool,
    pub identity: Option<crate::identity::Identity>,
    pub media_keys: HashMap<Id, (u32, [u8; 32])>,
    pub host_info: Option<HostInfo>,
    pub transport: Transport,
    pub integrations: HashMap<Id, Vec<BotInstall>>,
    /// By bot pubkey. What each bot's profile card offers to press.
    pub bot_commands: HashMap<String, Vec<crate::protocol::BotCommand>>,
    /// A bot's private answers: never stored anywhere, gone on reconnect.
    pub command_notes: Vec<CommandNote>,
    pub guild_emojis: HashMap<Id, Vec<crate::protocol::GuildEmoji>>,
    pub guild_sounds: HashMap<Id, Vec<crate::protocol::GuildSound>>,
    pub emoji_images: crate::media_cache::MediaCache,
    /// Address → when it was last asked for. A request the server dropped,
    /// throttled or cut for size is asked again once this is old enough.
    pub emoji_requested: HashMap<String, std::time::Instant>,
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
            dm_emoji: HashMap::new(),
            emoji_catalog: Vec::new(),
            dm_unread: HashMap::new(),
            dm_cleared_at: HashMap::new(),
            dm_clock_offset: HashMap::new(),
            dm_read_at: HashMap::new(),
            muted_channels: HashSet::new(),
            muted_guilds: HashSet::new(),
            nostr_event_ids: HashMap::new(),
            dm_delivery: HashMap::new(),
            dm_call: None,
            contacts: Default::default(),
            nostr_relays_up: std::collections::HashSet::new(),
            nostr_names: HashMap::new(),
            dm_mode: false,
            dm_pane_open: true,
            catalog: Vec::new(),
            catalog_total: 0,
            profiles: HashMap::new(),
            global_xp: HashMap::new(),
            server_origin: None,
            rendezvous_url: None,
            activities: HashMap::new(),
            profile_card: None,
            image_viewer: None,
            guild_dialog: None,
            rules_prompt: None,
            audio_settings: false,
            topology_open: false,
            channel_access_open: None,
            typing: HashMap::new(),
            notify_tick: 0,
            dm_notify_tick: 0,
            screen_token: None,
            voice_endpoint: None,
            sfu_tunnel_url: None,
            voice_location: None,
            voice_send_route: None,
            voice_receive_route: None,
            gateway_route: None,
            peer_routes: HashMap::new(),
            voice_route_revision: 0,
            screen_audio_token: None,
            screen_video_token: None,
            screen_viewer_token: None,
            ice_servers: Vec::new(),
            screen_share_target: None,
            screen_picker: None,
            screen_audio_joined: false,
            screen_sharing: false,
            screen_native_audio: false,
            screen_shares: HashMap::new(),
            screen_viewing: HashSet::new(),
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
            mic_sensitivity: crate::settings::DEFAULT_MIC_SENSITIVITY,
            mic_volume: 100,
            auto_gain_control: true,
            mic_level: 0,
            mic_gate_level: 0,
            mic_level_pre: 0,
            noise_cancellation: true,
            denoise_atten_lim_db: 30,
            bypass_system_audio_processing: true,
            mic_bypass_error: None,
            voice_bitrate_kbps: 64,
            voice_quality: HashMap::new(),
            voice_stats: HashMap::new(),
            screen_share_stats: None,
            screen_share_in_stats: None,
            user_volumes: HashMap::new(),
            user_muted: HashSet::new(),
            stream_volumes: HashMap::new(),
            stream_muted: HashSet::new(),
            soundboard_volume: crate::settings::DEFAULT_SOUNDBOARD_VOLUME as u32,
            soundboard_open: false,
            soundboard_adjusting: false,
            soundboard_anchor: (0.0, 0.0),
            recent_sounds: HashMap::new(),
            voice_menu: None,
            stream_has_audio: crate::stream_audio::Presence::default(),
            media_undecryptable: false,
            pending_rekey: false,
            identity: None,
            media_keys: HashMap::new(),
            host_info: None,
            transport: Transport::Loopback,
            integrations: HashMap::new(),
            bot_commands: HashMap::new(),
            command_notes: Vec::new(),
            guild_emojis: HashMap::new(),
            guild_sounds: HashMap::new(),
            emoji_images: Default::default(),
            emoji_requested: HashMap::new(),
            roles: HashMap::new(),
            bans: HashMap::new(),
            invites: HashMap::new(),
            error_toast: None,
            is_operator: false,
            audit_logs: HashMap::new(),
        }
    }

    pub fn set_voice_endpoint(&mut self, url: String) {
        self.voice_endpoint = Some(url.clone());
        for token in [
            &mut self.screen_token,
            &mut self.screen_audio_token,
            &mut self.screen_video_token,
            &mut self.screen_viewer_token,
        ]
        .into_iter()
        .flatten()
        {
            token.0 = url.clone();
        }
    }

    /// Clear locally before the server echo: retained tokens keep webview rooms alive,
    /// and the process-wide media key must not survive a call.
    pub fn end_voice_locally(&mut self) {
        self.voice.phase = VoicePhase::Idle;
        self.voice.channel_id = None;
        self.media_keys.clear();
        self.voice.error = None;
        self.screen_token = None;
        self.voice_endpoint = None;
        self.voice_location = None;
        self.voice_send_route = None;
        self.voice_receive_route = None;
        self.peer_routes.clear();
        self.screen_audio_token = None;
        self.screen_video_token = None;
        self.screen_viewer_token = None;
        self.screen_share_target = None;
        self.screen_sharing = false;
        self.screen_viewing.clear();
        self.camera_on = false;
        self.camera_starting = false;
        self.cameras_watching.clear();
    }

    pub fn clear_server_session(&mut self) {
        self.voice_route_revision = 0;
        self.status = ConnectionStatus::Disconnected;
        self.self_user = None;
        self.guilds.clear();
        self.channels.clear();
        self.members.clear();
        self.voice_states.clear();
        self.end_voice_locally();
        self.voice_session_epoch = self.voice_session_epoch.wrapping_add(1);
        self.selected_guild = None;
        self.selected_channel = self.selected_channel.filter(|id| self.dm_of(*id).is_some());
        self.dm_mode = self.selected_channel.is_some();
        self.dm_pane_open = false;
        let dm_channels: HashSet<_> = self.dms.iter().map(|dm| dm.channel_id).collect();
        self.messages.retain(|id, _| dm_channels.contains(id));
        self.catalog.clear();
        self.catalog_total = 0;
        self.profiles.clear();
        self.activities.clear();
        self.server_origin = None;
        self.sfu_tunnel_url = None;
        self.rendezvous_url = None;
        self.host_info = None;
        self.transport = Transport::Loopback;
        self.gateway_route = None;
        self.guild_dialog = None;
        self.rules_prompt = None;
        self.audio_settings = false;
        self.topology_open = false;
        self.channel_access_open = None;
        self.typing.clear();
        self.replying_to = None;
        self.profile_card = None;
        self.image_viewer = None;
        self.media_undecryptable = false;
        self.pending_rekey = false;
        self.screen_shares.clear();
        self.screen_picker = None;
        self.screen_audio_joined = false;
        self.stream_has_audio = Default::default();
        self.soundboard_open = false;
        self.soundboard_adjusting = false;
        self.recent_sounds.clear();
        self.voice_menu = None;
        self.voice_quality.clear();
        self.voice_stats.clear();
        self.screen_share_stats = None;
        self.screen_share_in_stats = None;
        self.integrations.clear();
        self.bot_commands.clear();
        self.command_notes.clear();
        self.guild_emojis.clear();
        self.guild_sounds.clear();
        self.emoji_requested.clear();
        self.roles.clear();
        self.bans.clear();
        self.invites.clear();
        self.audit_logs.clear();
        self.is_operator = false;
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

    /// Replaces the catalog's view of one guild. Returns whether it moved.
    pub fn remember_emojis(
        &mut self,
        guild_id: Id,
        emojis: &[crate::protocol::GuildEmoji],
    ) -> bool {
        let guild_name = self
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .map(|g| g.name.clone())
            .unwrap_or_default();
        let mut next: Vec<crate::settings::CatalogEmoji> = self
            .emoji_catalog
            .iter()
            .filter(|e| e.guild_id != guild_id)
            .cloned()
            .collect();
        next.extend(emojis.iter().map(|e| crate::settings::CatalogEmoji {
            shortcode: e.shortcode.clone(),
            image: e.image.clone(),
            guild_id,
            guild_name: guild_name.clone(),
        }));
        if next == self.emoji_catalog {
            return false;
        }
        self.emoji_catalog = next;
        true
    }

    pub fn emojis_of(&self, guild_id: Id) -> &[crate::protocol::GuildEmoji] {
        self.guild_emojis
            .get(&guild_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn sounds_of(&self, guild_id: Id) -> &[crate::protocol::GuildSound] {
        self.guild_sounds
            .get(&guild_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// The call the server has us in. It leads `voice.channel_id`, which waits
    /// for a token to be minted, so keys for the new call are not refused.
    pub fn server_voice_channel(&self) -> Option<Id> {
        let me = self.self_user.as_ref()?;
        self.voice_states
            .iter()
            .find(|v| v.user_pubkey == me.pubkey)?
            .channel_id
    }

    /// The guild of the voice channel we are in, whose sounds the board offers.
    pub fn voice_guild(&self) -> Option<Id> {
        let channel = self.voice.channel_id?;
        self.channels
            .iter()
            .find(|c| c.id == channel)
            .map(|c| c.guild_id)
    }

    pub fn roles_of(&self, guild_id: Id) -> &[Role] {
        self.roles
            .get(&guild_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn screen_viewer_names(&self, sharer: &str) -> Vec<String> {
        let Some(channel) = self
            .voice_states
            .iter()
            .find(|vs| vs.user_pubkey == sharer && vs.screen_sharing)
            .and_then(|vs| vs.channel_id)
        else {
            return Vec::new();
        };
        let mut viewers: Vec<_> = self
            .voice_states
            .iter()
            .filter(|vs| {
                vs.channel_id == Some(channel)
                    && vs.user_pubkey != sharer
                    && vs.screen_watching.iter().any(|pk| pk == sharer)
            })
            .map(|vs| vs.user_pubkey.as_str())
            .collect();
        viewers.sort_unstable();
        viewers.dedup();
        let mut names: Vec<_> = viewers
            .into_iter()
            .map(|pk| self.display_name(pk))
            .collect();
        names.sort();
        names
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

    /// What we have earned on *this* server, across every guild on it. The
    /// server stamps `Member::xp` per guild and knows no other server, so this
    /// sum is the largest true number available here.
    pub fn my_server_xp(&self) -> u64 {
        let Some(me) = self.self_user.as_ref().map(|u| u.pubkey.as_str()) else {
            return 0;
        };
        self.members
            .iter()
            .filter(|m| m.user.pubkey == me)
            .map(|m| m.xp)
            .sum()
    }

    pub fn global_xp_of(&self, pubkey: &str) -> Option<crate::nostr::xp::GlobalXp> {
        self.global_xp.get(pubkey).copied()
    }

    pub fn activity_of(&self, pubkey: &str) -> Option<&crate::protocol::Activity> {
        self.activities.get(pubkey)
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
        let index = held.partition_point(|m| m.created_at <= message.created_at);
        held.insert(index, message);
        true
    }

    /// What to subtract from `author`'s timestamps to put them on our clock.
    pub fn clock_offset(&self, author: &str) -> i64 {
        self.dm_clock_offset.get(author).copied().unwrap_or(0)
    }

    /// Records a fresh estimate of `author`'s clock offset and re-times what is
    /// already held, so a correction learned from one live message also fixes
    /// the history the relays replayed before it. Returns whether it changed.
    ///
    /// The correction is a pure shift, so re-timing is a shift too: nothing has
    /// to remember the raw timestamp it arrived with.
    pub fn note_clock_offset(&mut self, author: &str, offset: i64) -> bool {
        let delta = offset - self.clock_offset(author);
        if delta == 0 {
            return false;
        }
        self.dm_clock_offset.insert(author.to_string(), offset);
        let shift = chrono::TimeDelta::seconds(delta);
        // Conversations only. A guild message carries the *server's* time, and
        // a member who also DMs us must not have their channel messages moved.
        let conversations: Vec<Id> = self.dms.iter().map(|d| d.channel_id).collect();
        for cid in conversations {
            let Some(held) = self.messages.get_mut(&cid) else {
                continue;
            };
            let mut moved = false;
            for m in held.iter_mut().filter(|m| m.author.pubkey == author) {
                m.created_at -= shift;
                moved = true;
            }
            if moved {
                held.sort_by_key(|m| m.created_at);
            }
        }
        true
    }

    /// Merges a fetched page into what is already held.
    pub fn merge_history(&mut self, channel_id: Id, page: Vec<Message>) {
        let held = self.messages.entry(channel_id).or_default();
        let mut seen: HashSet<Id> = held.iter().map(|m| m.id).collect();
        let mut incoming = Vec::with_capacity(page.len());
        for m in page {
            // Recorded as we go rather than sampled once: a page that repeats
            // an id would otherwise be believed twice.
            if seen.insert(m.id) {
                incoming.push(m);
            }
        }
        if incoming.is_empty() {
            return;
        }
        incoming.sort_by_key(|m| m.created_at);
        if held
            .last()
            .zip(incoming.first())
            .is_none_or(|(last, first)| last.created_at <= first.created_at)
        {
            held.extend(incoming);
            return;
        }
        let mut merged = Vec::with_capacity(held.len() + incoming.len());
        let mut old = std::mem::take(held).into_iter().peekable();
        for message in incoming {
            while old
                .peek()
                .is_some_and(|m| m.created_at <= message.created_at)
            {
                if let Some(previous) = old.next() {
                    merged.push(previous);
                }
            }
            merged.push(message);
        }
        merged.extend(old);
        *held = merged;
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
                self.dm_notify_tick = self.dm_notify_tick.wrapping_add(1);
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

    pub fn leveling_of(&self, guild_id: Id) -> crate::protocol::Leveling {
        self.guilds
            .iter()
            .find(|g| g.id == guild_id)
            .map(|g| g.leveling.clone())
            .unwrap_or_default()
    }

    /// Ordered as the guild asked. Online still leads either way: the panel
    /// splits by presence anyway, so this only settles the order inside a
    /// group, and a rank is a poor reason to bury everyone who is here.
    pub fn members_of(&self, guild_id: Id) -> Vec<&Member> {
        let mut v: Vec<&Member> = self
            .members
            .iter()
            .filter(|m| m.guild_id == guild_id)
            .collect();
        let by_name = |a: &&Member, b: &&Member| {
            a.user
                .username
                .to_lowercase()
                .cmp(&b.user.username.to_lowercase())
        };
        match self.leveling_of(guild_id).member_sort {
            crate::protocol::MemberSort::Name => {
                v.sort_by(|a, b| b.online.cmp(&a.online).then_with(|| by_name(a, b)));
            }
            crate::protocol::MemberSort::Level => {
                v.sort_by(|a, b| {
                    b.online
                        .cmp(&a.online)
                        .then_with(|| b.xp.cmp(&a.xp))
                        .then_with(|| by_name(a, b))
                });
            }
            crate::protocol::MemberSort::Role => {
                // The id breaks a tie in position, so a role's members stay together.
                let rank = |m: &Member| {
                    self.top_role(guild_id, m)
                        .map_or((u32::MAX, None), |r| (r.position, Some(r.id)))
                };
                v.sort_by(|a, b| {
                    b.online
                        .cmp(&a.online)
                        .then_with(|| rank(a).cmp(&rank(b)))
                        .then_with(|| by_name(a, b))
                });
            }
        }
        v
    }

    /// The highest role a member holds: the lowest position.
    pub fn top_role(&self, guild_id: Id, member: &Member) -> Option<&Role> {
        self.roles
            .get(&guild_id)?
            .iter()
            .filter(|r| member.roles.contains(&r.id))
            .min_by_key(|r| r.position)
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
        if self.0.send(msg).is_err() {
            tracing::warn!("gateway send dropped: the session loop is gone");
        }
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

// Read marks change on several surfaces; one account-level writer prevents drift.
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

/// Writes the emoji catalog back to disk when a session changed it.
pub fn use_emoji_catalog_persistence(state: Signal<AppState>) {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let catalog = use_memo(move || state.read().emoji_catalog.clone());
    use_effect(move || {
        let catalog = catalog();
        // `peek`: writing back what this effect subscribes to is a loop.
        if settings.peek().emoji_catalog == catalog {
            return;
        }
        settings.write().emoji_catalog = catalog;
        crate::settings::save(&settings.peek());
    });
}

/// Writes the learned clock offsets back to disk.
///
/// Separate from the read marks because it moves for a different reason and
/// almost never: an offset is learned once per peer whose clock is out.
pub fn use_dm_clock_persistence(state: Signal<AppState>) {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let offsets = use_memo(move || {
        let mut v: Vec<(String, i64)> = state
            .read()
            .dm_clock_offset
            .iter()
            .map(|(author, off)| (author.clone(), *off))
            .collect();
        v.sort();
        v
    });
    use_effect(move || {
        let offsets = offsets();
        if offsets.is_empty() {
            return;
        }
        // `peek`, not `read`: writing back what this effect subscribes to is a
        // loop.
        let mut next = settings.peek().clone();
        let changed = offsets.into_iter().fold(false, |acc, (author, off)| {
            next.set_clock_offset(&author, off) || acc
        });
        if changed {
            settings.set(next.clone());
            crate::settings::save(&next);
        }
    });
}

/// Per-person and per-stream playback, written back as it moves. One hook, as
/// for the read marks: the sliders and mute buttons live in three components.
pub fn use_volume_persistence(state: Signal<AppState>) {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let chosen = use_memo(move || {
        let s = state.read();
        let sorted = |m: &HashMap<String, u32>| {
            let mut v: Vec<(String, u32)> = m.iter().map(|(k, v)| (k.clone(), *v)).collect();
            v.sort();
            v
        };
        let listed = |m: &HashSet<String>| {
            let mut v: Vec<String> = m.iter().cloned().collect();
            v.sort();
            v
        };
        (
            sorted(&s.user_volumes),
            listed(&s.user_muted),
            sorted(&s.stream_volumes),
            listed(&s.stream_muted),
        )
    });
    use_effect(move || {
        let (users, muted, streams, streams_muted) = chosen();
        // `peek`, not `read`: writing back what this effect subscribes to is a loop.
        let now = settings.peek().clone();
        if now.user_volumes == users
            && now.user_muted == muted
            && now.stream_volumes == streams
            && now.stream_muted == streams_muted
        {
            return;
        }
        let mut next = now;
        next.user_volumes = users;
        next.user_muted = muted;
        next.stream_volumes = streams;
        next.stream_muted = streams_muted;
        settings.set(next.clone());
        crate::settings::save(&next);
    });
}

pub fn use_gateway() -> GatewayTx {
    use_context::<GatewayTx>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Member, Profile, User};

    #[test]
    fn stream_viewers_exclude_other_channels_self_and_duplicate_identities() {
        let mut state = AppState::empty();
        let channel = Id::new_v4();
        let guild = Id::new_v4();
        let sharer = VoiceState {
            user_pubkey: "sharer".into(),
            guild_id: guild,
            channel_id: Some(channel),
            muted: false,
            deafened: false,
            speaking: false,
            camera_on: false,
            screen_sharing: true,
            screen_watching: vec!["sharer".into()],
        };
        let viewer = VoiceState {
            user_pubkey: "viewer".into(),
            screen_sharing: false,
            ..sharer.clone()
        };
        let other = VoiceState {
            user_pubkey: "other".into(),
            channel_id: Some(Id::new_v4()),
            ..viewer.clone()
        };
        state.voice_states = vec![sharer, viewer.clone(), viewer, other];
        assert_eq!(
            state.screen_viewer_names("sharer"),
            vec![state.display_name("viewer")]
        );
        state.voice_states[0].screen_sharing = false;
        assert!(state.screen_viewer_names("sharer").is_empty());
    }
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

    fn dm_at(s: &mut AppState, peer: &str, author: &str, secs: i64) -> Id {
        let cid = crate::nostr::service::conversation_id(peer);
        if !s.dms.iter().any(|d| d.channel_id == cid) {
            s.dms.push(DmInfo {
                channel_id: cid,
                other_pubkey: peer.to_string(),
            });
        }
        let id = Id::new_v4();
        let mut m = at(id, cid, secs);
        m.author = user(author);
        s.insert_message(cid, m);
        id
    }

    /// The bug this exists for: a peer whose clock is a quarter of an hour
    /// behind has every reply filed above the question it answers, and the
    /// correction has to reach the history already on screen, not only what
    /// arrives next.
    #[test]
    fn correcting_a_clock_re_times_the_conversation_already_held() {
        let peer = "b".repeat(64);
        let me = "a".repeat(64);
        let cid = crate::nostr::service::conversation_id(&peer);
        let mut s = AppState::empty();

        let question = dm_at(&mut s, &peer, &me, 2_000);
        let answer = dm_at(&mut s, &peer, &peer, 2_000 - 900 + 10);

        let order: Vec<Id> = s.messages[&cid].iter().map(|m| m.id).collect();
        assert_eq!(
            order,
            vec![answer, question],
            "the skew is what we start from"
        );

        assert!(s.note_clock_offset(&peer, -900));
        let order: Vec<Id> = s.messages[&cid].iter().map(|m| m.id).collect();
        assert_eq!(order, vec![question, answer]);
        assert_eq!(
            s.messages[&cid][1].created_at.timestamp(),
            2_010,
            "their stamp is put on our clock, not merely reordered"
        );
    }

    /// A member of a guild who also DMs us must not have their channel
    /// messages moved: those carry the server's time, not theirs.
    #[test]
    fn a_clock_correction_stops_at_the_conversation() {
        let peer = "b".repeat(64);
        let guild_channel = Id::new_v4();
        let mut s = AppState::empty();
        dm_at(&mut s, &peer, &peer, 1_000);
        let mut in_guild = at(Id::new_v4(), guild_channel, 1_000);
        in_guild.author = user(&peer);
        s.insert_message(guild_channel, in_guild);

        s.note_clock_offset(&peer, -900);

        assert_eq!(s.messages[&guild_channel][0].created_at.timestamp(), 1_000);
    }

    /// Nothing to do is not a change, or the settings file would be rewritten
    /// on every message.
    #[test]
    fn an_unchanged_offset_reports_nothing() {
        let mut s = AppState::empty();
        assert!(s.note_clock_offset("abcd", -900));
        assert!(!s.note_clock_offset("abcd", -900));
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

    #[test]
    fn history_merge_preserves_existing_edits_and_equal_timestamp_order() {
        let mut s = AppState::empty();
        let cid = Id::new_v4();
        let existing = Id::new_v4();
        let newest = Id::new_v4();
        let same_time = Id::new_v4();
        let oldest = Id::new_v4();
        let mut edited = at(existing, cid, 200);
        edited.content = "edited locally".into();
        s.insert_message(cid, edited);
        s.insert_message(cid, at(newest, cid, 400));
        s.merge_history(
            cid,
            vec![
                at(same_time, cid, 200),
                at(oldest, cid, 100),
                at(existing, cid, 200),
            ],
        );
        assert_eq!(
            s.messages[&cid].iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![oldest, existing, same_time, newest]
        );
        assert_eq!(s.messages[&cid][1].content, "edited locally");
        let tail = Id::new_v4();
        s.merge_history(cid, vec![at(tail, cid, 500)]);
        assert_eq!(s.messages[&cid].last().unwrap().id, tail);
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
            access: None,
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
        assert_eq!(s.dm_notify_tick, 1);
        assert_eq!(
            s.notify_tick, 0,
            "a DM rings the DM bell, not the other one"
        );

        s.mark_dm_read(cid);
        let after_reading = s.dm_notify_tick;
        s.note_dm_arrival(cid, peer, 100);
        assert!(s.dm_unread.is_empty());
        assert_eq!(s.dm_notify_tick, after_reading, "the replay rang again");
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

    /// The key in use is global to the process; a key kept for a call we left
    /// is one the bridge would never apply again on the way back.
    #[test]
    fn leaving_a_call_drops_its_media_key_and_everything_published() {
        let mut s = AppState::empty();
        let channel = uuid::Uuid::from_u128(7);
        s.voice.channel_id = Some(channel);
        s.voice.phase = VoicePhase::Connected;
        s.media_keys.insert(channel, (3, [9; 32]));
        s.screen_sharing = true;
        s.camera_on = true;
        s.screen_token = Some(("url".into(), "tok".into()));
        s.screen_viewer_token = Some(("url".into(), "viewer".into()));
        s.end_voice_locally();
        assert!(s.media_keys.is_empty());
        assert!(!s.screen_sharing && !s.camera_on && s.screen_token.is_none());
        assert!(s.screen_viewer_token.is_none());
        assert_eq!(s.voice.channel_id, None);
    }

    #[test]
    fn by_role_the_top_role_leads_and_offline_still_comes_last() {
        let gid = uuid::Uuid::nil();
        let mut s = AppState::empty();
        let mut guild: crate::protocol::Guild = serde_json::from_value(serde_json::json!({
            "id": gid, "name": "g", "icon": null
        }))
        .unwrap();
        guild.leveling.member_sort = crate::protocol::MemberSort::Role;
        s.guilds.push(guild);
        let role = |n: u128, name: &str, position: u32| Role {
            id: uuid::Uuid::from_u128(n),
            guild_id: gid,
            name: name.into(),
            color: None,
            permissions: Vec::new(),
            position,
        };
        s.roles
            .insert(gid, vec![role(1, "Admin", 0), role(2, "Mod", 1)]);
        let with = |pk: &str, online: bool, roles: &[u128]| {
            let mut m = member(pk, online);
            m.roles = roles.iter().map(|n| uuid::Uuid::from_u128(*n)).collect();
            m
        };
        s.members = vec![
            with("a-plain", true, &[]),
            with("b-mod", true, &[2]),
            with("c-both", true, &[2, 1]),
            with("d-admin-away", false, &[1]),
        ];
        let order: Vec<&str> = s
            .members_of(gid)
            .iter()
            .map(|m| m.user.pubkey.as_str())
            .collect();
        assert_eq!(order, ["c-both", "b-mod", "a-plain", "d-admin-away"]);
    }

    #[test]
    fn talking_lights_green_and_a_bad_connection_shows_between_words() {
        use ConnectionHealth::*;
        assert_eq!(talk_ring(true, None), "ring-2 ring-[var(--success)]");
        assert_eq!(
            talk_ring(true, Some(Excellent)),
            "ring-2 ring-[var(--success)]"
        );
        assert_eq!(talk_ring(true, Some(Poor)), "ring-2 ring-[var(--warn)]");
        assert_eq!(talk_ring(false, Some(Poor)), "ring-1 ring-[var(--warn)]");
        assert_eq!(talk_ring(false, Some(Lost)), "ring-1 ring-[var(--danger)]");
        assert_eq!(talk_ring(false, Some(Good)), "", "silent and fine is plain");
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
    fn joining_voice_carries_the_local_mute_and_deafen_choices() {
        let channel_id = Id::new_v4();
        for muted in [false, true] {
            for deafened in [false, true] {
                let voice = VoiceSession {
                    muted,
                    deafened,
                    ..VoiceSession::default()
                };
                match voice.join_message(channel_id) {
                    ClientMessage::JoinVoice {
                        channel_id: requested,
                        preferences: Some(prefs),
                    } => {
                        assert_eq!(requested, channel_id);
                        assert_eq!(prefs.muted, muted || deafened);
                        assert_eq!(prefs.deafened, deafened);
                    }
                    other => panic!("unexpected join: {other:?}"),
                }
            }
        }
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
