//! App-wide state shared via Dioxus context.

use std::collections::{BTreeMap, HashMap, HashSet};

use dioxus::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::host::HostInfo;
use crate::protocol::{
    BotInstall, Channel, ClientMessage, DmInfo, Guild, GuildSummary, Id, Member, Message,
    Permission, Profile, Role, User, VoiceState,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionParams {
    pub mode: SessionMode,
    pub username: String,
    /// Crypto identity used to sign the Identify handshake.
    pub identity: crate::identity::Identity,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceSession {
    pub phase: VoicePhase,
    pub channel_id: Option<Id>,
    pub muted: bool,
    pub deafened: bool,
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
            speaking: false,
            error: None,
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
    /// When true the channels column shows DM conversations instead of the
    /// selected guild's channels (the "DM home" view).
    pub dm_mode: bool,
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
    /// Whether the embedded webview supports navigator.mediaDevices.getDisplayMedia
    /// (used for screen sharing). Populated at runtime by the ScreenShareBridge.
    pub screen_capture_available: bool,

    // Audio device preferences surfaced to the UI.
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
    /// Live microphone peak level (0..=1000, fixed-point ×1000), sampled every
    /// 150ms from the same frames the transmit gate judges. Drives the VU bar
    /// in the audio settings popover so the user can see mic input against
    /// where the threshold sits.
    pub mic_level: u32,
    /// Whether DeepFilterNet noise suppression runs on captured microphone
    /// audio before it is published. Persisted via `ClientSettings`.
    pub noise_cancellation: bool,
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
    /// Populated when running in self-host mode. None for remote connections.
    pub host_info: Option<HostInfo>,
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
            dm_mode: false,
            catalog: Vec::new(),
            catalog_total: 0,
            profiles: HashMap::new(),
            profile_card: None,
            guild_dialog: None,
            typing: HashMap::new(),
            notify_tick: 0,
            screen_token: None,
            screen_audio_token: None,
            screen_audio_joined: false,
            screen_sharing: false,
            screen_native_audio: false,
            screen_shares: HashMap::new(),
            screen_viewing: None,
            screen_capture_available: false,
            // Audio device prefs: empty by default (discover on demand).
            available_input_devices: Vec::new(),
            available_output_devices: Vec::new(),
            selected_input_device: None,
            selected_output_device: None,
            mic_sensitivity: 25,
            mic_level: 0,
            noise_cancellation: false,
            user_volumes: HashMap::new(),
            user_muted: HashSet::new(),
            stream_volumes: HashMap::new(),
            stream_muted: HashSet::new(),
            stream_has_audio: HashSet::new(),
            host_info: None,
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
        self.emoji_images.get(image).map(String::as_str).filter(|u| !u.is_empty())
    }

    /// The custom emoji of a guild (empty slice if none).
    pub fn emojis_of(&self, guild_id: Id) -> &[crate::protocol::GuildEmoji] {
        self.guild_emojis.get(&guild_id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// The roles of a guild (empty slice if none).
    pub fn roles_of(&self, guild_id: Id) -> &[Role] {
        self.roles.get(&guild_id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Pubkeys sharing their screen in a channel.
    pub fn screen_sharers_in(&self, channel_id: Id) -> &[String] {
        self.screen_shares.get(&channel_id).map(|v| v.as_slice()).unwrap_or(&[])
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
        self.profiles
            .get(pubkey)
            .and_then(|p| p.avatar.as_deref())
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
            .min_by(|a, b| a.position.cmp(&b.position).then_with(|| a.name.cmp(&b.name)))
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

        // No label set at all ⇒ plain online.
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

        // Stream volume is tracked separately from the same user's voice.
        s.stream_volumes.insert("x".into(), 50);
        assert_eq!(s.stream_gain_of("x"), 0.5);
        assert_eq!(s.voice_gain_of("x"), 1.5);
    }
}
