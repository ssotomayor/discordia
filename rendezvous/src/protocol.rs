//! Wire protocol between rendezvous server and hosts.
//!
//! - Host opens a long-lived WS to `/control`, sends `Register`, receives
//!   `Registered { shortcode, livekit_url }`.
//! - Rendezvous notifies host of incoming friend connections by sending
//!   `NewFriend { session_id }`. Host responds by opening an outbound WS to
//!   `/proxy/{session_id}` which the rendezvous pairs with the friend's
//!   socket.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum HostToRendezvous {
    Register {
        /// Optional friendly name shown in the public listing.
        name: Option<String>,
        /// Hint for desired shortcode; rendezvous may reject and assign one.
        preferred: Option<String>,
        /// If true, this host appears in `GET /discover` and is browseable.
        #[serde(default)]
        publish_public: bool,
        /// Optional one-line description shown next to the name on the
        /// browse tab.
        #[serde(default)]
        description: Option<String>,
    },
}

/// Single entry returned by `GET /discover`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DiscoverEntry {
    pub shortcode: String,
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum RendezvousToHost {
    Registered {
        shortcode: String,
        /// LiveKit URL the host should hand to clients in JoinVoice responses.
        /// Provided when the rendezvous operator runs a shared LiveKit alongside.
        livekit_url: Option<String>,
    },
    NewFriend {
        session_id: String,
    },
    Error {
        message: String,
    },
}
