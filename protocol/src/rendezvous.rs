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
        transport_key: Option<String>,
        #[serde(default)]
        transport_signature: Option<String>,
        #[serde(default)]
        transport_addrs: Vec<String>,
        #[serde(default)]
        location: Option<GeoPoint>,
    },
}

/// Where a host says it is. Self-declared and unverified, like `bot` in
/// `Identify`: nothing may geolocate an address to fill it in.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
}

impl GeoPoint {
    /// One decimal is a town, not a doorstep; anything finer is not kept.
    pub fn coarse(self) -> Option<Self> {
        let ok = |v: f64, max: f64| v.is_finite() && v.abs() <= max;
        (ok(self.lat, 90.0) && ok(self.lon, 180.0)).then(|| Self {
            lat: (self.lat * 10.0).round() / 10.0,
            lon: (self.lon * 10.0).round() / 10.0,
        })
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct DiscoverEntry {
    pub shortcode: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// Missing from older relays, so the default means *fresh* — treating it
    /// as stale would show every host as unreachable.
    #[serde(default)]
    pub idle_secs: u64,
    #[serde(default)]
    pub transport_key: Option<String>,
    #[serde(default)]
    pub transport_addrs: Vec<String>,
    #[serde(default)]
    pub relay_url: Option<String>,
    #[serde(default)]
    pub location: Option<GeoPoint>,
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
            transport_key: None,
            transport_signature: None,
            transport_addrs: Vec::new(),
            location: None,
        })
        .unwrap();
        assert_eq!(json["op"], "register");
        let d = &json["d"];
        assert_eq!(d["name"], "casa");
        assert_eq!(d["publish_public"], true);
        assert!(d["description"].is_null());
    }

    #[test]
    fn discover_entry_tolerates_missing_fields() {
        let entry: DiscoverEntry = serde_json::from_str(
            r#"{"shortcode":"brave-otter-07","name":null,"description":null}"#,
        )
        .unwrap();
        assert!(entry.transport_key.is_none());
        assert_eq!(entry.idle_secs, 0);

        let with = DiscoverEntry {
            shortcode: "casa".into(),
            name: Some("Casa".into()),
            description: None,
            idle_secs: 3,
            transport_key: Some("ab".repeat(32)),
            transport_addrs: vec!["203.0.113.5:4433".into()],
            relay_url: None,
            location: Some(GeoPoint {
                lat: 17.3,
                lon: -62.7,
            }),
        };
        let back: DiscoverEntry =
            serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
        assert_eq!(back, with);
    }

    #[test]
    fn a_location_is_rounded_to_a_town_and_nonsense_is_dropped() {
        let p = GeoPoint {
            lat: 17.2971,
            lon: -62.7237,
        }
        .coarse()
        .unwrap();
        assert_eq!(
            p,
            GeoPoint {
                lat: 17.3,
                lon: -62.7
            }
        );
        assert!(
            GeoPoint {
                lat: 91.0,
                lon: 0.0
            }
            .coarse()
            .is_none()
        );
        assert!(
            GeoPoint {
                lat: 0.0,
                lon: -180.5
            }
            .coarse()
            .is_none()
        );
        assert!(
            GeoPoint {
                lat: f64::NAN,
                lon: 0.0
            }
            .coarse()
            .is_none()
        );
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
