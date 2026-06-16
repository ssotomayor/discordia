//! App-wide state shared via Dioxus context.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use tokio::sync::mpsc::UnboundedSender;

use crate::host::HostInfo;
use crate::protocol::{Channel, ClientMessage, Guild, Id, Member, Message, User, VoiceState};

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
    /// Populated when running in self-host mode. None for remote connections.
    pub host_info: Option<HostInfo>,
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
            host_info: None,
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

pub fn use_gateway() -> GatewayTx {
    use_context::<GatewayTx>()
}
