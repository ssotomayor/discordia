//! App-wide state shared via Dioxus context.

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
    /// Connect to a remote dioxusfun-server.
    Remote { server_url: String },
    /// Start the embedded dioxusfun-server on localhost and connect to it.
    /// `allow_lan` lets friends on the same network reach this host directly.
    /// `rendezvous_url` (when Some) makes the host register with a rendezvous
    /// server, surface a shortcode, and accept friends arriving via the
    /// rendezvous proxy.
    SelfHost {
        allow_lan: bool,
        rendezvous_url: Option<String>,
        /// Friendly name shown in `GET /discover` browse listings.
        publish_name: Option<String>,
        /// One-line description shown next to the name in the browse tab.
        description: Option<String>,
        /// Opt in to the public listing.
        publish_public: bool,
    },
    /// Join someone else's host by shortcode through a rendezvous server.
    ByCode {
        rendezvous_url: String,
        code: String,
    },
}

impl SessionMode {
    /// What to call this session in the UI.
    ///
    /// A gateway has no name of its own on this wire — nothing in the protocol
    /// asks a host what it is called — so the honest label is the thing the
    /// person used to get here: the code they were given, the address they
    /// typed, or the fact that it is this machine.
    pub fn label(&self) -> String {
        match self {
            Self::SelfHost { .. } => "This machine".to_string(),
            Self::ByCode { code, .. } => code.clone(),
            Self::Remote { server_url } => host_of(server_url),
        }
    }
}

