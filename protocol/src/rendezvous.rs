//! Wire protocol between the rendezvous relay and the hosts that register with
//! it.
//!
//! - Host opens a long-lived WS to `/control`, sends `Register`, receives
//!   `Registered { shortcode, livekit_url }`.
//! - Rendezvous notifies host of incoming friend connections by sending
//!   `NewFriend { session_id }`. Host responds by opening an outbound WS to
//!   `/proxy/{session_id}` which the rendezvous pairs with the friend's
//!   socket.
//!
//! This lives in the shared protocol crate rather than in `dioxusfun-rendezvous`
//! because the client speaks it too (the self-host path is the "host" side) and
//! nothing depends on the relay crate. Kept apart from the gateway protocol in
//! `lib.rs`: different endpoint, different peer, versioned on its own.

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
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct DiscoverEntry {
    pub shortcode: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Seconds since the rendezvous last heard from this host's control socket.
    ///
    /// A host is dropped from the listing once it misses enough heartbeats, but
    /// there is necessarily a window between "went away" and "we noticed". This
    /// lets a browser show that a host has gone quiet instead of presenting a
    /// stale entry as if it were reachable. Older rendezvous builds don't send
    /// it at all, hence the default — treat those as fresh rather than showing
    /// every host as stale.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The relay and the client were separate definitions of this frame until
    /// they were merged here; pin the shape so the merge can't have moved it,
    /// and so a future field can't silently break an already-deployed relay.
    #[test]
    fn register_frame_shape_is_stable() {
        let json = serde_json::to_value(HostToRendezvous::Register {
            name: Some("casa".into()),
            pubkey: Some("ab".repeat(32)),
            signature: Some("cd".repeat(64)),
            publish_public: true,
            description: None,
        })
        .unwrap();
        assert_eq!(json["op"], "register");
        let d = &json["d"];
        assert_eq!(d["name"], "casa");
        assert_eq!(d["publish_public"], true);
        assert!(d["description"].is_null());
    }

    /// A relay that omits an optional field — as `voice_token_grant` already
    /// does when empty — must still register a host rather than leaving it
    /// stuck waiting for a frame it can't parse. Serde gives `Option` fields
    /// that tolerance for free; this pins it, so turning one into a bare
    /// `String` later fails here instead of in the field.
    #[test]
    fn registered_tolerates_missing_optional_fields() {
        let msg: RendezvousToHost =
            serde_json::from_str(r#"{"op":"registered","d":{"shortcode":"brave-otter-07"}}"#)
                .unwrap();
        match msg {
            RendezvousToHost::Registered {
                shortcode,
                voice_token_grant,
                livekit_url,
            } => {
                assert_eq!(shortcode, "brave-otter-07");
                assert!(voice_token_grant.is_none());
                assert!(livekit_url.is_none());
            }
            other => panic!("parsed as {other:?}"),
        }
    }
}
