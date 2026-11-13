use std::collections::{BTreeMap, HashMap, HashSet};

use dioxus::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::host::HostInfo;
pub use crate::local_camera::CameraEvent;
pub use crate::local_share::ShareEvent;
use crate::protocol::{
    BotInstall, Channel, ClientMessage, Guild, GuildSummary, Id, Member, Message, Permission,
    Profile, Role, User, VoiceState,
};
pub use crate::voice_session::{CallState, MuteChange, VoiceEvent, VoicePhase, VoiceSession};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerApps {
    pub policy: crate::protocol::AppsPolicy,
    pub approved: Vec<String>,
    pub banned: Vec<String>,
    pub installs: Vec<crate::protocol::ServerAppInstall>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    Connecting,
    Reconnecting,
    Ready,
    Disconnected,
}

/// Whether this account is in a server session, for state that outlives one
/// (`AppState::server_session`). `App::session` is what mounts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerSession {
    /// Home, or a session whose gateway task has ended.
    Absent,
    /// Dialling, or redialling one that was live; nothing of the server is current.
    Connecting,
    /// The server let us in and its Ready is applied.
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionHealth {
    Excellent,
    Good,
    Poor,
    Lost,
}

/// The ring round a person's avatar: green while they talk.
pub fn talk_ring(speaking: bool) -> &'static str {
    if speaking {
        "ring-2 ring-[var(--success)]"
    } else {
        ""
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackStats {
    Unavailable,
    Inbound {
        loss_pct: f32,
        recent_loss_pct: Option<f32>,
        discarded_packets: Option<u64>,
        concealed_pct: Option<f32>,
        jitter_ms: f32,
        buffer_ms: f32,
        concealment_events: u64,
    },
    Outbound {
        bitrate_kbps: Option<u32>,
        packets_per_sec: Option<u32>,
        target_kbps: u32,
        loss_pct: Option<f32>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoReceptionStats {
    pub decode_ms: Option<f64>,
    pub dropped_pct: Option<f64>,
    pub render_dropped_pct: Option<f64>,
    pub loss_pct: Option<f64>,
    pub freezes: Option<f64>,
    pub frozen_ms: Option<f64>,
    pub buffer_ms: Option<f64>,
    pub assembly_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(test, derive(Default))]
pub struct ScreenShareStats {
    pub outbound: bool,
    pub capture_width: Option<u32>,
    pub capture_height: Option<u32>,
    pub capture_fps: Option<f64>,
    pub capture_processing_ms: Option<f64>,
    pub capture_conversion_ms: Option<f64>,
    pub capture_copy_ms: Option<f64>,
    pub capture_callbacks_fps: Option<f64>,
    pub capture_paced_out: Option<u64>,
    pub adapter_dropped_frames: Option<u64>,
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
    pub device_limited: bool,
    pub frames: Option<u64>,
    pub packets: Option<u64>,
    pub packets_lost: Option<i64>,
    pub jitter_ms: Option<f64>,
    pub reception: Option<VideoReceptionStats>,
    pub error: Option<String>,
}

/// A bot command whose fields are being filled in (`bot_commands::CommandDialog`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandForm {
    pub bot_pubkey: String,
    pub command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuildDialog {
    Accent(Id),
    Settings(Id),
    Integrations(Id),
    Roles(Id),
    /// Guild settings opened on its Soundboard tab.
    Sounds(Id),
    Invite(Id),
    VoiceInvite(Id),
    AddGuild,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingVoiceInvite {
    pub guild_id: Id,
    pub channel_id: Id,
    pub code: Option<String>,
    pub expires_at: Option<i64>,
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
    pub author_pubkey: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDmEdit {
    pub author: String,
    pub rumor_id: String,
    pub content: String,
    pub created_at: i64,
    pub emoji: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppBadge {
    None,
    Unread,
    Count(u32),
}

pub use crate::dm_standing::{DmChoice, DmStanding};

/// How a conversation's unread count may change; see `AppState::change_dm_unread`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadChange {
    Arrived,
    Read,
    Settle,
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
    /// Not `messages.contains_key`: a live message makes an entry for a channel
    /// never opened, and that then read as history already fetched.
    pub history_loaded: HashSet<Id>,
    /// Bumped when another device's profile is adopted, so the connected
    /// server gets it too; `net` sends the account cache only on connect.
    pub profile_synced_at: i64,
    pub account_pictures: Option<(String, crate::profile::Pictures)>,
    pub account_dnd: Option<(String, bool)>,
    pub message_search: Option<(Id, Id, Vec<Message>)>,
    /// A search result to bring into view: channel, message, its time in ms.
    pub message_jump: Option<(Id, Id, i64)>,
    pub file_upload_result: Option<(Id, Option<String>)>,
    pub server_chat_tools: bool,
    pub server_image_previews: bool,
    pub server_progressive_image_uploads: bool,
    pub guild_file_policies: BTreeMap<Id, crate::protocol::FilePolicy>,
    pub guild_stickers: BTreeMap<Id, Vec<crate::protocol::GuildEmoji>>,
    pub voice_states: Vec<VoiceState>,
    pub voice: VoiceSession,
    pub voice_session_epoch: u64,
    pub selected_guild: Option<Id>,
    pub selected_channel: Option<Id>,
    pub last_text_channels: Vec<crate::channel_selection::RememberedTextChannel>,
    pub dms: Vec<DmInfo>,
    /// The NIP-30 `emoji` tags a DM carried, by message: `:code:` in its text
    /// resolves here, since a DM has no guild to ask. Memory only — relays
    /// replay the tags with the message.
    pub dm_emoji: HashMap<Id, Vec<(String, String)>>,
    /// Seeded from settings and written back by `use_saved_emoji_persistence`.
    pub saved_emoji: Vec<crate::settings::SavedEmoji>,
    /// Written only by `change_dm_unread`, read through `dm_unread`.
    dm_unread: HashMap<Id, u32>,
    pub channel_unread: HashSet<Id>,
    /// Messages naming us since the channel was last read; counted live only,
    /// since Ready carries one timestamp per channel and no content.
    pub channel_mentions: HashMap<Id, u32>,
    /// Seeded from `ClientSettings::channel_read_at`; see there.
    pub channel_read_at: BTreeMap<String, BTreeMap<Id, i64>>,
    /// Newest message time (ms) per channel on this server, so opening one can
    /// move its read mark before its history arrives.
    pub channel_latest: HashMap<Id, i64>,
    /// Seeded from `ClientSettings::dm_cleared_at`; see it for why a delete is
    /// a watermark. Read on every insert, so replayed history stays hidden.
    pub dm_cleared_at: HashMap<String, i64>,
    /// Seeded from `ClientSettings::dm_read_at`. Read on every insert, so the
    /// history the relays replay at launch does not raise an alert twice.
    pub dm_read_at: HashMap<String, i64>,
    pub read_marks_revision: u64,
    /// The read mark a conversation had when it was last opened unread, in ms:
    /// marking it read moves the mark, and the "New messages" line needs the old one.
    pub unread_from: Option<(Id, i64)>,
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
    pub dm_deleted_ids: HashSet<String>,
    pub pending_dm_edits: HashMap<(String, String), PendingDmEdit>,
    pub dm_edit_versions: HashMap<Id, (i64, String)>,
    pub pending_dm_deletions: HashSet<(String, String)>,
    pub dm_delivery: HashMap<Id, crate::nostr::delivery::Delivery>,
    /// `nostr::outbox::Outbox::pending`, for the composer's cap.
    pub dm_pending: usize,
    pub dm_images: HashMap<Id, crate::nostr::dm_image::Image>,
    pub dm_call_history: HashMap<Id, crate::nostr::call_history::Entry>,
    /// Edits shown before the server answered; its `MessageUpdate` settles each.
    pub pending_edits: HashSet<Id>,
    pub dm_call: Option<crate::features::dm_call::CallView>,
    pub contacts: crate::nostr::nip02::ContactList,
    /// Seeded from `ClientSettings::dm_blocked` until our kind 10000 arrives.
    pub mutes: crate::nostr::nip51::MuteList,
    /// Seeded from `ClientSettings`; written back by `use_dm_standing_persistence`.
    /// Changed only by `choose_dm`.
    dm_choices: HashMap<String, DmChoice>,
    pub nostr_relays_up: std::collections::HashSet<String>,
    /// Names peers published for themselves (kind 0), by pubkey, each with the
    /// `created_at` it came with. Kept because kind 0 is replaceable and the
    /// pool dedupes by event id: a rename is a new id, so the old copy still
    /// arrives, and last-writer-wins would show whichever relay was slower.
    pub nostr_names: HashMap<String, (String, i64)>,
    pub dm_mode: bool,
    /// Whether the surface holding the selected conversation is on screen. The
    /// home drawer closes over a DM that stays selected, and a message arriving
    /// behind it is unread however selected it is. Written only by `use_dm_pane`.
    pub dm_pane_open: bool,
    /// Android's resume state; on desktop, whether any of our windows has focus.
    pub app_foreground: bool,
    pub dm_notification_open: Option<Id>,
    pub catalog: Vec<GuildSummary>,
    pub catalog_total: u32,
    pub catalog_revision: u64,
    pub guild_join_confirmation: Option<(u64, Id)>,
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
    /// The `quic://` a join code resolved to, so an invite can carry an address
    /// that works without the friend knowing this rendezvous.
    pub resolved_share: Option<String>,
    /// Never persisted and never merged into `profiles`: it is only true while
    /// the person holds a socket, and the server clears it when they drop.
    pub activities: HashMap<String, crate::protocol::Activity>,
    pub profile_card: Option<String>,
    pub command_form: Option<CommandForm>,
    pub image_viewer: Option<crate::features::image_viewer::ImageView>,
    pub guild_dialog: Option<GuildDialog>,
    pub rules_prompt: Option<RulesPrompt>,
    /// Opened from the title bar, rendered by the voice panel that owns the
    /// device signals — the flag is the only thing the two need to share.
    pub audio_settings: bool,
    /// Account-level, so it survives moving between Home and a server.
    pub agents_open: bool,
    pub server_settings_open: bool,
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
    pub screen_share: crate::local_share::ScreenShare,
    pub screen_shares: HashMap<Id, Vec<String>>,
    pub screen_viewing: crate::stream_placement::StreamWatches,
    pub replying_to: Option<ReplyDraft>,
    pub screen_capture_available: bool,

    pub camera: crate::local_camera::LocalCamera,
    pub available_cameras: Vec<CameraDevice>,
    pub cameras_watching: HashSet<String>,
    pub camera_capture_available: bool,

    pub available_input_devices: Vec<String>,
    pub available_output_devices: Vec<String>,
    pub active_input_device: Option<String>,
    pub active_output_device: Option<String>,
    pub mic_test_active: bool,
    pub selected_input_device: Option<String>,
    pub selected_output_device: Option<String>,
    pub mic_sensitivity: u32,
    pub mic_volume: u16,
    pub mic_processing: crate::mic_profile::Processing,
    pub auto_gain_control: bool,
    pub mic_level: u32,
    pub mic_gate_level: u32,
    pub mic_level_pre: u32,
    pub noise_cancellation: bool,
    pub denoise_atten_lim_db: u32,
    pub bypass_system_audio_processing: bool,
    pub mic_bypass_error: Option<String>,
    pub denoise_fallback: Option<crate::denoise::Fallback>,
    pub denoise_retry: u32,
    pub voice_bitrate_kbps: u32,
    pub voice_uplink_congested: bool,
    pub voice_ping_ms: Option<u32>,
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
    /// Counts departures from any call. A leave and a rejoin applied in one poll leave
    /// the roster as it was, and the member is owed the media key all the same.
    pub voice_turnover: u64,
    pub identity: Option<crate::identity::Identity>,
    pub media_key: crate::mediakey::CallKey,
    pub host_info: Option<HostInfo>,
    pub transport: Transport,
    pub integrations: HashMap<Id, Vec<BotInstall>>,
    /// What the host answered `FetchAppManifest` with, by bot key.
    pub app_manifest_events: HashMap<String, Option<String>>,
    /// Manifest events our relays served, newest per key, with `created_at`.
    pub relay_app_manifests: HashMap<String, (i64, String)>,
    /// Activities each guild's host holds a bundle for.
    pub guild_activities: HashMap<Id, Vec<crate::protocol::GuildActivity>>,
    /// Joinable activities running in each channel we can see.
    pub activity_instances: HashMap<Id, Vec<crate::protocol::ActivityInstance>>,
    /// Per guild, from its host: curators followed and the lists it holds.
    pub guild_blocklists: HashMap<Id, (Vec<String>, Vec<crate::protocol::AppBlocklist>)>,
    /// Blocklist events our relays served, newest per curator, with `created_at`.
    pub relay_blocklists: HashMap<String, (i64, String)>,
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
    /// Set by a switch from the server rail; the next Ready opens this guild.
    pub open_guild_on_ready: Option<Id>,
    /// A tapped mention notification on its way to its channel.
    pub mention_tap: Option<crate::push::tap::PendingTap>,
    /// Counts Readies, so a reconnect is seen even when nothing else changed.
    pub readies: u64,
    pub composer_focus_request: Option<(Id, u64)>,
    /// Set when following a voice invite; joins this voice channel upon ready or guild join.
    pub voice_invite_on_ready: Option<PendingVoiceInvite>,
    pub gateway_failure: Option<String>,
    pub is_operator: bool,
    /// The connected server's own agents; only ever sent to an operator.
    pub server_agents: Vec<crate::protocol::ServerAgentView>,
    /// The operator's rules and every install, when they asked (#455).
    pub server_apps: Option<ServerApps>,
    /// Server agents a guild's admin may install.
    pub offered_agents: Vec<crate::protocol::OfferedAgent>,
    /// The connected server's, from Ready and `DefaultGuild`.
    pub default_guild: Option<Id>,
    /// The connected server's name and the ceilings every guild is held under.
    pub server_info: crate::protocol::ServerInfo,
    /// Only ever sent to an operator, when they open Server Settings.
    pub server_settings: Option<crate::protocol::ServerSettingsView>,
    pub audit_logs: HashMap<Id, Vec<crate::protocol::AuditEntry>>,
}

impl AppState {
    /// The guild's file policy under the server's ceilings: what the server enforces.
    pub fn file_policy(&self, guild_id: Id) -> crate::protocol::FilePolicy {
        self.server_info.limits.file_policy(
            self.guild_file_policies
                .get(&guild_id)
                .cloned()
                .unwrap_or_default(),
        )
    }

    pub fn empty() -> Self {
        Self {
            status: ConnectionStatus::Disconnected,
            self_user: None,
            guilds: Vec::new(),
            channels: Vec::new(),
            members: Vec::new(),
            messages: BTreeMap::new(),
            history_loaded: HashSet::new(),
            profile_synced_at: 0,
            account_pictures: None,
            account_dnd: None,
            message_search: None,
            message_jump: None,
            file_upload_result: None,
            server_chat_tools: false,
            server_image_previews: false,
            server_progressive_image_uploads: false,
            guild_file_policies: BTreeMap::new(),
            guild_stickers: BTreeMap::new(),
            voice_states: Vec::new(),
            voice: VoiceSession::default(),
            voice_session_epoch: 0,
            selected_guild: None,
            selected_channel: None,
            last_text_channels: Vec::new(),
            dms: Vec::new(),
            dm_emoji: HashMap::new(),
            saved_emoji: Vec::new(),
            dm_unread: HashMap::new(),
            channel_unread: HashSet::new(),
            channel_mentions: HashMap::new(),
            channel_read_at: BTreeMap::new(),
            channel_latest: HashMap::new(),
            dm_cleared_at: HashMap::new(),
            dm_clock_offset: HashMap::new(),
            dm_read_at: HashMap::new(),
            read_marks_revision: 0,
            unread_from: None,
            muted_channels: HashSet::new(),
            muted_guilds: HashSet::new(),
            nostr_event_ids: HashMap::new(),
            dm_deleted_ids: HashSet::new(),
            pending_dm_edits: HashMap::new(),
            dm_edit_versions: HashMap::new(),
            pending_dm_deletions: HashSet::new(),
            dm_delivery: HashMap::new(),
            dm_pending: 0,
            dm_images: HashMap::new(),
            dm_call_history: HashMap::new(),
            pending_edits: HashSet::new(),
            dm_call: None,
            contacts: Default::default(),
            mutes: Default::default(),
            dm_choices: HashMap::new(),
            nostr_relays_up: std::collections::HashSet::new(),
            nostr_names: HashMap::new(),
            dm_mode: false,
            dm_pane_open: false,
            app_foreground: !cfg!(target_os = "android"),
            dm_notification_open: None,
            catalog: Vec::new(),
            catalog_total: 0,
            catalog_revision: 0,
            guild_join_confirmation: None,
            profiles: HashMap::new(),
            global_xp: HashMap::new(),
            server_origin: None,
            rendezvous_url: None,
            resolved_share: None,
            activities: HashMap::new(),
            profile_card: None,
            command_form: None,
            image_viewer: None,
            guild_dialog: None,
            rules_prompt: None,
            audio_settings: false,
            agents_open: false,
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
            screen_share: Default::default(),
            screen_audio_joined: false,
            screen_shares: HashMap::new(),
            screen_viewing: Default::default(),
            replying_to: None,
            screen_capture_available: false,
            camera: Default::default(),
            available_cameras: Vec::new(),
            cameras_watching: HashSet::new(),
            camera_capture_available: false,
            available_input_devices: Vec::new(),
            available_output_devices: Vec::new(),
            active_input_device: None,
            active_output_device: None,
            mic_test_active: false,
            selected_input_device: None,
            selected_output_device: None,
            mic_sensitivity: crate::settings::DEFAULT_MIC_SENSITIVITY,
            mic_volume: 100,
            mic_processing: Default::default(),
            auto_gain_control: true,
            mic_level: 0,
            mic_gate_level: 0,
            mic_level_pre: 0,
            noise_cancellation: true,
            denoise_atten_lim_db: 30,
            bypass_system_audio_processing: true,
            mic_bypass_error: None,
            denoise_fallback: None,
            denoise_retry: 0,
            voice_bitrate_kbps: 64,
            voice_uplink_congested: false,
            voice_ping_ms: None,
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
            voice_turnover: 0,
            identity: None,
            media_key: Default::default(),
            host_info: None,
            transport: Transport::Loopback,
            integrations: HashMap::new(),
            app_manifest_events: HashMap::new(),
            relay_app_manifests: HashMap::new(),
            guild_activities: HashMap::new(),
            activity_instances: HashMap::new(),
            guild_blocklists: HashMap::new(),
            relay_blocklists: HashMap::new(),
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
            open_guild_on_ready: None,
            mention_tap: None,
            readies: 0,
            composer_focus_request: None,
            voice_invite_on_ready: None,
            gateway_failure: None,
            is_operator: false,
            server_agents: Vec::new(),
            server_apps: None,
            server_info: Default::default(),
            server_settings: None,
            server_settings_open: false,
            offered_agents: Vec::new(),
            default_guild: None,
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
        self.voice_event(VoiceEvent::Ended);
        self.drop_media_key();
        self.screen_token = None;
        self.voice_endpoint = None;
        self.voice_location = None;
        self.voice_send_route = None;
        self.voice_receive_route = None;
        self.peer_routes.clear();
        self.screen_audio_token = None;
        self.screen_video_token = None;
        self.screen_viewer_token = None;
        self.screen_share.on(ShareEvent::CallEnded);
        self.screen_viewing.clear();
        let _ = self.camera.on(CameraEvent::Released);
        self.cameras_watching.clear();
    }

    /// Not `self_user` or `server_origin`: Home keeps our own user, and the
    /// origin is known before Ready.
    pub fn server_session(&self) -> ServerSession {
        match self.status {
            ConnectionStatus::Disconnected => ServerSession::Absent,
            ConnectionStatus::Connecting | ConnectionStatus::Reconnecting => {
                ServerSession::Connecting
            }
            ConnectionStatus::Ready => ServerSession::Live,
        }
    }

    pub fn prepare_server_switch(&mut self, guild: Option<Id>, invite: Option<PendingVoiceInvite>) {
        self.clear_server_session();
        self.open_guild_on_ready = guild;
        self.voice_invite_on_ready = invite;
    }

    pub fn request_composer_focus(&mut self, channel_id: Id) {
        let revision = self
            .composer_focus_request
            .map_or(0, |(_, revision)| revision.wrapping_add(1));
        self.composer_focus_request = Some((channel_id, revision));
    }

    pub fn follow_voice_invite(&mut self, invite: PendingVoiceInvite) -> Option<ClientMessage> {
        self.voice_invite_on_ready = Some(invite);
        self.resume_voice_invite(None)
    }

    pub fn resume_voice_invite(&mut self, joined: Option<Id>) -> Option<ClientMessage> {
        let live = self.server_session() == ServerSession::Live;
        let pending = self.voice_invite_on_ready.as_mut()?;
        if pending
            .expires_at
            .is_some_and(|expires| chrono::Utc::now().timestamp() >= expires)
        {
            self.voice_invite_on_ready = None;
            return None;
        }
        if !live {
            return None;
        }
        if self.dm_call.is_some() {
            self.voice_invite_on_ready = None;
            return None;
        }
        if joined.is_some_and(|gid| gid != pending.guild_id) {
            return None;
        }
        if !self.guilds.iter().any(|g| g.id == pending.guild_id) {
            return pending.code.take().map(|code| ClientMessage::JoinByInvite {
                code,
                accept: false,
                pow_nonce: None,
            });
        }
        let pending = self.voice_invite_on_ready.take()?;
        if !self.channels.iter().any(|c| {
            c.id == pending.channel_id
                && c.guild_id == pending.guild_id
                && c.kind == crate::protocol::ChannelKind::Voice
        }) {
            return None;
        }
        self.dm_mode = false;
        self.selected_guild = Some(pending.guild_id);
        self.selected_channel = Some(pending.channel_id);
        crate::channel_selection::remember_current(self);
        Some(self.voice.join_message(pending.channel_id))
    }

    /// Names every field, so none can be added without saying whether it
    /// outlives the server (trap 7).
    pub fn clear_server_session(&mut self) {
        crate::channel_selection::remember_current(self);
        self.accept_guild_mates();
        self.end_voice_locally();
        self.screen_share.on(ShareEvent::PickerClosed);
        self.voice_event(VoiceEvent::JoinAbandoned);
        let dm_selected = self.selected_channel.filter(|id| self.dm_of(*id).is_some());
        let dm_channels: HashSet<_> = self.dms.iter().map(|dm| dm.channel_id).collect();
        let account_name = self.identity.as_ref().map(|i| i.display_name.clone());
        let Self {
            // The server session's: emptied here.
            status,
            self_user,
            guilds,
            channels,
            members,
            messages,
            history_loaded,
            message_search,
            message_jump,
            file_upload_result,
            server_chat_tools,
            server_image_previews,
            server_progressive_image_uploads,
            guild_file_policies,
            guild_stickers,
            voice_states,
            selected_guild,
            selected_channel,
            dm_mode,
            channel_unread,
            channel_mentions,
            channel_latest,
            pending_edits,
            catalog,
            catalog_total,
            guild_join_confirmation,
            profiles,
            server_origin,
            rendezvous_url,
            resolved_share,
            activities,
            profile_card,
            command_form,
            image_viewer,
            guild_dialog,
            rules_prompt,
            audio_settings,
            server_settings_open,
            topology_open,
            channel_access_open,
            typing,
            sfu_tunnel_url,
            gateway_route,
            voice_route_revision,
            screen_audio_joined,
            ice_servers,
            screen_shares,
            replying_to,
            voice_uplink_congested,
            voice_ping_ms,
            voice_quality,
            voice_stats,
            screen_share_stats,
            screen_share_in_stats,
            soundboard_open,
            soundboard_adjusting,
            soundboard_anchor,
            recent_sounds,
            voice_menu,
            stream_has_audio,
            media_undecryptable,
            pending_rekey,
            voice_turnover,
            host_info,
            transport,
            integrations,
            app_manifest_events,
            guild_activities,
            activity_instances,
            guild_blocklists,
            bot_commands,
            command_notes,
            guild_emojis,
            guild_sounds,
            emoji_images,
            emoji_requested,
            roles,
            bans,
            invites,
            open_guild_on_ready,
            voice_invite_on_ready,
            gateway_failure,
            is_operator,
            server_agents,
            server_apps,
            offered_agents,
            default_guild,
            server_info,
            server_settings,
            audit_logs,

            // The server session's, ended above through their own writers.
            voice: _,
            media_key: _,
            camera: _,
            cameras_watching: _,
            screen_share: _,
            screen_viewing: _,
            screen_token: _,
            screen_audio_token: _,
            screen_video_token: _,
            screen_viewer_token: _,
            voice_endpoint: _,
            voice_location: _,
            voice_send_route: _,
            voice_receive_route: _,
            peer_routes: _,

            // The account's: DMs, Nostr, devices and preferences outlive every server.
            identity: _,
            profile_synced_at: _,
            account_pictures: _,
            account_dnd: _,
            last_text_channels: _,
            channel_read_at: _,
            muted_channels: _,
            muted_guilds: _,
            unread_from: _,
            dms: _,
            dm_emoji: _,
            saved_emoji: _,
            dm_unread: _,
            dm_cleared_at: _,
            dm_read_at: _,
            dm_clock_offset: _,
            nostr_event_ids: _,
            dm_deleted_ids: _,
            pending_dm_edits: _,
            dm_edit_versions: _,
            pending_dm_deletions: _,
            dm_delivery: _,
            dm_pending: _,
            dm_images: _,
            dm_call_history: _,
            dm_call: _,
            contacts: _,
            mutes: _,
            dm_choices: _,
            nostr_relays_up: _,
            nostr_names: _,
            relay_app_manifests: _,
            relay_blocklists: _,
            global_xp: _,
            dm_pane_open: _,
            app_foreground: _,
            dm_notification_open: _,
            mention_tap: _,
            error_toast: _,
            agents_open: _,
            composer_focus_request: _,
            screen_capture_available: _,
            camera_capture_available: _,
            available_cameras: _,
            available_input_devices: _,
            available_output_devices: _,
            active_input_device: _,
            active_output_device: _,
            selected_input_device: _,
            selected_output_device: _,
            mic_test_active: _,
            mic_sensitivity: _,
            mic_volume: _,
            mic_processing: _,
            auto_gain_control: _,
            mic_level: _,
            mic_gate_level: _,
            mic_level_pre: _,
            noise_cancellation: _,
            denoise_atten_lim_db: _,
            bypass_system_audio_processing: _,
            mic_bypass_error: _,
            denoise_fallback: _,
            denoise_retry: _,
            voice_bitrate_kbps: _,
            user_volumes: _,
            user_muted: _,
            stream_volumes: _,
            stream_muted: _,
            soundboard_volume: _,
            // Counters: starting one over would let a stale value equal a new one.
            voice_session_epoch: _,
            read_marks_revision: _,
            catalog_revision: _,
            notify_tick: _,
            dm_notify_tick: _,
            readies: _,
        } = &mut *self;
        *status = ConnectionStatus::Disconnected;
        // We stay ourselves at Home, where DMs and call history read it (`nostr::service` seeds it).
        if let (Some(me), Some(name)) = (self_user.as_mut(), account_name) {
            me.username = name;
        }
        guilds.clear();
        channels.clear();
        members.clear();
        messages.retain(|id, _| dm_channels.contains(id));
        history_loaded.clear();
        *message_search = None;
        *message_jump = None;
        *file_upload_result = None;
        *server_chat_tools = false;
        *server_image_previews = false;
        *server_progressive_image_uploads = false;
        guild_file_policies.clear();
        guild_stickers.clear();
        voice_states.clear();
        *selected_guild = None;
        *selected_channel = dm_selected;
        *dm_mode = dm_selected.is_some();
        channel_unread.clear();
        channel_mentions.clear();
        channel_latest.clear();
        pending_edits.clear();
        catalog.clear();
        *catalog_total = 0;
        *guild_join_confirmation = None;
        profiles.clear();
        *server_origin = None;
        *rendezvous_url = None;
        *resolved_share = None;
        activities.clear();
        *profile_card = None;
        *command_form = None;
        *image_viewer = None;
        *guild_dialog = None;
        *rules_prompt = None;
        *audio_settings = false;
        *server_settings_open = false;
        *topology_open = false;
        *channel_access_open = None;
        typing.clear();
        *sfu_tunnel_url = None;
        *gateway_route = None;
        *voice_route_revision = 0;
        *screen_audio_joined = false;
        ice_servers.clear();
        screen_shares.clear();
        *replying_to = None;
        *voice_uplink_congested = false;
        *voice_ping_ms = None;
        voice_quality.clear();
        voice_stats.clear();
        *screen_share_stats = None;
        *screen_share_in_stats = None;
        *soundboard_open = false;
        *soundboard_adjusting = false;
        *soundboard_anchor = (0.0, 0.0);
        recent_sounds.clear();
        *voice_menu = None;
        *stream_has_audio = Default::default();
        *media_undecryptable = false;
        *pending_rekey = false;
        *voice_turnover = 0;
        *host_info = None;
        *transport = Transport::Loopback;
        integrations.clear();
        app_manifest_events.clear();
        guild_activities.clear();
        activity_instances.clear();
        guild_blocklists.clear();
        bot_commands.clear();
        command_notes.clear();
        guild_emojis.clear();
        guild_sounds.clear();
        emoji_images.clear();
        emoji_requested.clear();
        roles.clear();
        bans.clear();
        invites.clear();
        *open_guild_on_ready = None;
        *voice_invite_on_ready = None;
        *gateway_failure = None;
        *is_operator = false;
        server_agents.clear();
        *server_apps = None;
        offered_agents.clear();
        *default_guild = None;
        *server_info = Default::default();
        *server_settings = None;
        audit_logs.clear();
        self.evict_inactive_dm_images(self.selected_channel);
    }

    pub(crate) fn retry_image_viewer(&mut self, id: Id) {
        let Some(view) = self.image_viewer.as_mut().filter(|view| view.id == id) else {
            return;
        };
        if let Some(crate::features::image_viewer::Original::Community(address)) = &view.target
            && let Some(name) = address.strip_prefix("media:")
        {
            self.emoji_images.retry_empty(name);
            self.emoji_requested.remove(name);
        }
        view.retry();
    }

    #[track_caller]
    pub fn evict_inactive_dm_images(&mut self, active_channel: Option<Id>) {
        for image in self.dm_images.values_mut() {
            if Some(image.channel_id) != active_channel {
                image.unload();
            }
        }
        let dm_channels: std::collections::HashSet<_> =
            self.dms.iter().map(|dm| dm.channel_id).collect();
        for (cid, msgs) in self.messages.iter_mut() {
            if Some(*cid) != active_channel && !dm_channels.contains(cid) && msgs.len() > 50 {
                let drain_count = msgs.len() - 50;
                msgs.drain(0..drain_count);
                self.history_loaded.remove(cid);
            }
        }
        if active_channel.is_none() {
            self.image_viewer = None;
            self.emoji_images.clear();
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

    fn emoji_address(&self, guild_id: Id, shortcode: &str) -> Option<&str> {
        self.guild_emojis
            .get(&guild_id)?
            .iter()
            .find(|e| e.shortcode == shortcode)
            .map(|e| e.image.as_str())
    }

    pub fn emoji_image(&self, guild_id: Id, shortcode: &str) -> Option<&str> {
        self.emoji_images
            .get(self.emoji_address(guild_id, shortcode)?)
            .map(String::as_str)
            .filter(|u| !u.is_empty())
    }

    pub fn emoji_uri(&self, guild_id: Id, shortcode: &str) -> Option<String> {
        crate::image_assets::cached_uri(
            &self.emoji_images,
            self.emoji_address(guild_id, shortcode)?,
        )
    }

    pub fn saved_emoji_of(&self, image: &str) -> Option<&crate::settings::SavedEmoji> {
        self.saved_emoji.iter().find(|e| e.image == image)
    }

    /// Keeps a guild emoji under `shortcode`, the person's own name for it.
    /// The same picture saved again is a rename. Needs the picture loaded,
    /// since the copy is made now, not when the guild is gone.
    pub fn save_emoji(&mut self, shortcode: &str, image: &str, guild_id: Id) -> Result<(), String> {
        let code = shortcode.trim().to_ascii_lowercase();
        if !crate::protocol::valid_shortcode(&code) {
            return Err(format!(
                "A name is 2-{} lowercase letters, digits or _.",
                crate::protocol::MAX_SHORTCODE_LEN
            ));
        }
        if self
            .saved_emoji
            .iter()
            .any(|e| e.shortcode == code && e.image != image)
        {
            return Err(format!(
                ":{code}: is already one of your emoji — pick another name."
            ));
        }
        let Some(data) = self
            .emoji_images
            .get(image)
            .cloned()
            .filter(|d| !d.is_empty())
        else {
            return Err("Its picture has not loaded yet — try again in a moment.".into());
        };
        let guild_name = self
            .guilds
            .iter()
            .find(|g| g.id == guild_id)
            .map(|g| g.name.clone())
            .unwrap_or_default();
        let blossom_url = self
            .saved_emoji_of(image)
            .and_then(|e| e.blossom_url.clone());
        self.saved_emoji.retain(|e| e.image != image);
        self.saved_emoji.push(crate::settings::SavedEmoji {
            shortcode: code,
            image: image.to_string(),
            guild_id,
            guild_name,
            blossom_url,
        });
        crate::emoji::store_saved(image, &data);
        Ok(())
    }

    pub fn remove_saved_emoji(&mut self, image: &str) {
        self.saved_emoji.retain(|e| e.image != image);
        crate::emoji::remove_saved(image);
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

    pub fn connected_voice_channel(&self) -> Option<Id> {
        self.voice.connected_channel()
    }

    /// `voice_session_epoch` numbers the native rooms: a restart rebuilds the
    /// screen rooms too, so trap 11's bridge must see a new one and republish.
    pub fn voice_event(&mut self, event: VoiceEvent) -> bool {
        let up = matches!(event, VoiceEvent::RoomUp(_));
        // A rejoin of the same channel is another call: nothing keyed on the epoch may carry over.
        let ended = matches!(event, VoiceEvent::Ended);
        let changed = self.voice.on(event);
        if ended || up && changed {
            self.voice_session_epoch = self.voice_session_epoch.wrapping_add(1);
        }
        changed
    }

    pub fn voice_call(&self) -> Option<crate::voice_session::VoiceCall> {
        Some(crate::voice_session::VoiceCall {
            channel: self.server_voice_channel()?,
            route: self.voice_route_revision,
        })
    }

    /// Keys can arrive before the local media token, so the server channel leads the join.
    pub fn server_voice_channel(&self) -> Option<Id> {
        let me = self.self_user.as_ref()?;
        self.voice_states
            .iter()
            .find(|v| v.user_pubkey == me.pubkey)?
            .channel_id
    }

    /// The guild of the voice channel we are in, whose sounds the board offers.
    pub fn voice_guild(&self) -> Option<Id> {
        let channel = self.voice.channel_id()?;
        self.channels
            .iter()
            .find(|c| c.id == channel)
            .map(|c| c.guild_id)
    }

    /// Only a person in the call's guild can have held its media key: a bot
    /// never joins a call, and a key is relayed only within one (trap 29).
    pub fn removal_holds_key(&self, guild_id: Id, pubkey: &str) -> bool {
        let bot = self
            .members
            .iter()
            .any(|m| m.guild_id == guild_id && m.user.pubkey == pubkey && m.bot);
        !bot && self.voice_guild().is_none_or(|g| g == guild_id)
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

    pub fn note_account_profile(&mut self, owner: String, profile: crate::profile::LocalProfile) {
        let dnd = profile
            .status
            .as_deref()
            .is_some_and(|status| status.trim() == "dnd");
        self.account_dnd = Some((owner.clone(), dnd));
        self.account_pictures = Some((owner, profile.into()));
    }

    pub fn account_is_dnd(&self, pubkey: &str) -> bool {
        self.account_dnd
            .as_ref()
            .filter(|(owner, _)| owner == pubkey)
            .map_or_else(|| self.presence_of(pubkey) == "dnd", |(_, dnd)| *dnd)
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
            .fold(0, u64::saturating_add)
    }

    pub fn global_xp_of(&self, pubkey: &str) -> Option<crate::nostr::xp::GlobalXp> {
        self.global_xp.get(pubkey).copied()
    }

    pub fn activity_of(&self, pubkey: &str) -> Option<&crate::protocol::Activity> {
        self.activities.get(pubkey)
    }

    /// The open guild's dressing for this key, which only an app it installed
    /// has. The profile is one per key for the whole server.
    fn guild_look_of(&self, pubkey: &str) -> Option<&crate::protocol::GuildLook> {
        if self.dm_mode {
            return None;
        }
        let guild = self.selected_guild?;
        self.members
            .iter()
            .find(|m| m.guild_id == guild && m.user.pubkey == pubkey)
            .map(|m| &m.look)
    }

    pub fn avatar_address(&self, pubkey: &str) -> Option<&str> {
        self.guild_look_of(pubkey)
            .and_then(|l| l.avatar.as_deref())
            .or_else(|| match self.profiles.get(pubkey) {
                Some(profile) => profile.avatar.as_deref(),
                None => self
                    .account_pictures
                    .as_ref()
                    .filter(|(owner, _)| owner == pubkey)?
                    .1
                    .avatar
                    .as_deref()
                    .map(String::as_str),
            })
    }

    pub fn banner_address(&self, pubkey: &str) -> Option<&str> {
        self.guild_look_of(pubkey)
            .and_then(|l| l.banner.as_deref())
            .or_else(|| match self.profiles.get(pubkey) {
                Some(profile) => profile.banner.as_deref(),
                None => self
                    .account_pictures
                    .as_ref()
                    .filter(|(owner, _)| owner == pubkey)?
                    .1
                    .banner
                    .as_deref()
                    .map(String::as_str),
            })
    }

    pub fn bio_of(&self, pubkey: &str) -> Option<&str> {
        self.guild_look_of(pubkey)
            .and_then(|l| l.bio.as_deref())
            .or_else(|| self.profiles.get(pubkey)?.bio.as_deref())
            .filter(|b| !b.trim().is_empty())
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

    pub fn media_src_shared(&self, raw: &str) -> Option<std::sync::Arc<String>> {
        match raw.strip_prefix("media:") {
            None => Some(std::sync::Arc::new(raw.to_string())),
            Some(address) => self
                .emoji_images
                .get_shared(address)
                .filter(|u| !u.is_empty()),
        }
    }

    pub fn default_channel_of(&self, guild_id: Id) -> Option<Id> {
        if let Some(channel) = crate::channel_selection::recalled(self, guild_id) {
            return Some(channel);
        }
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
        self.known_display_name(pubkey)
            .map(str::to_owned)
            .unwrap_or_else(|| crate::identity::truncate_pubkey(pubkey))
    }

    pub fn display_name_or(&self, pubkey: &str, fallback: &str) -> String {
        self.known_display_name(pubkey)
            .unwrap_or(fallback)
            .to_owned()
    }

    fn known_display_name(&self, pubkey: &str) -> Option<&str> {
        if let Some(u) = self.user_of(pubkey) {
            return Some(&u.username);
        }
        if let Some(pet) = self.contacts.petname(pubkey) {
            return Some(pet);
        }
        if let Some((published, _)) = self.nostr_names.get(pubkey) {
            return Some(published);
        }
        None
    }

    pub fn dm_of(&self, channel_id: Id) -> Option<&DmInfo> {
        self.dms.iter().find(|d| d.channel_id == channel_id)
    }

    pub fn dm_last_message(&self, channel_id: Id) -> Option<&Message> {
        self.messages.get(&channel_id).and_then(|m| m.last())
    }

    /// A bot in the guild is the very sender #311 is about, so it introduces nobody.
    pub fn shares_guild(&self, pubkey: &str) -> bool {
        self.members
            .iter()
            .any(|m| m.user.pubkey == pubkey && !m.bot)
    }

    pub fn dm_standing(&self, peer: &str) -> DmStanding {
        crate::dm_standing::standing(
            self.mutes.contains(peer),
            self.dm_choice(peer),
            self.contacts.contains(peer) || self.shares_guild(peer),
        )
    }

    pub fn dm_choice(&self, peer: &str) -> Option<DmChoice> {
        self.dm_choices.get(peer).copied()
    }

    /// The one writer of a peer's stored choice.
    pub fn choose_dm(&mut self, peer: &str, choice: Option<DmChoice>) {
        match choice {
            Some(choice) => {
                self.dm_choices.insert(peer.to_string(), choice);
            }
            None => {
                self.dm_choices.remove(peer);
            }
        }
    }

    /// The accepted and ignored lists `settings.json` keeps.
    pub fn dm_choice_lists(&self) -> (Vec<String>, Vec<String>) {
        crate::dm_standing::lists_of(&self.dm_choices)
    }

    pub fn accepted_dms(&self) -> impl Iterator<Item = &str> {
        self.dm_choices
            .iter()
            .filter(|(_, choice)| **choice == DmChoice::Accepted)
            .map(|(peer, _)| peer.as_str())
    }

    /// Standing must not depend on the open server (trap 7), so a guild-mate stays in the
    /// Inbox once the roster is gone. An explicit ignore stands: a shared guild only outranked it.
    pub fn accept_guild_mates(&mut self) {
        let mates: Vec<String> = self.guild_mates_to_accept().cloned().collect();
        for peer in mates {
            self.choose_dm(&peer, Some(DmChoice::Accepted));
        }
    }

    fn guild_mates_to_accept(&self) -> impl Iterator<Item = &String> {
        self.dms
            .iter()
            .map(|d| &d.other_pubkey)
            .filter(|peer| self.shares_guild(peer) && self.dm_choice(peer).is_none())
    }

    pub fn preferred_dm(&self) -> Option<DmInfo> {
        self.selected_channel
            .and_then(|id| self.dm_of(id))
            .cloned()
            .or_else(|| self.dms_by_recency(DmStanding::Inbox).into_iter().next())
    }

    /// Empty conversations sort last: one just opened by pasting a key has no
    /// activity to be recent about.
    pub fn dms_by_recency(&self, standing: DmStanding) -> Vec<DmInfo> {
        let mut v: Vec<DmInfo> = self
            .dms
            .iter()
            .filter(|d| self.dm_standing(&d.other_pubkey) == standing)
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            let at = self.dm_last_message(a.channel_id).map(|m| m.created_at);
            let bt = self.dm_last_message(b.channel_id).map(|m| m.created_at);
            bt.cmp(&at)
        });
        v
    }

    pub fn dm_unread_total(&self) -> u32 {
        self.dms
            .iter()
            .filter(|d| self.dm_standing(&d.other_pubkey) == DmStanding::Inbox)
            .map(|d| self.dm_unread(d.channel_id))
            .sum()
    }

    /// What a conversation shows as unread: nothing while it is being read.
    pub fn dm_unread(&self, channel_id: Id) -> u32 {
        if self.watched_dm() == Some(channel_id) {
            return 0;
        }
        self.dm_unread.get(&channel_id).copied().unwrap_or(0)
    }

    /// The conversation the screen shows, in front or not. Android's notifier
    /// needs it apart from focus: it learns of a resume before we do.
    pub fn dm_in_view(&self) -> Option<Id> {
        self.selected_channel
            .filter(|_| self.dm_mode && self.dm_pane_open)
    }

    /// The one answer to "is this conversation being read right now": what
    /// counts, rings, badges and dismisses alerts all ask it.
    pub fn watched_dm(&self) -> Option<Id> {
        self.dm_in_view().filter(|_| self.app_foreground)
    }

    /// The watched conversation while it still holds a count, for the effect
    /// that marks it read.
    pub fn unread_on_screen(&self) -> Option<Id> {
        self.watched_dm()
            .filter(|channel| self.dm_unread.contains_key(channel))
    }

    /// The one writer of `dm_unread`. `Settle` caps the count at what is past
    /// the read mark, so a mark moved elsewhere takes the count down with it.
    pub fn change_dm_unread(&mut self, channel_id: Id, change: UnreadChange) {
        match change {
            UnreadChange::Arrived => *self.dm_unread.entry(channel_id).or_insert(0) += 1,
            UnreadChange::Read => {
                self.dm_unread.remove(&channel_id);
            }
            UnreadChange::Settle => {
                let Some(&counted) = self.dm_unread.get(&channel_id) else {
                    return;
                };
                let left = counted.min(self.dm_unread_bound(channel_id));
                if left == 0 {
                    self.dm_unread.remove(&channel_id);
                } else {
                    self.dm_unread.insert(channel_id, left);
                }
            }
        }
    }

    /// The peer's messages and our missed calls past the read mark. A call's
    /// row is authored by its caller, so it counts once, as the missed call.
    fn dm_unread_bound(&self, channel_id: Id) -> u32 {
        let Some(peer) = self.dm_of(channel_id).map(|dm| dm.other_pubkey.as_str()) else {
            return 0;
        };
        let read = self.dm_read_at.get(peer).copied();
        let past = |at: i64| read.is_none_or(|mark| at > mark);
        let messages = self
            .messages
            .get(&channel_id)
            .into_iter()
            .flatten()
            .filter(|m| {
                m.author.pubkey == peer
                    && !self.dm_call_history.contains_key(&m.id)
                    && past(m.created_at.timestamp())
            })
            .count();
        let calls = self
            .dm_call_history
            .values()
            .filter(|entry| {
                entry.confirmed && (entry.record.caller == peer || entry.record.callee == peer)
            })
            .filter_map(|entry| entry.missed_at)
            .filter(|missed| past(*missed))
            .count();
        u32::try_from(messages + calls).unwrap_or(u32::MAX)
    }

    pub fn dm_request_count(&self) -> usize {
        self.dms
            .iter()
            .filter(|d| self.dm_standing(&d.other_pubkey) == DmStanding::Request)
            .count()
    }

    /// Moves the read mark too: nothing counted while it was a request, and a
    /// mark left behind would ring for all of it on the next launch's replay.
    pub fn accept_dm(&mut self, peer: &str) {
        self.choose_dm(peer, Some(DmChoice::Accepted));
        self.mark_dm_read(crate::nostr::service::conversation_id(peer));
    }

    pub fn ignore_dm(&mut self, peer: &str) {
        self.choose_dm(peer, Some(DmChoice::Ignored));
        let cid = crate::nostr::service::conversation_id(peer);
        if self.selected_channel == Some(cid) {
            self.selected_channel = None;
        }
    }

    pub fn set_mutes(&mut self, mutes: crate::nostr::nip51::MuteList) {
        self.mutes = mutes;
        let blocked: Vec<Id> = self
            .dms
            .iter()
            .filter(|d| self.mutes.contains(&d.other_pubkey))
            .map(|d| d.channel_id)
            .collect();
        for cid in blocked {
            self.drop_conversation(cid);
        }
    }

    fn drop_conversation(&mut self, cid: Id) {
        if let Some(peer) = self.dm_of(cid).map(|dm| dm.other_pubkey.clone()) {
            self.dm_call_history
                .retain(|_, entry| entry.record.caller != peer && entry.record.callee != peer);
        }
        self.dms.retain(|d| d.channel_id != cid);
        if let Some(messages) = self.messages.remove(&cid) {
            for message in messages {
                self.dm_images.remove(&message.id);
            }
        }
        self.change_dm_unread(cid, UnreadChange::Read);
        if self.selected_channel == Some(cid) {
            self.selected_channel = None;
        }
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
    pub fn apply_message_edit(
        &mut self,
        channel_id: Id,
        message_id: Id,
        content: String,
        edited_at: Option<chrono::DateTime<chrono::Utc>>,
    ) {
        self.pending_edits.remove(&message_id);
        let search = self
            .message_search
            .as_mut()
            .filter(|(cid, ..)| *cid == channel_id)
            .map(|(_, _, found)| found);
        for messages in self.messages.get_mut(&channel_id).into_iter().chain(search) {
            for m in messages.iter_mut().filter(|m| m.id == message_id) {
                m.content = content.clone();
                m.edited_at = edited_at;
            }
        }
    }

    /// Shown at once with a pending mark; the server's answer replaces it either way.
    pub fn edit_locally(&mut self, channel_id: Id, message_id: Id, content: String) {
        self.apply_message_edit(channel_id, message_id, content, Some(chrono::Utc::now()));
        self.pending_edits.insert(message_id);
    }

    pub fn delete_message_locally(&mut self, channel_id: Id, message_id: Id) {
        self.dm_images.remove(&message_id);
        self.dm_edit_versions.remove(&message_id);
        if let Some(msgs) = self.messages.get_mut(&channel_id) {
            msgs.retain(|m| m.id != message_id);
        }
        if let Some((cid, _, search_msgs)) = &mut self.message_search
            && *cid == channel_id
        {
            search_msgs.retain(|m| m.id != message_id);
        }
        if let Some(rumor_id) = self.nostr_event_ids.remove(&message_id) {
            self.pending_dm_edits.retain(|(id, _), _| id != &rumor_id);
            self.pending_dm_deletions.retain(|(id, _)| id != &rumor_id);
        }
        self.dm_delivery.remove(&message_id);
        self.dm_emoji.remove(&message_id);
        self.pending_edits.remove(&message_id);
        if self
            .replying_to
            .as_ref()
            .is_some_and(|r| r.message_id == message_id)
        {
            self.replying_to = None;
        }
    }

    pub fn insert_message(&mut self, channel_id: Id, message: Message) -> bool {
        if crate::features::is_expired_message(&message, chrono::Utc::now().timestamp()) {
            return false;
        }
        if self
            .messages
            .get(&channel_id)
            .is_some_and(|held| held.iter().any(|m| m.id == message.id))
        {
            return false;
        }
        let own = self
            .self_user
            .as_ref()
            .is_some_and(|u| u.pubkey == message.author.pubkey);
        let viewing =
            self.app_foreground && !self.dm_mode && self.selected_channel == Some(channel_id);
        if self.channels.iter().any(|channel| channel.id == channel_id) {
            let at = message.created_at.timestamp_millis();
            let latest = self.channel_latest.entry(channel_id).or_insert(at);
            *latest = (*latest).max(at);
            if own || viewing {
                self.advance_read_mark(channel_id, at);
            } else {
                self.channel_unread.insert(channel_id);
                if self
                    .self_user
                    .as_ref()
                    .is_some_and(|u| crate::protocol::mentions_name(&message.content, &u.username))
                {
                    *self.channel_mentions.entry(channel_id).or_insert(0) += 1;
                }
            }
        }
        let held = self.messages.entry(channel_id).or_default();
        let index = held.partition_point(|m| m.created_at <= message.created_at);
        held.insert(index, message);
        true
    }

    pub fn complete_pending_image(&mut self, message: &Message) -> bool {
        if message
            .image_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.upload_pending)
        {
            return false;
        }
        let Some(existing) = self
            .messages
            .get_mut(&message.channel_id)
            .and_then(|messages| messages.iter_mut().find(|item| item.id == message.id))
        else {
            return false;
        };
        if !existing
            .image_metadata
            .as_ref()
            .is_some_and(|metadata| metadata.upload_pending)
        {
            return false;
        }
        *existing = message.clone();
        true
    }

    /// Unread is kept for a muted channel and only hidden, so unmuting one
    /// shows what arrived meanwhile.
    pub fn channel_shows_unread(&self, channel_id: Id) -> bool {
        self.channel_unread.contains(&channel_id) && !self.is_muted(channel_id)
    }

    pub fn guild_has_unread(&self, guild_id: Id) -> bool {
        !self.muted_guilds.contains(&guild_id)
            && self.channels.iter().any(|channel| {
                channel.guild_id == guild_id
                    && self.channel_unread.contains(&channel.id)
                    && !self.muted_channels.contains(&channel.id)
            })
    }

    /// Hidden under a mute and kept, like the dot: a mute silences everything.
    pub fn channel_mention_count(&self, channel_id: Id) -> u32 {
        if self.is_muted(channel_id) {
            return 0;
        }
        self.channel_mentions.get(&channel_id).copied().unwrap_or(0)
    }

    pub fn guild_mention_count(&self, guild_id: Id) -> u32 {
        self.channels
            .iter()
            .filter(|channel| channel.guild_id == guild_id)
            .map(|channel| self.channel_mention_count(channel.id))
            .sum()
    }

    /// What the dock or taskbar icon says: a number for what is addressed to
    /// us, a bare dot for anything else unread.
    pub fn app_badge(&self) -> AppBadge {
        let mentions = self.dm_unread_total()
            + self
                .channels
                .iter()
                .map(|channel| self.channel_mention_count(channel.id))
                .sum::<u32>();
        if mentions > 0 {
            AppBadge::Count(mentions)
        } else if self
            .channels
            .iter()
            .any(|channel| self.channel_shows_unread(channel.id))
        {
            AppBadge::Unread
        } else {
            AppBadge::None
        }
    }

    pub fn mark_channel_read(&mut self, channel_id: Id) {
        let mark = self
            .server_origin
            .as_ref()
            .and_then(|server| self.channel_read_at.get(server)?.get(&channel_id).copied());
        self.note_unread_from(channel_id, self.channel_unread.contains(&channel_id), mark);
        self.channel_unread.remove(&channel_id);
        self.channel_mentions.remove(&channel_id);
        if let Some(at) = self.channel_latest.get(&channel_id).copied() {
            self.advance_read_mark(channel_id, at);
        }
    }

    fn note_unread_from(&mut self, channel_id: Id, unread: bool, mark: Option<i64>) {
        if unread && let Some(mark) = mark {
            self.unread_from = Some((channel_id, mark));
        } else if self.unread_from.is_some_and(|(id, _)| id == channel_id) {
            self.unread_from = None;
        }
    }

    fn advance_read_mark(&mut self, channel_id: Id, at: i64) {
        let Some(server) = self.server_origin.clone() else {
            return;
        };
        use std::collections::btree_map::Entry;
        let marks = self.channel_read_at.entry(server).or_default();
        let changed = match marks.entry(channel_id) {
            Entry::Vacant(e) => {
                e.insert(at);
                true
            }
            Entry::Occupied(mut e) => {
                if at > *e.get() {
                    e.insert(at);
                    true
                } else {
                    false
                }
            }
        };
        if changed {
            self.read_marks_revision = self.read_marks_revision.wrapping_add(1);
        }
    }

    /// Compares what arrived while we were away with our read marks. A channel
    /// with no mark is taken as read, or joining a busy guild lights it all up.
    pub fn note_latest_messages(&mut self, latest: HashMap<Id, i64>) {
        let Some(server) = self.server_origin.clone() else {
            return;
        };
        use std::collections::btree_map::Entry;
        let marks = self.channel_read_at.entry(server).or_default();
        let mut changed = false;
        for (channel_id, at) in latest {
            let viewing =
                self.app_foreground && !self.dm_mode && self.selected_channel == Some(channel_id);
            if !viewing && marks.get(&channel_id).is_some_and(|mark| at > *mark) {
                self.channel_unread.insert(channel_id);
            } else {
                match marks.entry(channel_id) {
                    Entry::Vacant(e) => {
                        e.insert(at);
                        changed = true;
                    }
                    Entry::Occupied(mut e) => {
                        if at > *e.get() {
                            e.insert(at);
                            changed = true;
                        }
                    }
                }
            }
            let held = self.channel_latest.entry(channel_id).or_insert(at);
            *held = (*held).max(at);
        }
        if changed {
            self.read_marks_revision = self.read_marks_revision.wrapping_add(1);
        }
    }

    /// What this device has read, in the form another device can use: DM
    /// marks go back onto the peer's clock (trap 18).
    pub fn read_marks(&self) -> crate::nostr::read_marks::ReadMarks {
        crate::nostr::read_marks::ReadMarks {
            dms: self
                .dm_read_at
                .iter()
                .map(|(peer, at)| (peer.clone(), at + self.clock_offset(peer)))
                .collect(),
            channels: self.channel_read_at.clone(),
        }
    }

    /// Another device's marks. They only ever move ours forward, so copies
    /// adopted in any order agree.
    pub fn adopt_read_marks(&mut self, marks: &crate::nostr::read_marks::ReadMarks) {
        let mut changed = false;
        for (peer, theirs) in &marks.dms {
            let at = theirs - self.clock_offset(peer);
            if self.dm_read_at.get(peer).is_some_and(|mine| *mine >= at) {
                continue;
            }
            self.dm_read_at.insert(peer.clone(), at);
            // The relays replay a conversation before the marks event may arrive.
            self.change_dm_unread(
                crate::nostr::service::conversation_id(peer),
                UnreadChange::Settle,
            );
            changed = true;
        }
        for (server, channels) in &marks.channels {
            let current = self.server_origin.as_deref() == Some(server.as_str());
            let mine = self.channel_read_at.entry(server.clone()).or_default();
            for (channel_id, at) in channels {
                use std::collections::btree_map::Entry;
                let (mark_val, mark_changed) = match mine.entry(*channel_id) {
                    Entry::Vacant(e) => {
                        e.insert(*at);
                        (*at, true)
                    }
                    Entry::Occupied(mut e) => {
                        if *at > *e.get() {
                            e.insert(*at);
                            (*at, true)
                        } else {
                            (*e.get(), false)
                        }
                    }
                };
                if mark_changed {
                    changed = true;
                }
                if current
                    && self
                        .channel_latest
                        .get(channel_id)
                        .is_some_and(|latest| *latest <= mark_val)
                {
                    self.channel_unread.remove(channel_id);
                    self.channel_mentions.remove(channel_id);
                }
            }
        }
        if changed {
            self.read_marks_revision = self.read_marks_revision.wrapping_add(1);
        }
    }

    /// Ready lists every channel this server still shows us, so a mark for any
    /// other is for a channel deleted or hidden since.
    pub fn prune_read_marks(&mut self) {
        let Some(server) = self.server_origin.as_deref() else {
            return;
        };
        if let Some(marks) = self.channel_read_at.get_mut(server) {
            let before = marks.len();
            marks.retain(|id, _| self.channels.iter().any(|c| c.id == *id));
            if marks.len() != before {
                self.read_marks_revision = self.read_marks_revision.wrapping_add(1);
            }
        }
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
        self.history_loaded.insert(channel_id);
        let held = self.messages.entry(channel_id).or_default();
        let mut seen: HashSet<Id> = held.iter().map(|m| m.id).collect();
        let mut incoming = Vec::with_capacity(page.len());
        for m in page {
            if crate::features::is_expired_message(&m, chrono::Utc::now().timestamp()) {
                continue;
            }
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
        let viewing = self.watched_dm() == Some(channel_id);
        if viewing {
            use std::collections::hash_map::Entry;
            let changed = match self.dm_read_at.entry(peer.to_string()) {
                Entry::Vacant(e) => {
                    e.insert(at);
                    true
                }
                Entry::Occupied(mut e) => {
                    if at > *e.get() {
                        e.insert(at);
                        true
                    } else {
                        false
                    }
                }
            };
            if changed {
                self.read_marks_revision = self.read_marks_revision.wrapping_add(1);
            }
        } else if self.dm_standing(peer) == DmStanding::Inbox
            && self.dm_read_at.get(peer).is_none_or(|mark| at > *mark)
        {
            self.change_dm_unread(channel_id, UnreadChange::Arrived);
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
        let unread = self.dm_unread.get(&channel_id).is_some_and(|n| *n > 0);
        self.change_dm_unread(channel_id, UnreadChange::Read);
        let Some(peer) = self.dm_of(channel_id).map(|d| d.other_pubkey.clone()) else {
            return;
        };
        // A DM mark is in seconds: everything after its second is new.
        let mark = self
            .dm_read_at
            .get(&peer)
            .map_or(i64::MIN, |at| at * 1000 + 999);
        self.note_unread_from(channel_id, unread, Some(mark));
        let newest = self
            .messages
            .get(&channel_id)
            .into_iter()
            .flatten()
            .map(|m| m.created_at.timestamp())
            .chain(
                self.dm_call_history
                    .values()
                    .filter(|entry| {
                        entry.confirmed
                            && (entry.record.caller == peer || entry.record.callee == peer)
                    })
                    .filter_map(|entry| entry.missed_at),
            )
            .max();
        if let Some(at) = newest {
            use std::collections::hash_map::Entry;
            let changed = match self.dm_read_at.entry(peer) {
                Entry::Vacant(e) => {
                    e.insert(at);
                    true
                }
                Entry::Occupied(mut e) => {
                    if at > *e.get() {
                        e.insert(at);
                        true
                    } else {
                        false
                    }
                }
            };
            if changed {
                self.read_marks_revision = self.read_marks_revision.wrapping_add(1);
            }
        }
    }

    // Relays replay old events and duplicates; neither may undo a rename.
    pub fn note_name(&mut self, pubkey: &str, name: String, at: i64) -> bool {
        match self.nostr_names.get(pubkey) {
            Some((_, seen)) if *seen >= at => false,
            _ => {
                if let Some(user) = self.self_user.as_mut().filter(|u| u.pubkey == pubkey) {
                    user.username.clone_from(&name);
                }
                for member in self
                    .members
                    .iter_mut()
                    .filter(|m| !m.bot && m.user.pubkey == pubkey)
                {
                    member.user.username.clone_from(&name);
                }
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
        self.drop_conversation(cid);
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
            crate::protocol::MemberSort::Rank => {
                let rules = self.leveling_of(guild_id);
                let tier = |m: &Member| rules.tiers.iter().rposition(|t| m.xp >= t.xp);
                v.sort_by(|a, b| {
                    b.online
                        .cmp(&a.online)
                        .then_with(|| tier(b).cmp(&tier(a)))
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
        // An app is named by each guild that installed it, so the open one wins.
        let here = |m: &&Member| Some(m.guild_id) == self.selected_guild;
        let rows = || self.members.iter().filter(|m| m.user.pubkey == pubkey);
        rows().find(here).or_else(|| rows().next()).map(|m| &m.user)
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
    let rev = use_memo(move || state.read().read_marks_revision);
    use_effect(move || {
        let _ = rev();
        let marks: Vec<(String, i64)> = state
            .peek()
            .dm_read_at
            .iter()
            .map(|(peer, at)| (peer.clone(), *at))
            .collect();
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

pub fn use_channel_read_persistence(state: Signal<AppState>) {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let rev = use_memo(move || state.read().read_marks_revision);
    use_effect(move || {
        let _ = rev();
        let marks = state.peek().channel_read_at.clone();
        // `peek`, not `read`: writing back what this effect subscribes to is a
        // loop.
        if settings.peek().channel_read_at != marks {
            let mut next = settings.peek().clone();
            next.channel_read_at = marks;
            settings.set(next.clone());
            crate::settings::save(&next);
        }
    });
}

/// A roster exists only while connected, so a guild-mate is accepted for good
/// once seen; on Home their next DM would otherwise be a request.
/// The one writer of `dm_pane_open`, mounted by the one component per screen
/// that holds the conversation; a parent and a child both writing raced on Android.
pub fn use_dm_pane(mut state: Signal<AppState>, open: impl Fn() -> bool + 'static) {
    use_effect(move || {
        let open = open();
        if state.peek().dm_pane_open != open {
            state.write().dm_pane_open = open;
        }
    });
    use_drop(move || {
        // The account's state may be gone first when the whole account unmounts.
        if let Ok(mut state) = state.try_write() {
            state.dm_pane_open = false;
        }
    });
}

pub fn use_dm_standing_persistence(mut state: Signal<AppState>) {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let guild_mates = use_memo(move || state.read().guild_mates_to_accept().next().is_some());
    use_effect(move || {
        if guild_mates() {
            state.write().accept_guild_mates();
        }
    });
    let standing = use_memo(move || {
        let s = state.read();
        let sorted = |it: &mut dyn Iterator<Item = String>| {
            let mut v: Vec<String> = it.collect();
            v.sort();
            v
        };
        let (accepted, ignored) = s.dm_choice_lists();
        (
            accepted,
            ignored,
            sorted(&mut s.mutes.pubkeys().map(str::to_string)),
        )
    });
    use_effect(move || {
        let (accepted, ignored, blocked) = standing();
        let saved = settings.peek();
        if saved.dm_accepted == accepted
            && saved.dm_ignored == ignored
            && saved.dm_blocked == blocked
        {
            return;
        }
        let mut next = saved.clone();
        drop(saved);
        next.dm_accepted = accepted;
        next.dm_ignored = ignored;
        next.dm_blocked = blocked;
        settings.set(next.clone());
        crate::settings::save(&next);
    });
}

/// Writes the saved emoji back to disk when they change.
pub fn use_saved_emoji_persistence(state: Signal<AppState>) {
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let saved = use_memo(move || state.read().saved_emoji.clone());
    use_effect(move || {
        let saved = saved();
        // `peek`: writing back what this effect subscribes to is a loop.
        if settings.peek().saved_emoji == saved {
            return;
        }
        settings.write().saved_emoji = saved;
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

    #[test]
    fn account_pictures_survive_server_teardown_without_leaking_to_other_users() {
        let mut app = AppState::empty();
        app.account_pictures = Some((
            "self".into(),
            crate::profile::LocalProfile {
                avatar: Some("data:image/png;base64,YXZhdGFy".into()),
                banner: Some("data:image/png;base64,YmFubmVy".into()),
                ..Default::default()
            }
            .into(),
        ));
        app.clear_server_session();
        assert_eq!(
            app.avatar_address("self"),
            Some("data:image/png;base64,YXZhdGFy")
        );
        assert_eq!(
            app.banner_address("self"),
            Some("data:image/png;base64,YmFubmVy")
        );
        assert_eq!(app.avatar_address("other"), None);
        app.profiles.insert(
            "self".into(),
            Profile {
                pubkey: "self".into(),
                ..Default::default()
            },
        );
        assert_eq!(
            app.avatar_address("self"),
            None,
            "server removal is authoritative"
        );
        app.clear_server_session();
        app.account_pictures.as_mut().unwrap().1.avatar = None;
        assert_eq!(
            app.avatar_address("self"),
            None,
            "removing the saved image restores the initial"
        );
    }

    /// Each session field is filled and must come back empty; the pattern at the
    /// end names every field, so a new one does not compile until it is listed.
    macro_rules! after_a_server {
        (
            $s:ident;
            session { $($field:ident = $value:expr => |$v:ident| $emptied:expr;)* }
            account { $($kept:ident),* $(,)? }
        ) => {{
            $( $s.$field = $value; )*
            let before = $s.clone();
            $s.clear_server_session();
            $({
                let $v = &$s.$field;
                assert!($emptied, "{} outlived its server", stringify!($field));
            })*
            let AppState { $($field: _,)* $($kept: _,)* } = &$s;
            before
        }};
    }

    /// An imported guild keeps its id and a bot its key on another host, so
    /// anything one server said would be read by the next (trap 7).
    #[test]
    fn the_next_server_sees_nothing_of_the_last_and_the_account_keeps_everything() {
        let guild = Id::new_v4();
        let channel = Id::new_v4();
        let peer = "peer";
        let mut s = AppState::empty();
        let dm = dm_with(&mut s, peer, &[1]);
        s.identity = Some(crate::identity::Identity::create("Account").unwrap());
        s.choose_dm(peer, Some(DmChoice::Accepted));
        s.nostr_relays_up.insert("wss://relay.example".into());
        s.nostr_names.insert(peer.into(), ("Peer".into(), 1));
        s.contacts =
            crate::nostr::nip02::ContactList::default().with(crate::nostr::nip02::Contact {
                pubkey: peer.into(),
                relay: None,
                petname: None,
            });
        s.relay_app_manifests.insert("bot".into(), (1, "{}".into()));
        s.last_text_channels
            .push(crate::channel_selection::RememberedTextChannel {
                server: "server-b".into(),
                guild_id: guild,
                channel_id: channel,
            });
        s.channel_read_at
            .insert("server-b".into(), BTreeMap::from([(channel, 1)]));
        s.user_volumes.insert(peer.into(), 50);
        s.mic_sensitivity = 7;
        s.selected_input_device = Some("mic".into());
        s.voice_session_epoch = 9;
        s.readies = 3;
        s.error_toast = Some("kept".into());
        let guild_channel = {
            let mut other = AppState::empty();
            text_channel(&mut other, guild);
            other.channels
        };
        let mut camera = crate::local_camera::LocalCamera::default();
        let _ = camera.on(CameraEvent::Started);
        let mut share = crate::local_share::ScreenShare::default();
        share.on(ShareEvent::WebViewStarted {
            native_audio: false,
        });
        let mut watches = crate::stream_placement::StreamWatches::default();
        watches.watch(peer);
        let mut voice = VoiceSession::default();
        voice.force(CallState::Connected(channel));
        voice.on(VoiceEvent::JoinSent(Id::new_v4()));
        let mut presence = crate::stream_audio::Presence::default();
        presence.set(crate::stream_audio::Source::Voice, peer, true);
        let mut emoji = crate::media_cache::MediaCache::default();
        emoji.insert("a.png".into(), "data:image/png;base64,AA==".into());
        let token = || Some(("wss://sfu".to_string(), "token".to_string()));
        let route = || {
            Some(crate::connection_routes::MediaRoute {
                family: "IPv4",
                relayed: false,
                protocol: "udp".into(),
                endpoint: "host".into(),
            })
        };
        fn json<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> T {
            serde_json::from_value(value).unwrap()
        }

        let before = after_a_server! { s;
            session {
                status = ConnectionStatus::Ready => |v| *v == ConnectionStatus::Disconnected;
                self_user = Some(user("self")) => |v| v.as_ref().is_some_and(|me| me.pubkey == "self" && me.username == "Account");
                guilds = vec![json(serde_json::json!({ "id": guild, "name": "g", "icon": null }))] => |v| v.is_empty();
                channels = guild_channel => |v| v.is_empty();
                members = vec![member(peer, true)] => |v| v.is_empty();
                messages = BTreeMap::from([(channel, Vec::new()), (dm, s.messages[&dm].clone())]) => |v| v.keys().eq([&dm]);
                history_loaded = HashSet::from([channel]) => |v| v.is_empty();
                message_search = Some((channel, Id::new_v4(), Vec::new())) => |v| v.is_none();
                message_jump = Some((channel, Id::new_v4(), 1)) => |v| v.is_none();
                file_upload_result = Some((Id::new_v4(), None)) => |v| v.is_none();
                server_chat_tools = true => |v| !*v;
                server_image_previews = true => |v| !*v;
                server_progressive_image_uploads = true => |v| !*v;
                guild_file_policies = BTreeMap::from([(guild, Default::default())]) => |v| v.is_empty();
                guild_stickers = BTreeMap::from([(guild, Vec::new())]) => |v| v.is_empty();
                voice_states = vec![VoiceState {
                    user_pubkey: peer.into(),
                    guild_id: guild,
                    channel_id: Some(channel),
                    muted: false,
                    deafened: false,
                    speaking: false,
                    camera_on: false,
                    screen_sharing: true,
                    screen_watching: Vec::new(),
                    camera_watching: Vec::new(),
                }] => |v| v.is_empty();
                selected_guild = Some(guild) => |v| v.is_none();
                selected_channel = Some(channel) => |v| v.is_none();
                dm_mode = true => |v| !*v;
                channel_unread = HashSet::from([channel]) => |v| v.is_empty();
                channel_mentions = HashMap::from([(channel, 1)]) => |v| v.is_empty();
                channel_latest = HashMap::from([(channel, 1)]) => |v| v.is_empty();
                pending_edits = HashSet::from([Id::new_v4()]) => |v| v.is_empty();
                catalog = vec![GuildSummary { id: guild, name: "g".into(), icon: None, member_count: 1 }] => |v| v.is_empty();
                catalog_total = 1 => |v| *v == 0;
                guild_join_confirmation = Some((1, guild)) => |v| v.is_none();
                profiles = HashMap::from([(peer.to_string(), Profile::default())]) => |v| v.is_empty();
                server_origin = Some("server-a".into()) => |v| v.is_none();
                rendezvous_url = Some("wss://rendezvous".into()) => |v| v.is_none();
                resolved_share = Some("quic://share".into()) => |v| v.is_none();
                activities = HashMap::from([(peer.to_string(), json(serde_json::json!({ "name": "game" })))]) => |v| v.is_empty();
                profile_card = Some(peer.into()) => |v| v.is_none();
                command_form = Some(CommandForm { bot_pubkey: "bot".into(), command: "roll".into() }) => |v| v.is_none();
                image_viewer = Some(crate::features::image_viewer::ImageView::new(Default::default(), None)) => |v| v.is_none();
                guild_dialog = Some(GuildDialog::Settings(guild)) => |v| v.is_none();
                rules_prompt = Some(prompt(None)) => |v| v.is_none();
                audio_settings = true => |v| !*v;
                server_settings_open = true => |v| !*v;
                topology_open = true => |v| !*v;
                channel_access_open = Some(channel) => |v| v.is_none();
                typing = HashMap::from([(channel, HashMap::new())]) => |v| v.is_empty();
                sfu_tunnel_url = Some("ws://127.0.0.1:1/sfu".into()) => |v| v.is_none();
                gateway_route = Some("direct".into()) => |v| v.is_none();
                voice_route_revision = 1 => |v| *v == 0;
                screen_audio_joined = true => |v| !*v;
                ice_servers = vec![Default::default()] => |v| v.is_empty();
                screen_shares = HashMap::from([(channel, vec![peer.to_string()])]) => |v| v.is_empty();
                replying_to = Some(ReplyDraft {
                    message_id: Id::new_v4(),
                    channel_id: channel,
                    author_pubkey: peer.into(),
                    author_username: "Peer".into(),
                    excerpt: "hi".into(),
                }) => |v| v.is_none();
                voice_uplink_congested = true => |v| !*v;
                voice_ping_ms = Some(1) => |v| v.is_none();
                voice_quality = HashMap::from([(peer.to_string(), ConnectionHealth::Good)]) => |v| v.is_empty();
                voice_stats = HashMap::from([(peer.to_string(), TrackStats::Unavailable)]) => |v| v.is_empty();
                screen_share_stats = Some(Default::default()) => |v| v.is_none();
                screen_share_in_stats = Some(Default::default()) => |v| v.is_none();
                soundboard_open = true => |v| !*v;
                soundboard_adjusting = true => |v| !*v;
                soundboard_anchor = (1.0, 2.0) => |v| *v == (0.0, 0.0);
                recent_sounds = HashMap::from([(peer.to_string(), ("horn".to_string(), std::time::Instant::now()))]) => |v| v.is_empty();
                voice_menu = Some(VoiceMenu { pubkey: peer.into(), name: "Peer".into(), x: 1.0, y: 1.0 }) => |v| v.is_none();
                stream_has_audio = presence => |v| !v.contains(peer);
                media_undecryptable = true => |v| !*v;
                pending_rekey = true => |v| !*v;
                voice_turnover = 3 => |v| *v == 0;
                host_info = Some(HostInfo {
                    offered_routes: Vec::new(),
                    connections: Vec::new(),
                    gateway_addr: "127.0.0.1:1".parse().unwrap(),
                    livekit_url: String::new(),
                    local_url: String::new(),
                    share: None,
                    voice_bundled: true,
                    voice_reason: String::new(),
                    shortcode: None,
                    publish_error: None,
                    listed_public: false,
                    reachability: crate::host::Reachability::LoopbackOnly,
                }) => |v| v.is_none();
                transport = Transport::Quic => |v| *v == Transport::Loopback;
                integrations = HashMap::from([(guild, Vec::new())]) => |v| v.is_empty();
                app_manifest_events = HashMap::from([("bot".to_string(), Some("{}".to_string()))]) => |v| v.is_empty();
                guild_activities = HashMap::from([(guild, Vec::new())]) => |v| v.is_empty();
                activity_instances = HashMap::from([(channel, Vec::new())]) => |v| v.is_empty();
                guild_blocklists = HashMap::from([(guild, (vec!["curator".to_string()], Vec::new()))]) => |v| v.is_empty();
                bot_commands = HashMap::from([("bot".to_string(), Vec::new())]) => |v| v.is_empty();
                command_notes = vec![CommandNote {
                    invocation_id: Id::new_v4(),
                    bot_pubkey: "bot".into(),
                    channel_id: channel,
                    content: "secret".into(),
                }] => |v| v.is_empty();
                guild_emojis = HashMap::from([(guild, Vec::new())]) => |v| v.is_empty();
                guild_sounds = HashMap::from([(guild, Vec::new())]) => |v| v.is_empty();
                emoji_images = emoji => |v| v.get("a.png").is_none();
                emoji_requested = HashMap::from([("a.png".to_string(), std::time::Instant::now())]) => |v| v.is_empty();
                roles = HashMap::from([(guild, Vec::new())]) => |v| v.is_empty();
                bans = HashMap::from([(guild, Vec::new())]) => |v| v.is_empty();
                invites = HashMap::from([(guild, "code".to_string())]) => |v| v.is_empty();
                open_guild_on_ready = Some(guild) => |v| v.is_none();
                voice_invite_on_ready = Some(PendingVoiceInvite {
                    guild_id: guild,
                    channel_id: channel,
                    code: None,
                    expires_at: None,
                }) => |v| v.is_none();
                gateway_failure = Some("refused".into()) => |v| v.is_none();
                is_operator = true => |v| !*v;
                server_agents = vec![json(serde_json::json!({
                    "pubkey": "agent",
                    "spec": {
                        "name": "agent",
                        "provider": "anthropic",
                        "provider_label": "",
                        "base_url": "",
                        "model": "",
                        "per_member_per_hour": 0,
                        "daily_tokens": 0,
                    },
                }))] => |v| v.is_empty();
                server_apps = Some(ServerApps {
                    policy: Default::default(),
                    approved: Vec::new(),
                    banned: Vec::new(),
                    installs: Vec::new(),
                }) => |v| v.is_none();
                offered_agents = vec![crate::protocol::OfferedAgent {
                    pubkey: "agent".into(),
                    name: "agent".into(),
                    provider_label: String::new(),
                    model: String::new(),
                    guild_daily_tokens: 0,
                }] => |v| v.is_empty();
                default_guild = Some(guild) => |v| v.is_none();
                server_info = crate::protocol::ServerInfo {
                    name: Some("server a".into()),
                    ..Default::default()
                } => |v| *v == Default::default();
                server_settings = Some(crate::protocol::ServerSettingsView {
                    settings: Default::default(),
                    pinned: Default::default(),
                    source: Default::default(),
                    publishes: false,
                    media_used_bytes: 0,
                }) => |v| v.is_none();
                audit_logs = HashMap::from([(guild, Vec::new())]) => |v| v.is_empty();

                voice = voice => |v| v.channel_id().is_none() && v.joining_channel_id().is_none();
                media_key = crate::mediakey::CallKey::force(channel, (1, [7; crate::mediakey::KEY_LEN])) => |v| v.channel().is_none();
                camera = camera => |v| !v.wanted();
                cameras_watching = HashSet::from([peer.to_string()]) => |v| v.is_empty();
                screen_share = share => |v| !v.is_live();
                screen_viewing = watches => |v| v.is_empty();
                screen_token = token() => |v| v.is_none();
                screen_audio_token = token() => |v| v.is_none();
                screen_video_token = token() => |v| v.is_none();
                screen_viewer_token = token() => |v| v.is_none();
                voice_endpoint = Some("wss://sfu".into()) => |v| v.is_none();
                voice_location = Some("host".into()) => |v| v.is_none();
                voice_send_route = route() => |v| v.is_none();
                voice_receive_route = route() => |v| v.is_none();
                peer_routes = HashMap::from([(peer.to_string(), PeerRoutes {
                    gateway: "direct".into(),
                    sent: None,
                    received: None,
                })]) => |v| v.is_empty();
            }
            account {
                identity, profile_synced_at, account_pictures, account_dnd,
                last_text_channels, channel_read_at, muted_channels, muted_guilds,
                unread_from, dms, dm_emoji, saved_emoji, dm_unread, dm_cleared_at,
                dm_read_at, dm_clock_offset, nostr_event_ids, dm_deleted_ids,
                pending_dm_edits, dm_edit_versions, pending_dm_deletions, dm_delivery,
                dm_pending, dm_images, dm_call_history, dm_call, contacts, mutes,
                dm_choices, nostr_relays_up, nostr_names, relay_app_manifests,
                relay_blocklists, global_xp, dm_pane_open, app_foreground,
                dm_notification_open, mention_tap, error_toast, agents_open,
                composer_focus_request, screen_capture_available,
                camera_capture_available, available_cameras, available_input_devices,
                available_output_devices, active_input_device, active_output_device,
                selected_input_device, selected_output_device, mic_test_active,
                mic_sensitivity, mic_volume, mic_processing, auto_gain_control,
                mic_level, mic_gate_level, mic_level_pre, noise_cancellation,
                denoise_atten_lim_db, bypass_system_audio_processing, mic_bypass_error,
                denoise_fallback, denoise_retry, voice_bitrate_kbps, user_volumes,
                user_muted, stream_volumes, stream_muted, soundboard_volume,
                voice_session_epoch, read_marks_revision, catalog_revision, notify_tick,
                dm_notify_tick, readies,
            }
        };

        assert_eq!(s.dms, before.dms);
        assert_eq!(s.messages[&dm], before.messages[&dm]);
        assert_eq!(s.dm_unread(dm), before.dm_unread(dm));
        assert_eq!(s.dm_choice(peer), Some(DmChoice::Accepted));
        assert_eq!(
            s.identity.as_ref().map(|i| &i.pubkey),
            before.identity.as_ref().map(|i| &i.pubkey)
        );
        assert_eq!(s.nostr_relays_up, before.nostr_relays_up);
        assert_eq!(s.nostr_names, before.nostr_names);
        assert!(s.contacts.contains(peer));
        assert_eq!(s.relay_app_manifests, before.relay_app_manifests);
        assert_eq!(s.last_text_channels, before.last_text_channels);
        assert_eq!(s.channel_read_at, before.channel_read_at);
        assert_eq!(s.user_volumes, before.user_volumes);
        assert_eq!(s.mic_sensitivity, 7);
        assert_eq!(s.selected_input_device, before.selected_input_device);
        assert!(
            s.voice_session_epoch > before.voice_session_epoch,
            "a call ended"
        );
        assert_eq!(s.readies, 3);
        assert_eq!(s.error_toast, before.error_toast);
    }

    /// Home has our own user and a dialled session has its origin, so neither
    /// can say whether there is a session.
    #[test]
    fn only_the_gateway_status_says_whether_a_session_exists() {
        let mut s = AppState::empty();
        s.self_user = Some(user("self"));
        s.server_origin = Some("server-a".into());
        for (status, session) in [
            (ConnectionStatus::Disconnected, ServerSession::Absent),
            (ConnectionStatus::Connecting, ServerSession::Connecting),
            (ConnectionStatus::Reconnecting, ServerSession::Connecting),
            (ConnectionStatus::Ready, ServerSession::Live),
        ] {
            s.status = status;
            assert_eq!(s.server_session(), session, "{status:?}");
        }
        s.clear_server_session();
        assert_eq!(s.server_session(), ServerSession::Absent);
        assert!(s.self_user.is_some(), "we are still ourselves at Home");
    }

    /// `short.png` fails `emoji::safe_name`, so nothing touches the disk.
    #[test]
    fn saving_an_emoji_needs_its_picture_refuses_a_taken_name_and_renames_itself() {
        let mut s = AppState::empty();
        let gid = uuid::Uuid::new_v4();
        assert!(
            s.save_emoji("cat", "short.png", gid)
                .unwrap_err()
                .contains("not loaded")
        );
        s.emoji_images
            .insert("short.png".into(), "data:image/png;base64,AA==".into());
        s.emoji_images
            .insert("other.png".into(), "data:image/png;base64,AA==".into());
        assert!(s.save_emoji("Bad Name", "short.png", gid).is_err());
        s.save_emoji(" Cat ", "short.png", gid).unwrap();
        assert_eq!(s.saved_emoji[0].shortcode, "cat", "trimmed and lowercased");
        assert!(
            s.save_emoji("cat", "other.png", gid)
                .unwrap_err()
                .contains("already")
        );
        s.save_emoji("kitty", "short.png", gid).unwrap();
        assert_eq!(s.saved_emoji.len(), 1, "the same picture again is a rename");
        assert_eq!(s.saved_emoji_of("short.png").unwrap().shortcode, "kitty");
        s.remove_saved_emoji("short.png");
        assert!(s.saved_emoji.is_empty());
    }
    use super::*;
    use crate::protocol::{Member, Profile, User};

    #[test]
    fn the_open_guild_dresses_an_app_and_elsewhere_its_profile_shows() {
        let mut s = AppState::empty();
        let (ours, theirs) = (Id::new_v4(), Id::new_v4());
        let row = |guild_id, username: &str, look| Member {
            user: User {
                pubkey: "app".into(),
                username: username.into(),
            },
            guild_id,
            online: true,
            bot: true,
            roles: Vec::new(),
            xp: 0,
            look,
            agent: None,
            reads: None,
        };
        let dressed = crate::protocol::GuildLook {
            avatar: Some("data:image/png;base64,ours".into()),
            banner: None,
            bio: Some("Ours".into()),
        };
        s.members = vec![
            row(theirs, "Theirs", Default::default()),
            row(ours, "Ours", dressed),
        ];
        s.profiles.insert(
            "app".into(),
            Profile {
                pubkey: "app".into(),
                avatar: Some("data:image/png;base64,own".into()),
                banner: Some("data:image/png;base64,banner".into()),
                bio: Some("Own".into()),
                ..Default::default()
            },
        );

        s.selected_guild = Some(ours);
        assert_eq!(s.avatar_address("app"), Some("data:image/png;base64,ours"));
        assert_eq!(
            s.banner_address("app"),
            Some("data:image/png;base64,banner")
        );
        assert_eq!(s.bio_of("app"), Some("Ours"));
        assert_eq!(s.display_name("app"), "Ours");

        s.selected_guild = Some(theirs);
        assert_eq!(s.avatar_address("app"), Some("data:image/png;base64,own"));
        assert_eq!(s.bio_of("app"), Some("Own"));
        assert_eq!(s.display_name("app"), "Theirs");
    }

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
            camera_watching: Vec::new(),
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

    #[test]
    fn kind_0_refreshes_shared_rosters_without_renaming_bots() {
        let mut s = AppState::empty();
        let mut first = member("peer", true);
        first.user.username = "Before".into();
        let mut second = first.clone();
        second.guild_id = Id::new_v4();
        let mut bot = first.clone();
        bot.bot = true;
        bot.guild_id = Id::new_v4();
        s.members = vec![first, second, bot];
        assert!(s.note_name("peer", "After".into(), 200));
        assert_eq!(s.display_name("peer"), "After");
        assert_eq!(s.members[0].user.username, "After");
        assert_eq!(s.members[1].user.username, "After");
        assert_eq!(s.members[2].user.username, "Before");
        assert!(!s.note_name("peer", "Before".into(), 100));
        assert!(!s.note_name("peer", "Duplicate".into(), 200));
        assert_eq!(s.members[0].user.username, "After");
        assert_eq!(s.members[1].user.username, "After");
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
            author_bot: false,
            edited_at: None,
            image_metadata: None,
            attachment: None,
            pinned: false,
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

    #[test]
    fn a_completed_progressive_image_replaces_its_placeholder_in_place() {
        let mut state = AppState::empty();
        let channel_id = Id::new_v4();
        let id = Id::new_v4();
        let mut pending = at(id, channel_id, 100);
        pending.image = Some("media:preview.png".into());
        pending.image_metadata = Some(crate::protocol::ImageMetadata {
            name: Some("original.png".into()),
            width: None,
            height: None,
            preview: Some("media:preview.png".into()),
            upload_pending: true,
        });
        state.insert_message(channel_id, pending);

        let mut completed = at(id, channel_id, 100);
        completed.image = Some("media:original.png".into());
        completed.image_metadata = Some(crate::protocol::ImageMetadata {
            name: Some("original.png".into()),
            width: Some(800),
            height: Some(600),
            preview: Some("media:preview.png".into()),
            upload_pending: false,
        });

        assert!(state.complete_pending_image(&completed));
        assert_eq!(state.messages[&channel_id].len(), 1);
        assert_eq!(state.messages[&channel_id][0], completed);
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

    #[test]
    fn a_live_message_does_not_count_as_fetched_history() {
        let mut s = AppState::empty();
        let cid = Id::new_v4();
        s.insert_message(cid, at(Id::new_v4(), cid, 200));
        assert!(!s.history_loaded.contains(&cid));

        s.merge_history(cid, Vec::new());
        assert!(
            s.history_loaded.contains(&cid),
            "an empty channel is loaded too"
        );
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

    #[test]
    fn an_edit_shows_at_once_and_the_servers_answer_settles_it() {
        let mut s = AppState::empty();
        let cid = Id::new_v4();
        let mid = Id::new_v4();
        s.insert_message(cid, at(mid, cid, 100));

        s.edit_locally(cid, mid, "fixed".into());
        assert_eq!(s.messages[&cid][0].content, "fixed");
        assert!(s.messages[&cid][0].edited_at.is_some());
        assert!(s.pending_edits.contains(&mid));

        s.apply_message_edit(cid, mid, "t100".into(), None);
        assert_eq!(
            s.messages[&cid][0].content, "t100",
            "a refusal puts it back"
        );
        assert!(s.messages[&cid][0].edited_at.is_none());
        assert!(s.pending_edits.is_empty());
    }

    fn text_channel(s: &mut AppState, guild_id: Id) -> Id {
        let id = Id::new_v4();
        s.channels.push(Channel {
            divider: Default::default(),
            id,
            guild_id,
            name: "general".into(),
            kind: crate::protocol::ChannelKind::Text,
            topic: None,
            read_only: false,
            slowmode_secs: 0,
            markdown: false,
            position: 0,
            access: None,
        });
        id
    }

    #[test]
    fn guild_unread_tracks_other_channels_and_clears_only_the_opened_channel() {
        let mut s = AppState::empty();
        s.self_user = Some(user("self"));
        let guild = Id::new_v4();
        let first = text_channel(&mut s, guild);
        let second = text_channel(&mut s, guild);
        let other_guild = Id::new_v4();
        let other = text_channel(&mut s, other_guild);
        s.selected_channel = Some(first);
        s.dm_mode = false;
        s.insert_message(first, at(Id::new_v4(), first, 1));
        assert!(!s.guild_has_unread(guild));
        let message = at(Id::new_v4(), second, 2);
        s.insert_message(second, message.clone());
        s.insert_message(other, at(Id::new_v4(), other, 3));
        assert!(s.guild_has_unread(guild));
        assert!(s.guild_has_unread(other_guild));
        s.mark_channel_read(second);
        assert!(!s.guild_has_unread(guild));
        assert!(s.guild_has_unread(other_guild));
        assert!(!s.insert_message(second, message));
        assert!(!s.guild_has_unread(guild));
        let mut own = at(Id::new_v4(), second, 4);
        own.author = user("self");
        s.insert_message(second, own);
        assert!(!s.guild_has_unread(guild));
        s.dm_mode = true;
        s.insert_message(first, at(Id::new_v4(), first, 5));
        assert!(s.channel_unread.contains(&first));
        s.clear_server_session();
        assert!(s.channel_unread.is_empty());
    }

    /// What arrived while the app was closed shows as unread on the next
    /// Ready, measured against marks only this machine holds.
    #[test]
    fn opening_an_unread_channel_keeps_the_mark_it_had() {
        let guild = Id::new_v4();
        let mut s = AppState::empty();
        s.server_origin = Some("server-a".into());
        let channel = text_channel(&mut s, guild);
        let other = text_channel(&mut s, guild);
        s.note_latest_messages(HashMap::from([(channel, 100), (other, 100)]));
        s.note_latest_messages(HashMap::from([(channel, 250)]));
        s.mark_channel_read(channel);
        assert_eq!(
            s.unread_from,
            Some((channel, 100)),
            "the mark before opening"
        );
        s.mark_channel_read(other);
        assert_eq!(
            s.unread_from,
            Some((channel, 100)),
            "another read channel leaves it"
        );
        s.mark_channel_read(channel);
        assert_eq!(s.unread_from, None, "reopened once read, nothing is new");
    }

    #[test]
    fn read_marks_outlive_the_session_and_meet_readys_newest_times() {
        let guild = Id::new_v4();
        let mut s = AppState::empty();
        s.server_origin = Some("server-a".into());
        s.self_user = Some(user("self"));
        let seen = text_channel(&mut s, guild);
        let opened = text_channel(&mut s, guild);
        s.note_latest_messages(HashMap::from([(seen, 100), (opened, 100)]));
        assert!(
            s.channel_unread.is_empty(),
            "no mark yet is read, not unread"
        );
        s.insert_message(opened, at(Id::new_v4(), opened, 1));
        s.mark_channel_read(opened);

        let mut next = AppState::empty();
        next.server_origin = Some("server-a".into());
        next.channel_read_at = s.channel_read_at.clone();
        next.channels = s.channels.clone();
        next.channel_read_at
            .entry("server-b".into())
            .or_default()
            .insert(Id::new_v4(), 5);
        let gone = Id::new_v4();
        next.channel_read_at
            .get_mut("server-a")
            .unwrap()
            .insert(gone, 5);
        next.prune_read_marks();
        assert!(!next.channel_read_at["server-a"].contains_key(&gone));
        assert_eq!(
            next.channel_read_at["server-b"].len(),
            1,
            "another server's marks stay"
        );

        next.note_latest_messages(HashMap::from([(seen, 200), (opened, 1_000)]));
        assert!(next.channel_unread.contains(&seen));
        assert!(
            !next.channel_unread.contains(&opened),
            "read up to its newest"
        );
        next.mark_channel_read(seen);
        assert_eq!(next.channel_read_at["server-a"][&seen], 200);

        next.selected_channel = Some(seen);
        next.note_latest_messages(HashMap::from([(seen, 300)]));
        assert!(
            !next.channel_unread.contains(&seen),
            "what is on screen is read"
        );
        assert_eq!(next.channel_read_at["server-a"][&seen], 300);
    }

    /// The relays replay a conversation before another device's marks may
    /// arrive; adopting them takes back what that device already read.
    #[test]
    fn another_devices_dm_marks_recount_the_replayed_unread() {
        let peer = "b".repeat(64);
        let mut s = AppState::empty();
        dm_at(&mut s, &peer, &peer, 100);
        dm_at(&mut s, &peer, &peer, 200);
        let cid = crate::nostr::service::conversation_id(&peer);
        s.dm_unread.insert(cid, 2);
        let marks = |at| crate::nostr::read_marks::ReadMarks {
            dms: [(peer.clone(), at)].into(),
            ..Default::default()
        };

        s.adopt_read_marks(&marks(150));
        assert_eq!(s.dm_unread[&cid], 1);
        s.adopt_read_marks(&marks(200));
        assert!(!s.dm_unread.contains_key(&cid));
        s.adopt_read_marks(&marks(50));
        assert_eq!(
            s.dm_read_at[&peer], 200,
            "a stale copy never moves a mark back"
        );
    }

    #[test]
    fn two_devices_agree_on_two_hundred_reads_without_hiding_newer_messages() {
        let peer = "b".repeat(64);
        let cid = crate::nostr::service::conversation_id(&peer);
        let mut desktop = AppState::empty();
        let mut phone = AppState::empty();
        for at in 1..=200 {
            dm_at(&mut desktop, &peer, &peer, at);
            dm_at(&mut phone, &peer, &peer, at);
        }
        desktop.mark_dm_read(cid);
        phone.dm_unread.insert(cid, 200);
        let marks = desktop.read_marks();
        phone.adopt_read_marks(&marks);
        assert!(!phone.dm_unread.contains_key(&cid));
        phone.choose_dm(&peer, Some(crate::state::DmChoice::Accepted));
        dm_at(&mut phone, &peer, &peer, 201);
        phone.note_dm_arrival(cid, &peer, 201);
        phone.adopt_read_marks(&marks);
        assert_eq!(phone.dm_unread[&cid], 1);
    }

    /// Devices measure a skewed peer's clock a little differently, so a mark
    /// travels on the peer's clock and lands on each device's own.
    #[test]
    fn a_dm_mark_crosses_devices_on_the_peers_clock() {
        let peer = "b".repeat(64);
        let mut a = AppState::empty();
        a.dm_clock_offset.insert(peer.clone(), 900);
        a.dm_read_at.insert(peer.clone(), 1_000);
        let sent = a.read_marks();
        assert_eq!(sent.dms[&peer], 1_900);

        let mut b = AppState::empty();
        b.dm_clock_offset.insert(peer.clone(), 897);
        b.adopt_read_marks(&sent);
        assert_eq!(b.dm_read_at[&peer], 1_003);
    }

    #[test]
    fn another_devices_channel_marks_clear_what_it_read_here() {
        let guild = Id::new_v4();
        let mut s = AppState::empty();
        s.server_origin = Some("server-a".into());
        s.self_user = Some(user("self"));
        let read = text_channel(&mut s, guild);
        let newer = text_channel(&mut s, guild);
        s.note_latest_messages(HashMap::from([(read, 100), (newer, 100)]));
        s.insert_message(read, at(Id::new_v4(), read, 1));
        s.insert_message(newer, at(Id::new_v4(), newer, 2));
        let other = Id::new_v4();
        let marks = crate::nostr::read_marks::ReadMarks {
            channels: [
                (
                    "server-a".to_string(),
                    [(read, 1_000), (newer, 1_500)].into(),
                ),
                ("server-b".to_string(), [(other, 7)].into()),
            ]
            .into(),
            ..Default::default()
        };

        s.adopt_read_marks(&marks);
        assert!(!s.channel_unread.contains(&read));
        assert!(
            s.channel_unread.contains(&newer),
            "it arrived after that device read"
        );
        assert_eq!(s.channel_read_at["server-b"][&other], 7);
    }

    #[test]
    fn a_muted_channel_or_guild_hides_its_unread_dot_until_unmuted() {
        let mut s = AppState::empty();
        s.self_user = Some(user("self"));
        let guild = Id::new_v4();
        let quiet = text_channel(&mut s, guild);
        s.set_channel_muted(quiet, true);
        s.insert_message(quiet, at(Id::new_v4(), quiet, 1));
        assert!(!s.channel_shows_unread(quiet));
        assert!(!s.guild_has_unread(guild));
        s.set_channel_muted(quiet, false);
        assert!(s.channel_shows_unread(quiet));
        assert!(s.guild_has_unread(guild));
        s.muted_guilds.insert(guild);
        assert!(!s.channel_shows_unread(quiet));
        assert!(!s.guild_has_unread(guild));
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

    fn mention(channel_id: Id, secs: i64) -> Message {
        let mut message = at(Id::new_v4(), channel_id, secs);
        message.content = "hey @u-self look".into();
        message
    }

    #[test]
    fn a_mention_counts_until_its_channel_is_read() {
        let mut s = AppState::empty();
        s.self_user = Some(user("self"));
        let guild = Id::new_v4();
        let open = text_channel(&mut s, guild);
        let other = text_channel(&mut s, guild);
        s.selected_channel = Some(open);
        s.insert_message(open, mention(open, 1));
        s.insert_message(other, at(Id::new_v4(), other, 2));
        assert_eq!(s.guild_mention_count(guild), 0, "read on arrival");
        assert_eq!(s.app_badge(), AppBadge::Unread);
        s.insert_message(other, mention(other, 3));
        s.insert_message(other, mention(other, 4));
        assert_eq!(s.channel_mention_count(other), 2);
        assert_eq!(s.guild_mention_count(guild), 2);
        assert_eq!(s.app_badge(), AppBadge::Count(2));
        s.mark_channel_read(other);
        assert_eq!(s.guild_mention_count(guild), 0);
        assert_eq!(s.app_badge(), AppBadge::None);
    }

    /// A channel open behind another app is not being read, so a mention in
    /// it still reaches the dock.
    #[test]
    fn an_open_channel_counts_while_the_app_is_in_the_background() {
        let mut s = AppState::empty();
        s.self_user = Some(user("self"));
        let cid = text_channel(&mut s, Id::new_v4());
        s.selected_channel = Some(cid);
        s.app_foreground = false;
        s.insert_message(cid, mention(cid, 1));
        assert_eq!(s.app_badge(), AppBadge::Count(1));
    }

    #[test]
    fn a_muted_channel_keeps_its_mentions_out_of_sight() {
        let mut s = AppState::empty();
        s.self_user = Some(user("self"));
        let cid = text_channel(&mut s, Id::new_v4());
        s.set_channel_muted(cid, true);
        s.insert_message(cid, mention(cid, 1));
        assert_eq!(s.app_badge(), AppBadge::None);
        s.set_channel_muted(cid, false);
        assert_eq!(s.app_badge(), AppBadge::Count(1));
    }

    #[test]
    fn unread_dms_count_on_the_badge() {
        let mut s = AppState::empty();
        let cid = dm_with(&mut s, "peer", &[100]);
        s.dm_unread.insert(cid, 3);
        assert_eq!(s.app_badge(), AppBadge::Count(3));
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

    #[test]
    fn preferred_dm_uses_recency_without_replacing_an_explicit_selection() {
        let mut state = AppState::empty();
        assert!(state.preferred_dm().is_none());
        let older = dm_with(&mut state, "older", &[100]);
        let recent = dm_with(&mut state, "recent", &[200]);
        assert_eq!(state.preferred_dm().unwrap().channel_id, recent);
        state.selected_channel = Some(older);
        assert_eq!(state.preferred_dm().unwrap().channel_id, older);
        state.selected_channel = Some(Id::new_v4());
        state.choose_dm("recent", Some(crate::state::DmChoice::Ignored));
        assert_eq!(state.preferred_dm().unwrap().channel_id, older);
    }

    fn dm_with(s: &mut AppState, peer: &str, ats: &[i64]) -> Id {
        s.choose_dm(peer, Some(crate::state::DmChoice::Accepted));
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
                author_bot: false,
                edited_at: None,
                image_metadata: None,
                attachment: None,
                pinned: false,
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
        s.dm_pane_open = true;
        s.selected_channel = Some(cid);

        let rev_before = s.read_marks_revision;
        s.note_dm_arrival(cid, peer, 300);

        assert!(s.dm_unread.is_empty());
        assert_eq!(s.dm_read_at.get(peer), Some(&300));
        assert_eq!(s.read_marks_revision, rev_before.wrapping_add(1));

        // An older message arriving while watching does not bump or regress mark
        s.note_dm_arrival(cid, peer, 200);
        assert_eq!(s.dm_read_at.get(peer), Some(&300));
        assert_eq!(s.read_marks_revision, rev_before.wrapping_add(1));
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

    /// Selected, DM mode, the pane shown and the app in front: all four, or no
    /// conversation is being read.
    #[test]
    fn a_conversation_is_watched_only_with_every_fact() {
        let peer = "abcd";
        for foreground in [false, true] {
            for dm_mode in [false, true] {
                for pane in [false, true] {
                    for selected in [false, true] {
                        let mut s = AppState::empty();
                        let cid = dm_with(&mut s, peer, &[]);
                        s.app_foreground = foreground;
                        s.dm_mode = dm_mode;
                        s.dm_pane_open = pane;
                        s.selected_channel = selected.then_some(cid);
                        let shown = dm_mode && pane && selected;
                        let label = format!("{foreground} {dm_mode} {pane} {selected}");
                        assert_eq!(s.dm_in_view(), shown.then_some(cid), "{label}");
                        assert_eq!(
                            s.watched_dm(),
                            (shown && foreground).then_some(cid),
                            "{label}"
                        );
                        s.note_dm_arrival(cid, peer, 100);
                        let counted = !(shown && foreground);
                        assert_eq!(s.dm_unread.contains_key(&cid), counted, "{label}");
                        assert_eq!(s.dm_read_at.contains_key(peer), !counted, "{label}");
                    }
                }
            }
        }
    }

    /// A count left from before the conversation came on screen is not shown
    /// while it is read, though only marking it read removes it.
    #[test]
    fn a_watched_conversation_shows_no_unread() {
        let peer = "abcd";
        let mut s = AppState::empty();
        let cid = dm_with(&mut s, peer, &[100]);
        s.note_dm_arrival(cid, peer, 100);
        assert_eq!((s.dm_unread(cid), s.dm_unread_total()), (1, 1));
        assert_eq!(s.unread_on_screen(), None);
        s.dm_mode = true;
        s.dm_pane_open = true;
        s.selected_channel = Some(cid);
        assert_eq!((s.dm_unread(cid), s.dm_unread_total()), (0, 0));
        assert_eq!(s.unread_on_screen(), Some(cid));
        s.mark_dm_read(cid);
        assert_eq!(s.unread_on_screen(), None);
    }

    #[test]
    fn every_unread_change() {
        let peer = "abcd";
        // Held messages at 100, 200 and 300; (counted, read mark, change, after).
        let table: Vec<(u32, Option<i64>, UnreadChange, u32)> = vec![
            (0, None, UnreadChange::Arrived, 1),
            (2, None, UnreadChange::Arrived, 3),
            (2, None, UnreadChange::Read, 0),
            (0, None, UnreadChange::Read, 0),
            (3, None, UnreadChange::Settle, 3),
            (5, None, UnreadChange::Settle, 3),
            (3, Some(150), UnreadChange::Settle, 2),
            (1, Some(150), UnreadChange::Settle, 1),
            (3, Some(300), UnreadChange::Settle, 0),
            (0, Some(50), UnreadChange::Settle, 0),
        ];
        for (counted, mark, change, after) in table {
            let mut s = AppState::empty();
            let cid = dm_with(&mut s, peer, &[100, 200, 300]);
            if counted > 0 {
                s.dm_unread.insert(cid, counted);
            }
            if let Some(mark) = mark {
                s.dm_read_at.insert(peer.into(), mark);
            }
            s.change_dm_unread(cid, change);
            assert_eq!(
                s.dm_unread(cid),
                after,
                "{counted} past {mark:?} on {change:?}"
            );
            assert_eq!(s.dm_unread.contains_key(&cid), after > 0);
        }
    }

    /// The peer's own call rows are counted once, as the missed call.
    #[test]
    fn a_settled_count_does_not_count_a_call_row_as_a_message() {
        use crate::nostr::call_history::{Outcome, Record, merge};
        let (peer, me) = ("b".repeat(64), "a".repeat(64));
        let mut s = AppState::empty();
        s.choose_dm(&peer, Some(crate::state::DmChoice::Accepted));
        let record = Record {
            version: 1,
            call_id: uuid::Uuid::new_v4(),
            caller: peer.clone(),
            callee: me.clone(),
            started_at: 100,
            outcome: Outcome::Missed,
        };
        merge(&mut s, &me, record, &peer, 145);
        let cid = crate::nostr::service::conversation_id(&peer);
        assert_eq!(s.messages[&cid].len(), 1);
        assert_eq!(s.dm_unread(cid), 1);
        s.dm_unread.insert(cid, 2);
        s.change_dm_unread(cid, UnreadChange::Settle);
        assert_eq!(s.dm_unread(cid), 1);
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

    /// The roster that made someone a guild-mate is gone at Home; the
    /// conversation is not (trap 7), so neither is its place in the Inbox.
    #[test]
    fn leaving_the_server_keeps_a_guild_mate_in_the_inbox_and_an_ignore_ignored() {
        let mut s = AppState::empty();
        for peer in ["mate", "ignored-mate", "bot"] {
            dm_with(&mut s, peer, &[100]);
            s.choose_dm(peer, None);
        }
        s.choose_dm("ignored-mate", Some(DmChoice::Ignored));
        s.members.push(member("mate", true));
        s.members.push(member("ignored-mate", true));
        s.members.push(Member {
            bot: true,
            ..member("bot", true)
        });
        let before: Vec<_> = ["mate", "ignored-mate", "bot"]
            .map(|peer| s.dm_standing(peer))
            .into();
        assert_eq!(
            before,
            vec![DmStanding::Inbox, DmStanding::Inbox, DmStanding::Request]
        );
        s.clear_server_session();
        assert_eq!(s.dm_standing("mate"), DmStanding::Inbox);
        assert_eq!(s.dm_standing("ignored-mate"), DmStanding::Ignored);
        assert_eq!(s.dm_standing("bot"), DmStanding::Request);
        assert_eq!(
            s.dm_choice_lists(),
            (vec!["mate".to_string()], vec!["ignored-mate".to_string()])
        );
    }

    #[test]
    fn a_stranger_waits_in_requests_without_counting_or_ringing() {
        let mut s = AppState::empty();
        let peer = "stranger";
        let cid = dm_with(&mut s, peer, &[100]);
        s.choose_dm(peer, None);

        s.note_dm_arrival(cid, peer, 100);
        assert_eq!(s.dm_standing(peer), DmStanding::Request);
        assert!(s.dm_unread.is_empty());
        assert_eq!(s.dm_notify_tick, 0);
        assert_eq!(s.dm_request_count(), 1);
        assert!(s.dms_by_recency(DmStanding::Inbox).is_empty());
    }

    #[test]
    fn a_contact_or_a_guild_mate_goes_to_the_inbox_but_a_bot_does_not() {
        let mut s = AppState::empty();
        s.contacts = s.contacts.clone().with(crate::nostr::nip02::Contact {
            pubkey: "friend".into(),
            relay: None,
            petname: None,
        });
        s.members.push(member("mate", true));
        s.members.push(Member {
            bot: true,
            ..member("harvester", true)
        });
        assert_eq!(s.dm_standing("friend"), DmStanding::Inbox);
        assert_eq!(s.dm_standing("mate"), DmStanding::Inbox);
        assert_eq!(s.dm_standing("harvester"), DmStanding::Request);
    }

    /// Accepting moves the read mark, or the next launch's replay would count
    /// and ring for everything that arrived while it was a request.
    #[test]
    fn accepting_moves_the_conversation_and_its_read_mark() {
        let mut s = AppState::empty();
        let peer = "stranger";
        let cid = dm_with(&mut s, peer, &[100, 200]);
        s.choose_dm(peer, None);
        s.choose_dm(peer, Some(crate::state::DmChoice::Ignored));

        s.accept_dm(peer);
        assert_eq!(s.dm_standing(peer), DmStanding::Inbox);
        assert_ne!(s.dm_choice(peer), Some(crate::state::DmChoice::Ignored));
        assert_eq!(s.dms_by_recency(DmStanding::Inbox)[0].channel_id, cid);
        s.note_dm_arrival(cid, peer, 200);
        assert!(s.dm_unread.is_empty());
    }

    #[test]
    fn an_ignored_request_leaves_the_list_and_the_count() {
        let mut s = AppState::empty();
        let peer = "stranger";
        let cid = dm_with(&mut s, peer, &[100]);
        s.choose_dm(peer, None);
        s.selected_channel = Some(cid);

        s.ignore_dm(peer);
        assert_eq!(s.dm_standing(peer), DmStanding::Ignored);
        assert_eq!(s.dm_request_count(), 0);
        assert_eq!(s.selected_channel, None);
    }

    /// Blocking outranks every reason to let somebody in, contacts included.
    #[test]
    fn a_mute_drops_the_conversation_whoever_it_is() {
        let mut s = AppState::empty();
        let peer = "pest";
        let cid = dm_with(&mut s, peer, &[100]);
        s.dm_unread.insert(cid, 1);
        s.selected_channel = Some(cid);

        s.set_mutes(crate::nostr::nip51::MuteList::default().with(peer));
        assert_eq!(s.dm_standing(peer), DmStanding::Blocked);
        assert!(s.dms.is_empty());
        assert!(!s.messages.contains_key(&cid));
        assert!(s.dm_unread.is_empty());
        assert_eq!(s.selected_channel, None);
    }

    /// A request's leftover count must not reach the badges.
    #[test]
    fn the_unread_total_counts_the_inbox_only() {
        let mut s = AppState::empty();
        let friend = dm_with(&mut s, "friend", &[]);
        let stranger = dm_with(&mut s, "stranger", &[]);
        s.choose_dm("stranger", None);
        s.dm_unread.insert(friend, 2);
        s.dm_unread.insert(stranger, 5);
        assert_eq!(s.dm_unread_total(), 2);
    }

    fn member(pk: &str, online: bool) -> Member {
        Member {
            user: user(pk),
            guild_id: uuid::Uuid::nil(),
            online,
            bot: false,
            roles: Vec::new(),
            xp: 0,
            look: Default::default(),
            agent: None,
            reads: None,
        }
    }

    #[test]
    fn server_experience_saturates_without_counting_other_members() {
        let mut state = AppState::empty();
        state.self_user = Some(user("self"));
        let mut first = member("self", true);
        first.xp = u64::MAX;
        let mut second = member("self", true);
        second.guild_id = uuid::Uuid::new_v4();
        second.xp = 1;
        let mut other = member("other", true);
        other.xp = 500;
        state.members = vec![first, second, other];
        assert_eq!(state.my_server_xp(), u64::MAX);
    }

    fn profile(pk: &str, status: Option<&str>) -> Profile {
        Profile {
            pubkey: pk.into(),
            status: status.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn only_a_person_leaving_the_calls_guild_changes_the_media_key() {
        let mut s = AppState::empty();
        let (call_guild, other_guild) = (Id::new_v4(), Id::new_v4());
        let channel = text_channel(&mut s, call_guild);
        s.voice.force_channel(Some(channel));
        let mut agent = member("agent", true);
        agent.guild_id = call_guild;
        agent.bot = true;
        let mut person = member("person", true);
        person.guild_id = call_guild;
        s.members.extend([agent, person]);
        assert!(!s.removal_holds_key(call_guild, "agent"));
        assert!(s.removal_holds_key(call_guild, "person"));
        assert!(!s.removal_holds_key(other_guild, "person"));
    }

    /// The key in use is global to the process; a key kept for a call we left
    /// is one the bridge would never apply again on the way back.
    #[test]
    fn leaving_a_call_drops_its_media_key_and_everything_published() {
        let mut s = AppState::empty();
        let channel = uuid::Uuid::from_u128(7);
        s.voice.force(CallState::Connected(channel));
        s.media_key = crate::mediakey::CallKey::force(channel, (3, [9; 32]));
        s.screen_share.on(ShareEvent::Chosen {
            target: crate::sysvideo::Target::Display(0),
            audio: true,
        });
        let _ = s.camera.on(CameraEvent::Started);
        s.screen_token = Some(("url".into(), "tok".into()));
        s.screen_viewer_token = Some(("url".into(), "viewer".into()));
        let epoch = s.voice_session_epoch;
        s.end_voice_locally();
        assert_ne!(
            s.voice_session_epoch, epoch,
            "a rejoin must not pass for this call"
        );
        assert_eq!(s.media_key, Default::default());
        assert!(!s.screen_share.is_live() && s.screen_token.is_none());
        assert_eq!(s.camera.phase(), crate::local_camera::CameraPhase::Off);
        assert!(s.screen_viewer_token.is_none());
        assert_eq!(s.voice.channel_id(), None);
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
    fn talking_lights_green_and_silence_shows_no_ring() {
        assert_eq!(talk_ring(true), "ring-2 ring-[var(--success)]");
        assert_eq!(talk_ring(false), "");
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

    #[test]
    fn evict_inactive_dm_images_unloads_other_channels() {
        let mut s = AppState::empty();
        let ch_a = Id::new_v4();
        let ch_b = Id::new_v4();
        let msg_a = Id::new_v4();
        let msg_b = Id::new_v4();
        let (descriptor, _) = crate::nostr::dm_image::encrypt(
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
        ).unwrap();

        let shown = |channel: Id, data: &str| {
            let viewer = uuid::Uuid::new_v4();
            let mut image = crate::nostr::dm_image::Image::received(channel, descriptor.clone());
            image.see(viewer, false);
            let (token, _) = image.start(viewer).unwrap();
            let mut one = HashMap::from([(Id::nil(), image)]);
            crate::nostr::dm_image::finish_load(&mut one, Id::nil(), token, true, Ok(data.into()));
            (one.remove(&Id::nil()).unwrap(), viewer)
        };
        let (image_a, viewer_a) = shown(ch_a, "data_a");
        s.dm_images.insert(msg_a, image_a);
        s.dm_images.insert(msg_b, shown(ch_b, "data_b").0);
        let waiting = &crate::nostr::dm_image::Status::Waiting;

        s.evict_inactive_dm_images(Some(ch_a));
        assert!(s.dm_images[&msg_a].ready().is_some());
        assert_eq!(s.dm_images[&msg_a].wants(viewer_a), Some(false));
        assert_eq!(s.dm_images[&msg_b].status(), waiting);

        s.evict_inactive_dm_images(None);
        assert_eq!(s.dm_images[&msg_a].status(), waiting);
        assert_eq!(s.dm_images[&msg_a].wants(viewer_a), None);
    }

    #[test]
    fn evict_inactive_dm_images_preserves_dm_history() {
        let mut s = AppState::empty();
        let dm_channel = Id::new_v4();
        let guild_channel = Id::new_v4();

        s.dms.push(DmInfo {
            channel_id: dm_channel,
            other_pubkey: "peer1".into(),
        });

        let dummy_msg = |cid| Message {
            id: Id::new_v4(),
            channel_id: cid,
            author: user("a"),
            content: "test".into(),
            created_at: chrono::Utc::now(),
            edited_at: None,
            reply_to: None,
            image: None,
            image_metadata: None,
            attachment: None,
            reactions: Vec::new(),
            author_bot: false,
            pinned: false,
        };

        s.messages
            .insert(dm_channel, (0..60).map(|_| dummy_msg(dm_channel)).collect());
        s.messages.insert(
            guild_channel,
            (0..60).map(|_| dummy_msg(guild_channel)).collect(),
        );
        s.history_loaded.insert(guild_channel);

        s.evict_inactive_dm_images(Some(dm_channel));
        assert_eq!(s.messages[&dm_channel].len(), 60);
        assert_eq!(s.messages[&guild_channel].len(), 50);
        assert!(!s.history_loaded.contains(&guild_channel));

        s.evict_inactive_dm_images(None);
        assert_eq!(s.messages[&dm_channel].len(), 60);
    }
}
