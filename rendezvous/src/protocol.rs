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
        /// Claimed unique name (URL-safe: letters, digits, `-`, `_`, `.`). It
        /// doubles as the join code (`/join/{name}`) and is compared
        /// case-insensitively for uniqueness. When set it must be accompanied
        /// by `pubkey` + `signature` proving ownership, and the reservation is
        /// persisted. When `None` the rendezvous assigns a random shortcode
        /// (ephemeral, anonymous — the previous default).
        name: Option<String>,
        /// x-only Nostr pubkey (64-char hex) that owns the claimed name.
        /// Required iff `name` is set.
        #[serde(default)]
        pubkey: Option<String>,
        /// Schnorr signature over `SHA256(nonce || pubkey || name)` where
        /// `nonce` is the one from the preceding `Challenge`. Required iff
        /// `name` is set.
        #[serde(default)]
        signature: Option<String>,
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
    /// Seconds since the rendezvous last heard from this host's control socket.
    ///
    /// A host is dropped from the listing once it misses enough heartbeats, but
    /// there is necessarily a window between "went away" and "we noticed". This
    /// lets a browser show that a host has gone quiet instead of presenting a
    /// stale entry as if it were reachable.
    #[serde(default)]
    pub idle_secs: u64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum RendezvousToHost {
    /// First frame the rendezvous sends on `/control`: a nonce the host signs
    /// (together with its pubkey + claimed name) to prove name ownership.
    Challenge {
        nonce: String,
    },
    Registered {
        shortcode: String,
        /// Bearer credential for `POST /voice-token`, letting this host ask us
        /// to mint LiveKit tokens for the shared SFU. Deliberately NOT the
        /// signing secret: on a public relay any host holding that could mint
        /// tokens into any other host's rooms. This grant is per-session,
        /// scoped to rooms we namespace for this host, and dies with the
        /// control connection.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice_token_grant: Option<String>,
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