/// Host (and port) of a gateway URL, or the whole string when it does not
/// parse as one — a label should degrade to something recognisable rather than
/// to nothing.
fn host_of(url: &str) -> String {
    let rest = url
        .strip_prefix("wss://")
        .or_else(|| url.strip_prefix("ws://"))
        .or_else(|| url.strip_prefix("https://"))
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let host = rest.split(['/', '?']).next().unwrap_or(rest);
    if host.is_empty() {
        url.to_string()
    } else {
        host.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionParams {
    pub mode: SessionMode,
    pub username: String,
    /// Crypto identity used to sign the Identify handshake.
    pub identity: crate::identity::Identity,
}

/// Who carries the bytes of this connection.
///
/// Worth showing rather than inferring: a relayed connection means the relay
/// operator can read everything on it, and a direct one means the host learned
/// our address. Neither is wrong, but which one happened is not something a
/// person should have to guess. See `docs/NETWORKING.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Our own machine — self-host, over loopback.
    Loopback,
    /// Straight to the host over QUIC, encrypted, with the host authenticated
    /// by its public key. The only one of these that is both direct *and*
    /// unreadable to the hops in between.
    Private,
    /// Straight to the host, in the clear: a typed `ws://` URL, or a plaintext
    /// address it published. Nobody is relaying it, and everybody on the path
    /// can read it.
    Direct,
    /// Through a rendezvous relay, which sees every frame.
    Relayed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// No gateway at all, and not waiting for one. Home is reachable in this
    /// state on purpose: direct messages are Nostr events keyed to your
    /// identity, so they owe nothing to a server, and a client that demanded
    /// one before showing them would be lying about where they live.
    Offline,
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

/// How well a participant's connection is holding up, as the SFU sees it.
///
/// Mirrors LiveKit's `ConnectionQuality` rather than re-exporting it: the UI
/// should not have to name a livekit type to draw a dot, and the SDK's enum is
/// `#[non_exhaustive]`, so pinning our own keeps the match in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionHealth {
    Excellent,
    Good,
    /// Losing packets — audible as dropouts.
    Poor,
    /// The SFU has stopped hearing from them entirely.
    Lost,
}

impl ConnectionHealth {
    /// Colour and tooltip for the roster dot, or `None` when there is nothing
    /// worth saying. Excellent and Good draw nothing on purpose: a status light
    /// on every name in a healthy call is noise, and teaches people to ignore
    /// the one that means something.
    ///
    /// `is_self` picks the wording, and it matters. The reading comes from the
    /// SFU's view of one participant, so on someone else's row it is about
    /// them, and on your own row it is about you — being told "their audio may
    /// drop out" next to your own name is both confusing and the opposite of
    /// actionable, since you are the only one who can do anything about it.
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

/// One participant's live transport numbers, as libwebrtc measures them.
///
/// Two variants rather than one struct of `Option`s: a remote row is about
/// what we *receive* from that person and our own row is about what we *send*,
/// and the two directions share almost no fields. Collapsing them would mean
/// half the columns being permanently blank on every row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackStats {
    Inbound {
        /// Share of this stream's packets that never arrived.
        loss_pct: f32,
        jitter_ms: f32,
        /// Delay NetEq is aiming to hold, averaged per emitted sample. This is
        /// the number the panel exists for: it is what the decoder adds
        /// *before* our own drift buffer, so the two together are the real
        /// playback latency.
        buffer_ms: f32,
        /// How many times loss had to be papered over. Rising here is loss
        /// that became audible, as opposed to loss the redundancy absorbed.
        concealment_events: u64,
    },
    Outbound {
        /// What is actually leaving, from the byte counter between two readings
        /// — not the encoder's `target_bitrate`, which is only what it aims
        /// for. The row exists to say whether the bitrate setting had the
        /// effect it claims, and an aim cannot answer that.
        bitrate_kbps: Option<u32>,
        /// Packets a second. Opus at the default 20ms frame sits around 50
        /// while the transmit gate is open and near zero while DTX holds
        /// silence back, so this is the sender-side "audio is leaving this
        /// machine".
        ///
        /// `None` on the first reading, like the bitrate — both are deltas and
        /// both arrive on the same tick. A zero would be indistinguishable from
        /// sending nothing, which is the one thing this row is for.
        packets_per_sec: Option<u32>,
        /// What the encoder says it is aiming for. Kept alongside the measured
        /// rate rather than replaced by it, because the two answer different
        /// questions and only this one is answerable while you are silent: a
        /// measured rate is 0 with the transmit gate closed, and so is a
        /// publication that broke. Shown in the tooltip, not as a column — the
        /// row is narrow and this is the secondary reading.
        ///
        /// Not the same as `AppState::voice_bitrate_kbps`, which is what we
        /// *asked* for. This is what the encoder adopted, and the gap between
        /// them is the interesting part.
        target_kbps: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceSession {
    pub phase: VoicePhase,
    pub channel_id: Option<Id>,
    pub muted: bool,
    pub deafened: bool,
    /// What `muted` was before deafening, so undeafening gives back what the
    /// user actually chose. Deafening forces mute on, and always unmuting on
    /// the way out would silently undo a mute they set themselves.
    ///
    /// Only meaningful while `deafened` is true, and rewritten on every
    /// transition into it — so it is never cleared, and a value left over from
    /// a session that ended is read by nothing.
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
    /// Flip deafen and report the `(muted, deafened)` pair to announce.
    ///
    /// Deafening implies muting — talking to people you can't hear is the
    /// antisocial half of the state — but muting says nothing about deafen, so
    /// only this direction is coupled. Lives here rather than in the click
    /// handler so the restore rule is the one piece of deafen a test can reach
    /// without audio hardware.
    ///
    /// `muted`/`deafened` themselves are not written here — the `VoiceCmd`s the
    /// caller sends own that, so the flags and the audio can't drift apart.
    pub fn toggle_deafen(&mut self) -> (bool, bool) {
        if self.deafened {
            (self.muted_before_deafen, false)
        } else {
            self.muted_before_deafen = self.muted;
            (true, true)
        }
    }
}

/// The guild-management dialogs, all app-modal and all rendered at the
/// workspace root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuildDialog {
    Settings(Id),
    Integrations(Id),
    Roles(Id),
}
/// Which pane home fills its main area with.
///
/// The two explore panes are separate because they answer different questions
/// against different sources: `Communities` is this host's guild catalog
/// (`FetchCatalog`), `Servers` is the rendezvous directory of *other hosts*
/// (`GET /discover`). Conflating them was the thing the home redesign set out
/// to fix — a community you can only reach by first arriving at its server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HomeView {
    #[default]
    Dms,
    Communities,
    Servers,
    People,
}

/// What the composer needs to show a "replying to X" banner.
///
/// Deliberately not `protocol::ReplyRef`: that one is the server's *answer*,
/// built from its own row and carrying an excerpt it vouches for. This is the
/// client's *intent*, and only `message_id` survives the round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyDraft {
    pub message_id: Id,
    /// Which channel the reply was started in — see the composer for why.
    pub channel_id: Id,
    pub author_username: String,
    pub excerpt: String,
}

/// One camera `enumerateDevices` offered us.
///
/// The label is carried alongside the id, and persisted with it, because
/// deviceIds are origin-salted and can rotate between sessions while labels
/// usually do not — so the label is the fallback matcher for a remembered
/// choice. Same id-then-label rule the audio output follower already uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraDevice {
    pub id: String,
    pub label: String,
}

