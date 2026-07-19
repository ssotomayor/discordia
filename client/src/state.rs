//! App-wide state shared via Dioxus context.

use std::collections::{BTreeMap, HashMap};

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
    pub error: Option<String>,
}

impl Default for VoiceSession {
    fn default() -> Self {
        Self {
            phase: VoicePhase::Idle,
            channel_id: None,
            muted: false,
            deafened: false,
            error: None,
        }
    }
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
    /// Who is currently typing, per channel: pubkey -> (username, last seen).
    /// Entries are swept after a few seconds (see WorkspaceView).
    pub typing: HashMap<Id, HashMap<String, (String, std::time::Instant)>>,
    /// Bumped whenever an inbound DM / mention should chime. A sound component
    /// watches this and plays a notification.
    pub notify_tick: u64,
    /// (livekit_url, token) for the webview JS screen-share room — set while in
    /// a voice channel. The screen bridge connects when this is Some.
    pub screen_token: Option<(String, String)>,
    /// Whether we're currently sharing our screen (UI state).
    pub screen_sharing: bool,
    /// Pubkeys currently screen-sharing, per channel (from the server).
    pub screen_shares: HashMap<Id, Vec<String>>,
    /// Pubkey whose screen we're viewing in the big viewer dialog, if any.
    pub screen_viewing: Option<String>,
    /// Populated when running in self-host mode. None for remote connections.
    pub host_info: Option<HostInfo>,
    /// Bot installs per guild, for the owner's Integrations dialog. Populated by
    /// `GuildIntegrations` (owner-only) in response to `FetchIntegrations` and
    /// after each install/uninstall.
    pub integrations: HashMap<Id, Vec<BotInstall>>,
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
            selected_guild: None,
            selected_channel: None,
            dms: Vec::new(),
            dm_unread: HashMap::new(),
            dm_mode: false,
            catalog: Vec::new(),
            catalog_total: 0,
            profiles: HashMap::new(),
            profile_card: None,
            typing: HashMap::new(),
            notify_tick: 0,
            screen_token: None,
            screen_sharing: false,
            screen_shares: HashMap::new(),
            screen_viewing: None,
            host_info: None,
            integrations: HashMap::new(),
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

    /// The avatar data URL for a pubkey, if set.
    pub fn avatar_of(&self, pubkey: &str) -> Option<&str> {
        self.profiles
            .get(pubkey)
            .and_then(|p| p.avatar.as_deref())
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
