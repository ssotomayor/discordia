//! Here rather than in `dioxusfun-rendezvous` because the client speaks the
//! host side of it, and nothing depends on the relay crate.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum HostToRendezvous {
    ReleaseName {
        name: String,
        pubkey: String,
        signature: String,
    },
    Register {
        name: Option<String>,
        #[serde(default)]
        pubkey: Option<String>,
        #[serde(default)]
        signature: Option<String>,
        #[serde(default)]
        publish_public: bool,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        endpoint: Option<String>,
        #[serde(default)]
        transport_key: Option<String>,
        #[serde(default)]
        transport_signature: Option<String>,
        #[serde(default)]
        transport_addrs: Vec<String>,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct DiscoverEntry {
    pub shortcode: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Missing from older relays, so the default means *fresh* — treating it
    /// as stale would show every host as unreachable.
    #[serde(default)]
    pub idle_secs: u64,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub transport_key: Option<String>,
    #[serde(default)]
    pub transport_addrs: Vec<String>,
    #[serde(default)]
    pub relay_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", content = "d", rename_all = "snake_case")]
pub enum RendezvousToHost {
    Challenge {
        nonce: String,
    },
    Registered {
        shortcode: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice_token_grant: Option<String>,
        livekit_url: Option<String>,
        #[serde(default)]
        relay_url: Option<String>,
    },
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

        let old: HostToRendezvous =
            serde_json::from_str(r#"{"op":"register","d":{"name":null}}"#).unwrap();
        let HostToRendezvous::Register { endpoint, .. } = old else {
            panic!("a register frame must deserialize as one");
        };
        assert!(endpoint.is_none());
    }

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