/// A one-to-one conversation, from the viewpoint of one participant: `other`
/// is the person on the far side.
///
/// **Client-side only.** It used to be a wire type, back when a DM was a
/// channel on the host and the server told you which ones you had. Direct
/// messages are Nostr gift wraps now — the server has never heard of them — so
/// this describes what the sidebar draws, nothing more. The `channel_id` is
/// derived from the peer's pubkey by `nostr::service::conversation_id`, which
/// is what lets the DM views keep working unchanged.
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
    /// Bumped every time the voice service builds a new `ActiveVoice`.
    ///
    /// Deliberately monotonic and outside `VoiceSession`, which is reset
    /// field-wise on leave. Anything that has to be re-issued to a rebuilt
    /// session keys on this: a device change tears the session down and
    /// reconnects, and a guard comparing only *what* it last sent would see no
    /// change and stay quiet, leaving the new session missing whatever the old
    /// one had been told.
    pub voice_session_epoch: u64,
    pub selected_guild: Option<Id>,
    pub selected_channel: Option<Id>,
    /// The user's direct-message conversations.
    pub dms: Vec<DmInfo>,
    /// Unread received-message counts per DM channel. Incremented when a DM
    /// message arrives for a conversation you aren't currently viewing, and
    /// cleared when you open it. Drives the DM-home unread badge.
    pub dm_unread: HashMap<Id, u32>,
    /// Nostr event id behind each DM message, keyed by the synthetic `Id` the
    /// UI uses. Needed to answer a message: a reply names the *event*, and the
    /// Uuid is derived from it one way only.
    pub nostr_event_ids: HashMap<Id, String>,
    /// The Nostr contact list (NIP-02), as the relays hold it. Public, unlike
    /// the messages: adding someone here is visible to anyone.
    pub contacts: crate::nostr::nip02::ContactList,
    /// Relays currently connected, for the DM status line. A set rather than a
    /// count because which relay is up is the useful thing when one is not.
    pub nostr_relays_up: std::collections::HashSet<String>,
    /// When true the channels column shows DM conversations instead of the
    /// selected guild's channels (the "DM home" view).
    pub dm_mode: bool,
    /// Which of home's panes fills the main area. Only meaningful while
    /// `dm_mode` is on — a guild is always its own channel view.
    pub home_view: HomeView,
    /// Whether home's servers pane should arrive with the hosting form already
    /// unfolded. Set by the buttons that promise hosting specifically, so the
    /// label is true: landing on the pane with the form still shut leaves the
    /// person who pressed "Host my own" to go looking for it.
    pub home_open_host: bool,
    /// What to call the server this session is attached to, in home's column.
    ///
    /// Derived from `SessionParams` at mount rather than from anything the
    /// host tells us: a gateway has no name of its own on this wire, and the
    /// thing a person recognises is the code or address they arrived by.
    pub server_label: Option<String>,
    /// Public directory of guilds on the host we've fetched so far (paginated,
    /// browse-and-join). May be a prefix of the whole directory — see
    /// `catalog_total`.
    pub catalog: Vec<GuildSummary>,
    /// Total public guilds on the host (from the last `GuildCatalog` page), so
    /// the browse UI knows whether more pages remain.
    pub catalog_total: u32,
    /// Known user profiles (avatar/bio) by pubkey. Looked up when rendering a
    /// user anywhere (message author, member row, profile card).
    pub profiles: HashMap<String, Profile>,
    /// Pubkey of the user whose profile card is open, if any (UI state).
    pub profile_card: Option<String>,
    /// Source of the chat image being viewed full-size, if any. Lives here (and
    /// renders at the workspace root) for the same reason `profile_card` does —
    /// a lightbox mounted inside the message row would be clipped by the chat
    /// panel's stacking context.
    pub image_viewer: Option<String>,
    /// Which guild-management dialog is open, if any.
    ///
    /// Lives here rather than in `GuildsSidebar` so the dialog can be rendered
    /// at the workspace root. Rendered inside the panel it was opened from, an
    /// app-modal dialog sits inside that panel's stacking context and paints
    /// underneath any panel stacked above it — which is exactly what happened
    /// once panels could overlap.
    pub guild_dialog: Option<GuildDialog>,
    /// Who is currently typing, per channel: pubkey -> (username, last seen).
    /// Entries are swept after a few seconds (see WorkspaceView).
    pub typing: HashMap<Id, HashMap<String, (String, std::time::Instant)>>,
    /// Bumped whenever an inbound DM / mention should chime. A sound component
    /// watches this and plays a notification.
    pub notify_tick: u64,
    /// (livekit_url, token) for the webview JS screen-share room — set while in
    /// a voice channel. The screen bridge connects when this is Some.
    pub screen_token: Option<(String, String)>,
    /// (livekit_url, token) for the *native* half of the same room, used to
    /// subscribe to screen-share audio so it plays through the chosen output
    /// device instead of the webview's. None against servers that predate it,
    /// in which case stream audio falls back to webview playback.
    pub screen_audio_token: Option<(String, String)>,
    /// Whether the native screen-audio room is *actually joined* — not merely
    /// whether a token for it exists.
    ///
    /// The webview stands down from playing stream audio on this, so keying it
    /// on the token instead would mean a failed or dropped join leaves nobody
    /// playing at all: silence, with a volume slider that still looks live. On
    /// this, the same failure degrades to webview playback on the system
    /// default device — worse than the native path, far better than nothing.
    pub screen_audio_joined: bool,
    /// (livekit_url, token) the *native* capture path publishes screen video
    /// under, on platforms where the webview cannot capture at all (macOS — see
    /// the `sysvideo` module). None against servers that predate it.
    pub screen_video_token: Option<(String, String)>,
    /// Which surface the native path is sharing (or is about to).
    ///
    /// Held here rather than passed straight to the voice service because the
    /// effect that re-publishes after a voice-session restart has to know what
    /// was being shared — otherwise changing your microphone mid-share would
    /// resume it pointed at the wrong screen.
    pub screen_share_target: Option<crate::sysvideo::Target>,
    /// The surfaces offered by the picker, and whether it is open.
    ///
    /// Enumerating blocks on an OS query that can sit behind the Screen
    /// Recording prompt, so it happens off the UI thread and lands here.
    /// `Err` is the reason to show instead of a list — most often that
    /// permission having been refused.
    pub screen_picker: Option<Result<Vec<crate::sysvideo::Source>, String>>,
    /// Whether we're currently sharing our screen (UI state).
    pub screen_sharing: bool,
    /// Whether *our* current share is the one whose sound `sysaudio` captures.
    /// Decided after the picker closes (it depends on the surface the user
    /// chose), so it cannot be recomputed later from the platform alone — and it
    /// has to survive a voice reconnect, which drops the publication and needs
    /// to know whether to re-make it.
    pub screen_native_audio: bool,
    /// Pubkeys currently screen-sharing, per channel (from the server).
    pub screen_shares: HashMap<Id, Vec<String>>,
    /// Pubkey whose screen we're viewing in the big viewer dialog, if any.
    pub screen_viewing: Option<String>,
    /// The message the composer is currently replying to, if any.
    ///
    /// Lives in `AppState` rather than the composer's own signal because the
    /// reply is *started* from a message row and *shown* above the input —
    /// two different components. Cleared on send, on cancel, and on switching
    /// channel, so a reply can never be aimed at a message in another channel.
    /// Only the id is authoritative; the name and excerpt are for the banner,
    /// and the server rebuilds the real quote from its own row.
    pub replying_to: Option<ReplyDraft>,
    /// Whether this build can capture a screen *somehow* — either the webview
    /// exposes `getDisplayMedia`, or `sysvideo` has a native backend.
    ///
    /// Two ways in, because the two platforms answer differently: on Windows the
    /// webview is the capture path and the probe in `ScreenShareBridge` decides;
    /// on macOS the webview has no `navigator.mediaDevices` at all and the
    /// native path decides. Anything gating the share button reads this rather
    /// than asking about a webview API.
    pub screen_capture_available: bool,

    /// Whether we are actually publishing a camera.
    ///
    /// Set when the JS reports `camera-started`, never from the click — the
    /// capture can still be refused, and a button that lights before a track
    /// exists is the bug the share path already learnt (`share-started`).
    pub camera_on: bool,
    /// A start is in flight: the click landed, `getUserMedia` has not answered.
    pub camera_starting: bool,
    /// Cameras this webview can open. `label` is empty until a camera grant
    /// exists for this origin; the ids work regardless, so an unlabelled list is
    /// offered rather than withheld.
    pub available_cameras: Vec<CameraDevice>,
    /// Whose cameras we have chosen to watch, by pubkey.
    ///
    /// Opt-in per person, deliberately — the same shape as `screen_viewing`,
    /// which nobody is shown until they ask. An earlier version opened a grid of
    /// *everyone's* camera as soon as one appeared, which both took the screen
    /// over uninvited and pulled down video nobody had asked for. A set rather
    /// than `screen_viewing`'s single `Option` because several small camera tiles
    /// coexist happily, where two full-size shared screens do not.
    pub cameras_watching: HashSet<String>,
    /// Whether this webview exposes `getUserMedia` at all. Unlike
    /// `screen_capture_available` there is no native fallback to consult — the
    /// camera is the webview on every platform.
    pub camera_capture_available: bool,

    /// Available input device names (populated by voice service on request).
    pub available_input_devices: Vec<String>,
    /// Available output device names (populated by voice service on request).
    pub available_output_devices: Vec<String>,
    /// Selected input device name (None = use system default).
    pub selected_input_device: Option<String>,
    /// Selected output device name (None = use system default).
    pub selected_output_device: Option<String>,
    /// Microphone gate threshold (1..=1000, peak ×1000 — the same scale as
    /// `mic_level`). Frames below it are treated as inactive mic: they are not
    /// transmitted and the speaking indicator stays off. Driven by the audio
    /// settings slider, persisted via `ClientSettings`. Lower = more sensitive.
    pub mic_sensitivity: u32,
    /// Microphone input gain as a percentage (0..=200, 100 = unity), applied
    /// before the meter and the gate. Persisted via `ClientSettings`.
    pub mic_volume: u16,
    /// libwebrtc automatic gain control on the capture. Fights `mic_volume`, so
    /// the user gets a switch. Persisted via `ClientSettings`.
    pub auto_gain_control: bool,
    /// Live microphone peak level (0..=1000, fixed-point ×1000), sampled every
    /// 150ms from the same frames the transmit gate judges. Drives the VU bar
    /// in the audio settings popover so the user can see mic input against
    /// where the threshold sits.
    pub mic_level: u32,
    /// The same hop before DeepFilterNet, ×1000. Equal to `mic_level` when
    /// noise cancellation is off; the gap between them is what the model is
    /// removing, which is the only place a user can see it happening.
    pub mic_level_pre: u32,
    /// Whether DeepFilterNet noise suppression runs on captured microphone
    /// audio before it is published. Persisted via `ClientSettings`.
    pub noise_cancellation: bool,
    /// Ceiling on DeepFilterNet's attenuation, in dB. See
    /// `ClientSettings::denoise_atten_lim_db`.
    pub denoise_atten_lim_db: u32,
    /// Whether the microphone should be captured with the OS's own input
    /// processing bypassed. Read when the capture is opened — the mode is fixed
    /// for the life of a stream — so changing it restarts the voice session.
    /// Persisted via `ClientSettings`.
    pub bypass_system_audio_processing: bool,
    /// Why that bypass isn't in effect, when it was asked for and didn't
    /// happen. `Some` means the microphone is open on the ordinary path
    /// regardless of the switch, which the panel says out loud rather than
    /// leaving a lit toggle to imply otherwise.
    pub mic_bypass_error: Option<String>,
    /// Opus bitrate for our microphone track, in kbit/s (24 or 48). Applied
    /// when the track is published, so a change takes effect on the next voice
    /// connect. Persisted via `ClientSettings`.
    pub voice_bitrate_kbps: u32,
    /// How good LiveKit thinks each participant's connection is, keyed by
    /// pubkey. Only populated while in a voice channel, and only for
    /// participants the SFU has reported on — absent means "no reading yet",
    /// which the roster shows as nothing rather than as a problem.
    pub voice_quality: HashMap<String, ConnectionHealth>,
    /// Transport numbers per participant, keyed by pubkey. Only populated
    /// while the stats panel is open — it is a diagnostic, and polling the
    /// peer connection every second for a panel nobody is looking at is pure
    /// cost. Empty therefore means "not measuring", never "measured zero".
    pub voice_stats: HashMap<String, TrackStats>,
    /// Per-participant playback gain, keyed by pubkey, as a percentage
    /// (100 = unity, 0..=200). Purely local: it scales *incoming* audio in our
    /// own mixer and is never sent anywhere, so it cannot affect what the
    /// speaker transmits or what anyone else hears.
    pub user_volumes: HashMap<String, u32>,
    /// Participants muted locally (independent of `user_volumes`, so unmuting
    /// restores the previous level rather than resetting it to unity).
    pub user_muted: HashSet<String>,
    /// Per-sharer screen-share audio gain, keyed by pubkey, as a percentage
    /// (100 = unity, 0..=200). Separate from `user_volumes`: the broadcast's
    /// audio track and the sharer's microphone are two different streams.
    pub stream_volumes: HashMap<String, u32>,
    /// Screen-share streams muted locally, keyed by sharer pubkey.
    pub stream_muted: HashSet<String>,
    /// Sharers whose stream actually carries an audio track. A share can
    /// legitimately be video-only (the platform may not let the app capture
    /// system audio at all), and a volume slider over silence looks like a bug —
    /// so the UI needs to know.
    ///
    /// Normally reported by the voice service, which subscribes to every
    /// screen-audio track; the webview reports it only in the fallback case
    /// where the server sent no `audio_token`.
    pub stream_has_audio: HashSet<String>,
    /// Media has arrived that we could not decrypt — almost always a key that
    /// has not reached us yet.
    ///
    /// Cleared when a new key is adopted, because that is the event most likely
    /// to fix it. A latch rather than a counter: the UI question is "is
    /// something wrong right now", and one failed frame and a thousand mean the
    /// same thing to the person who cannot hear anybody.
    pub media_undecryptable: bool,
    /// Set when a member is removed from a guild, cleared once the media key
    /// has been rolled.
    ///
    /// A flag rather than a direct call because `apply` has no gateway to send
    /// on and no async to await — it mutates state and returns. `MediaKeyBridge`
    /// watches this and does the work.
    pub pending_rekey: bool,
    /// Our own keypair, for the paths that need to do crypto with it rather
    /// than merely name us — opening a media key sealed to us, and sealing one
    /// for somebody else. Set when the session starts.
    ///
    /// `self_user` names who we are; this is what proves it.
    pub identity: Option<crate::identity::Identity>,
    /// The media key in force for each voice channel we hold one for, with the
    /// epoch it arrived under.
    ///
    /// Held here rather than passed around because four separate LiveKit
    /// connections need it and a rekey has to reach all of them — see
    /// `crate::mediakey` for how it gets here, and `crate::e2ee` for where it
    /// goes. Never persisted: a key that outlived the session would outlive the
    /// membership it was scoped to.
    pub media_keys: HashMap<Id, (u32, [u8; 32])>,
    /// Populated when running in self-host mode. None for remote connections.
    pub host_info: Option<HostInfo>,
    /// Who is carrying this connection — which of `docs/NETWORKING.md`'s tiers
    /// we ended up on. Set once the socket is up, since for a join by code that
    /// is decided by a race rather than by the session parameters.
    pub transport: Transport,
    /// Bot installs per guild, for the owner's Integrations dialog. Populated by
    /// `GuildIntegrations` (owner-only) in response to `FetchIntegrations` and
    /// after each install/uninstall.
    pub integrations: HashMap<Id, Vec<BotInstall>>,
    /// Custom emoji per guild. Arrives in `Ready`/`GuildJoined` and stays live
    /// via `GuildEmojis` pushes. The catalog only — see `emoji_images`.
    pub guild_emojis: HashMap<Id, Vec<crate::protocol::GuildEmoji>>,
    /// Emoji images by content address (`<sha256>.<ext>` -> `data:` URL).
    /// An empty value means "the server has no such blob" — cached so a broken
    /// emoji is asked about once, not on every render.
    pub emoji_images: HashMap<String, String>,
    /// Content addresses we've already asked the server for, so a catalog that
    /// mentions the same image fifty times produces one request.
    pub emoji_requested: HashSet<String>,
    /// Roles per guild. Arrives in `Ready`/`GuildJoined` and stays live via
    /// `GuildRoles` pushes.
    pub roles: HashMap<Id, Vec<Role>>,
    /// Ban lists per guild (moderators only; reply to `FetchBans`).
    pub bans: HashMap<Id, Vec<User>>,
    /// Invite codes per guild (reply to `CreateInvite`).
    pub invites: HashMap<Id, String>,
    /// Latest server error to surface as a toast (management-op rejections
    /// would otherwise be invisible). Cleared on dismiss.
    pub error_toast: Option<String>,
    /// True if we're a configured operator — owner of system guilds (the
    /// Lobby) for permission purposes. Mirrors the server so the UI surfaces
    /// management controls there. Set from `Ready`.
    pub is_operator: bool,
    /// Moderation audit log per guild (reply to `FetchAuditLog`).
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
            home_view: HomeView::Dms,
            home_open_host: false,
            server_label: None,
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
            // Keep in step with `settings::default_mic_sensitivity` — this is
            // what a session shows before saved settings are restored.
            mic_sensitivity: 50,
            mic_volume: 100,
            auto_gain_control: true,
            mic_level: 0,
            mic_level_pre: 0,
            noise_cancellation: false,
            denoise_atten_lim_db: 30,
            bypass_system_audio_processing: false,
            mic_bypass_error: None,
            // Keep in step with `settings::default_voice_bitrate_kbps`.
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

    /// True if the current user owns `guild_id` for permission purposes.
    /// Mirrors the server's `is_owner`: a normal guild → the literal owner; a
    /// system guild (empty owner, e.g. the Lobby) → whether we're an operator.
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

    /// Whether the current user holds `perm` in `guild_id` — mirrors the
    /// server's rule: owner ⇒ everything, system guild ⇒ nothing, otherwise
    /// the union over the member's assigned roles. Advisory only (drives UI
    /// affordances); the server re-checks every action.
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

    /// Resolve a `:shortcode:` to its image data URL, within one guild.
    /// `None` when the guild has no such emoji or its bytes haven't arrived
    /// yet — callers render the literal `:shortcode:` text in that case, which
    /// is also what a client without the emoji would show.
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

    /// The custom emoji of a guild (empty slice if none).
    pub fn emojis_of(&self, guild_id: Id) -> &[crate::protocol::GuildEmoji] {
        self.guild_emojis
            .get(&guild_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// The roles of a guild (empty slice if none).
    pub fn roles_of(&self, guild_id: Id) -> &[Role] {
        self.roles
            .get(&guild_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Pubkeys sharing their screen in a channel, sorted.
    ///
    /// The union of two sources, deliberately. `VoiceState::screen_sharing` is
    /// the current one and the only one that survives a reconnect, since `Ready`
    /// carries the voice roster and no screen-share snapshot. `screen_shares` is
    /// filled by `ScreenShareState`, which a server older than that flag is the
    /// only thing still sending — so taking the union is what makes this client
    /// work against both without needing to detect which it is talking to.
    ///
    /// Returns owned rather than a slice because there is no longer one stored
    /// list to borrow.
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

    /// Pubkeys with their camera on in a channel, sorted.
    ///
    /// Derived from `voice_states` rather than kept in a map of its own, because
    /// the server puts `camera_on` on the voice state — so this is already
    /// snapshot on connect and already cleared by every teardown, where
    /// `screen_shares` needed its own message and its own cleanup.
    ///
    /// Sorted so a tile grid built from it does not reshuffle under the reader
    /// when the roster's order changes for unrelated reasons.
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

    /// Usernames currently typing in a channel (sorted, for a stable label).
    pub fn typers_in(&self, channel_id: Id) -> Vec<String> {
        let mut names: Vec<String> = self
            .typing
            .get(&channel_id)
            .map(|m| m.values().map(|(name, _)| name.clone()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    /// The profile for a pubkey, if we have one.
    pub fn profile_of(&self, pubkey: &str) -> Option<&Profile> {
        self.profiles.get(pubkey)
    }

    /// Effective presence for a pubkey: `"online" | "away" | "dnd" | "offline"`.
    ///
    /// Two independent things feed this and both matter. Connection presence is
    /// the server's (`Member.online`, kept live by `MemberJoin`/`MemberLeave`);
    /// the self-set status in the profile is only a *label* the user picked. A
    /// disconnected user with `status: "online"` sitting in their profile is
    /// offline, so connection presence wins — that's the bug behind the profile
    /// card's permanently green dot, which read the label alone.
    ///
    /// Ourselves we always treat as connected (we're the ones rendering), and a
    /// pubkey we hold no member row for (a DM partner in a guild we don't
    /// share) has unknown presence — falling back to the label there beats
    /// asserting "offline" about someone we simply can't see.
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

    /// Local playback gain for a participant's voice, as a linear multiplier.
    /// Locally muted or 0% ⇒ 0.0. Never leaves this client.
    pub fn voice_gain_of(&self, pubkey: &str) -> f32 {
        if self.user_muted.contains(pubkey) {
            return 0.0;
        }
        self.user_volumes.get(pubkey).copied().unwrap_or(100) as f32 / 100.0
    }

    /// Local playback gain for a screen share's audio, as a linear multiplier.
    pub fn stream_gain_of(&self, pubkey: &str) -> f32 {
        if self.stream_muted.contains(pubkey) {
            return 0.0;
        }
        self.stream_volumes.get(pubkey).copied().unwrap_or(100) as f32 / 100.0
    }

    /// The avatar data URL for a pubkey, if set.
    pub fn avatar_of(&self, pubkey: &str) -> Option<&str> {
        self.profiles.get(pubkey).and_then(|p| p.avatar.as_deref())
    }

    /// The channel a guild should open on: its first *text* channel in the same
    /// order the sidebar renders.
    ///
    /// `channels` is stored in arrival order, but the channel list sorts by
    /// `(position, name)`, so picking the vec's first entry could land on a
    /// different channel than the one sitting at the top of the list — or, if
    /// the `kind` filter is forgotten, on a voice channel with no messages at
    /// all. Every guild-switch path routes through here so they agree.
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

    /// The DM conversation whose channel id is `channel_id`, if any.
    pub fn dm_of(&self, channel_id: Id) -> Option<&DmInfo> {
        self.dms.iter().find(|d| d.channel_id == channel_id)
    }

    /// Total unread DM messages across all conversations.
    pub fn dm_unread_total(&self) -> u32 {
        self.dm_unread.values().copied().sum()
    }

    /// The last message in a conversation, for the sidebar's preview line.
    ///
    /// Reads the same `messages` map the chat view does — the Nostr service
    /// files every decrypted wrap there on replay, not only the open one, so
    /// this is available for conversations nobody has clicked yet.
    pub fn dm_last_message(&self, channel_id: Id) -> Option<&Message> {
        self.messages.get(&channel_id).and_then(|m| m.last())
    }

    /// Conversations most-recent first, the order a message list is read in.
    ///
    /// Conversations with nothing in them sort last rather than first: an
    /// empty one is usually one you just opened by pasting a key, and it has
    /// no activity to be recent about.
    pub fn dms_by_recency(&self) -> Vec<DmInfo> {
        let mut v = self.dms.clone();
        v.sort_by(|a, b| {
            let at = self.dm_last_message(a.channel_id).map(|m| m.created_at);
            let bt = self.dm_last_message(b.channel_id).map(|m| m.created_at);
            bt.cmp(&at)
        });
        v
    }

    /// Whether the gateway currently reports this person as online.
    ///
    /// Only answerable for members of guilds we share — presence is a gateway
    /// fact, and a Nostr contact we share no guild with simply has none. False
    /// here therefore means "not known to be online", never "offline", and
    /// nothing may draw it as the latter.
    pub fn is_online(&self, pubkey: &str) -> bool {
        self.members
            .iter()
            .any(|m| m.user.pubkey == pubkey && m.online)
    }

    /// Communities in this host's public catalog we haven't joined yet.
    pub fn joinable_communities(&self) -> Vec<GuildSummary> {
        self.catalog
            .iter()
            .filter(|c| !self.guilds.iter().any(|g| g.id == c.id))
            .cloned()
            .collect()
    }

    /// Communities we chose to be in. The host's system space (empty owner) is
    /// auto-joined by `snapshot_for`, so counting raw membership would say
    /// "you're settled in" to somebody who has joined nothing.
    pub fn joined_communities(&self) -> usize {
        self.guilds
            .iter()
            .filter(|g| !g.owner_pubkey.is_empty())
            .count()
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

    /// What to show for a pubkey: their username, or a truncated key.
    ///
    /// The fallback is the honest answer rather than a placeholder — an
    /// audit-log actor or an emoji uploader who has since left the guild is not
    /// in `members` any more, and the key is still who they were.
    ///
    /// Here rather than inline at each site because the pair `user_of` +
    /// `truncate_pubkey` is the whole rule, and it is spelled out identically
    /// in several places. Anything that changes it later — nicknames, a
    /// per-guild display name — should have one place to change.
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

    /// A code is what somebody was handed and what they would say out loud, so
    /// it is the label even though an address is more precise.
    #[test]
    fn a_code_labels_itself() {
        let mode = SessionMode::ByCode {
            rendezvous_url: "ws://rz.example:7700".into(),
            code: "purple-fox-42".into(),
        };
        assert_eq!(mode.label(), "purple-fox-42");
    }

    /// The scheme and path are noise in a chip; the host is the part that
    /// identifies the machine.
    #[test]
    fn an_address_shows_its_host() {
        let mode = SessionMode::Remote {
            server_url: "wss://chat.example.com:9000/gateway".into(),
        };
        assert_eq!(mode.label(), "chat.example.com:9000");
    }

    /// Anything that is not a URL still has to produce something readable
    /// rather than an empty chip.
    #[test]
    fn an_unparseable_address_survives_whole() {
        let mode = SessionMode::Remote {
            server_url: "not a url".into(),
        };
        assert_eq!(mode.label(), "not a url");
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

    /// The bug behind the always-green profile dot: a disconnected user whose
    /// profile still carries `status: "online"` must read as offline.
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

    /// Membership of several guilds: online anywhere is online.
    #[test]
    fn presence_is_online_if_any_member_row_is() {
        let mut s = AppState::empty();
        s.members.push(member("dave", false));
        let mut second = member("dave", true);
        second.guild_id = uuid::Uuid::from_u128(1);
        s.members.push(second);
        assert_eq!(s.presence_of("dave"), "online");
    }

    /// Someone we hold no member row for (a DM partner in no shared guild) has
    /// unknown presence — fall back to their label rather than claim offline.
    #[test]
    fn presence_falls_back_to_the_label_for_unknown_users() {
        let mut s = AppState::empty();
        s.profiles
            .insert("erin".into(), profile("erin", Some("away")));
        assert_eq!(s.presence_of("erin"), "away");
        assert_eq!(s.presence_of("nobody"), "online");
    }

    /// We are always connected from our own point of view, even before any
    /// member row for us has arrived.
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
        // Muting must not clobber the stored level — unmuting restores 150%.
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

        // Already muted → deafen keeps it → undeafen leaves it muted rather
        // than undoing a choice the user made before deafening.
        let mut v = VoiceSession {
            muted: true,
            ..VoiceSession::default()
        };
        assert_eq!(v.toggle_deafen(), (true, true));
        v.deafened = true;
        assert_eq!(v.toggle_deafen(), (true, false));
    }

    /// The fallback is the point of the helper: seven call sites spelled this
    /// out identically, and the honest answer for someone no longer in
    /// `members` is their key, not a placeholder.
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

    /// `user_of` answers for the logged-in user before it looks at `members`,
    /// and `display_name` inherits that — otherwise your own name would read as
    /// a truncated key in any guild whose roster has not arrived yet.
    #[test]
    fn display_name_resolves_the_logged_in_user_without_a_roster() {
        let mut s = AppState::empty();
        let me = "c".repeat(64);
        s.self_user = Some(user(&me));

        assert!(s.members.is_empty());
        assert_eq!(s.display_name(&me), format!("u-{me}"));
    }
}
