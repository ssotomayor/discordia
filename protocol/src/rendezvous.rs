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
    /// Give up a claimed name, proving ownership the same way claiming it did.
    ///
    /// Sent *instead of* `Register` on a control connection: this is an
    /// administrative act, not a session, so the connection carries one frame
    /// and closes. Without it a reservation was permanent — `claim_name`
    /// persists and nothing ever removed one, so a name claimed by mistake, or
    /// by a key its owner has since rotated away from, was stuck forever.
    ReleaseName {
        /// The claimed name, as sent when it was claimed (compared
        /// case-insensitively, like every other use of a name here).
        name: String,
        /// x-only Nostr pubkey that owns it.
        pubkey: String,
        /// Schnorr signature over `SHA256(nonce || pubkey || name)`, against
        /// the nonce from the preceding `Challenge` — the same construction
        /// the claim used, so one verifier serves both.
        signature: String,
    },
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
        /// A gateway URL this host believes the internet can dial directly —
        /// the result of a port mapping (UPnP-IGD / NAT-PMP), or a manual
        /// forward. `None` means "relay me": either the host obtained no
        /// address, or it chose not to publish one.
        ///
        /// Advertising it is what lets a friend skip the relay, so it is also
        /// what publishes the host's home IP to anyone who can read the
        /// listing. That trade is the host's to make — see `docs/NETWORKING.md`.
        #[serde(default)]
        endpoint: Option<String>,
        /// The host's QUIC transport key, which a friend dials *instead of* an
        /// address: the address only says where to send the packets.
        ///
        /// A separate key from `pubkey` because it is on a different curve —
        /// ed25519 for the transport, secp256k1 for the account — so this says
        /// nothing on its own about whose host it is. `transport_signature` is
        /// what ties them together.
        #[serde(default)]
        transport_key: Option<String>,
        /// Schnorr signature over `SHA256(nonce || pubkey || transport_key)`,
        /// against the same `Challenge` nonce the name claim uses.
        ///
        /// Without it a host could advertise somebody else's transport key, or
        /// a name's owner could be impersonated by anyone able to register —
        /// the point of the whole transport being authenticated is lost if the
        /// key it authenticates against is unattested.
        #[serde(default)]
        transport_signature: Option<String>,
        /// UDP addresses the transport is listening on, most useful first.
        ///
        /// Hints, not identity: a joiner tries them and the key decides whether
        /// whatever answers is the right host.
        #[serde(default)]
        transport_addrs: Vec<String>,
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
    /// The host's directly-dialable gateway URL, when it advertised one at
    /// registration. A joiner races this against the relay and keeps whichever
    /// answers, so `None` simply means "relay only" rather than an error.
    ///
    /// Older rendezvous builds don't send it; older clients ignore it. Both
    /// directions degrade to the relayed path, which is what they already do.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// The host's QUIC transport key and where to try it. Present only when the
    /// host advertised one *and* proved it belongs to the key that owns the
    /// registration — an unverified pair is dropped at registration rather than
    /// passed on for a joiner to worry about.
    #[serde(default)]
    pub transport_key: Option<String>,
    #[serde(default)]
    pub transport_addrs: Vec<String>,
    /// The iroh relay to be introduced through, when this rendezvous runs one.
    /// A joiner needs it for the same reason the host did.
    #[serde(default)]
    pub relay_url: Option<String>,
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
        /// The iroh relay this rendezvous runs, if it runs one.
        ///
        /// Hole punching needs somebody to introduce two peers, and this says
        /// who. Absent, a host does no coordination at all rather than falling
        /// back to a public relay nobody chose — the whole point of it arriving
        /// here is that the third party is the one the user already picked.
        #[serde(default)]
        relay_url: Option<String>,
    },
    /// The name is no longer reserved.
    Released {
        name: String,
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
            endpoint: None,
            transport_key: None,
            transport_signature: None,
            transport_addrs: Vec::new(),
        })
        .unwrap();
        assert_eq!(json["op"], "register");
        let d = &json["d"];
        assert_eq!(d["name"], "casa");
        assert_eq!(d["publish_public"], true);
        assert!(d["description"].is_null());
    }

    /// A host that obtained a public address puts it on the wire, and a
    /// rendezvous that predates the field still parses the frame. Both halves
    /// matter: the endpoint is what turns a relayed join into a direct one, and
    /// a host that advertises one must not become unregisterable against an
    /// older relay.
    #[test]
    fn endpoint_round_trips_and_is_optional() {
        let json = serde_json::to_string(&HostToRendezvous::Register {
            name: None,
            pubkey: None,
            signature: None,
            publish_public: false,
            description: None,
            endpoint: Some("ws://203.0.113.5:9000".into()),
            transport_key: None,
            transport_signature: None,
            transport_addrs: Vec::new(),
        })
        .unwrap();
        let back: HostToRendezvous = serde_json::from_str(&json).unwrap();
        let HostToRendezvous::Register { endpoint, .. } = back else {
            panic!("a register frame must deserialize as one");
        };
        assert_eq!(endpoint.as_deref(), Some("ws://203.0.113.5:9000"));

        // A frame from a client that has never heard of the field.
        let old: HostToRendezvous =
            serde_json::from_str(r#"{"op":"register","d":{"name":null}}"#).unwrap();
        let HostToRendezvous::Register { endpoint, .. } = old else {
            panic!("a register frame must deserialize as one");
        };
        assert!(endpoint.is_none());
    }

    /// The listing carries the endpoint through, and an entry from a relay that
    /// never sends one is still readable — a client would otherwise fail to
    /// decode the whole browse response over one absent field.
    #[test]
    fn discover_entry_endpoint_is_optional() {
        let entry: DiscoverEntry = serde_json::from_str(
            r#"{"shortcode":"brave-otter-07","name":null,"description":null}"#,
        )
        .unwrap();
        assert!(entry.endpoint.is_none());
        assert_eq!(entry.idle_secs, 0);

        let with = DiscoverEntry {
            shortcode: "casa".into(),
            name: Some("Casa".into()),
            description: None,
            idle_secs: 3,
            endpoint: Some("ws://203.0.113.5:9000".into()),
            transport_key: None,
            transport_addrs: Vec::new(),
            relay_url: None,
        };
        let back: DiscoverEntry =
            serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
        assert_eq!(back, with);
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
                relay_url,
            } => {
                assert_eq!(shortcode, "brave-otter-07");
                assert!(voice_token_grant.is_none());
                assert!(livekit_url.is_none());
                assert!(relay_url.is_none());
            }
            other => panic!("parsed as {other:?}"),
        }
    }
}
