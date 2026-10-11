//! Here rather than in `dioxusfun-rendezvous` because the client speaks the
//! host side of it, and nothing depends on the relay crate.

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct VoiceEvictRequest {
    pub grant: String,
    pub channel_id: crate::Id,
    pub pubkey: String,
}

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
    /// Asked before the credentials in `Registered` expire; the stream stays up.
    RenewTurn {},
    /// The signature covers `LIST_NAMES`: the answer names unlisted codes,
    /// and a code is all it takes to connect.
    ListNames { pubkey: String, signature: String },
}

/// What a `ListNames` signature signs in place of a name.
pub const LIST_NAMES: &str = "dioxusfun:list-names";

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct ReservedName {
    pub code: String,
    /// `None` while a session holds it: a live name does not expire.
    pub expires_in_secs: Option<u64>,
}

/// Time-limited TURN credentials (the coturn REST scheme: the username is an
/// expiry, the credential an HMAC of it). Issued per registration, so a host
/// that stops registering stops being able to relay.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct TurnCredentials {
    pub urls: Vec<String>,
    pub username: String,
    pub credential: String,
    pub expires_unix: u64,
}

impl TurnCredentials {
    pub fn ice_servers(&self) -> Vec<crate::IceServer> {
        vec![crate::IceServer {
            urls: self.urls.clone(),
            username: self.username.clone(),
            credential: self.credential.clone(),
        }]
    }
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

/// The longest server name a host may claim, counted in characters.
pub const MAX_NAME_CHARS: usize = 64;

/// ASCII only, so a lookalike letter from another script cannot squat a code:
/// "Friday Game Night" answers to `friday-game-night`.
pub fn join_code(name: &str) -> Option<String> {
    use unicode_normalization::UnicodeNormalization as _;
    let mut code = String::new();
    let mut gap = false;
    for c in name.nfkd() {
        let spelled = match c {
            'ß' => "ss",
            'æ' | 'Æ' => "ae",
            'œ' | 'Œ' => "oe",
            'ø' | 'Ø' => "o",
            'đ' | 'Đ' => "d",
            'ł' | 'Ł' => "l",
            'þ' | 'Þ' => "th",
            _ => "",
        };
        if !spelled.is_empty() || c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            if gap && !code.is_empty() {
                code.push('-');
            }
            gap = false;
            if spelled.is_empty() {
                code.push(c.to_ascii_lowercase());
            } else {
                code.push_str(spelled);
            }
        } else if !matches!(c, '\'' | '\u{2019}' | '\u{2018}')
            && !unicode_normalization::char::is_combining_mark(c)
        {
            gap = true;
        }
    }
    code.truncate(MAX_NAME_CHARS);
    let code = code.trim_end_matches('-');
    (!code.is_empty()).then(|| code.to_string())
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<TurnCredentials>,
    },
    /// The answer to `RenewTurn`: fresh credentials for the same relay.
    Turn(TurnCredentials),
    Released {
        name: String,
    },
    Names {
        names: Vec<ReservedName>,
    },
    /// The same key registered this name again; this session no longer holds
    /// it, and registering back would take it from the newer one.
    Superseded {},
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
                turn,
            } => {
                assert!(turn.is_none());
                assert_eq!(shortcode, "brave-otter-07");
                assert!(voice_token_grant.is_none());
                assert!(livekit_url.is_none());
                assert!(relay_url.is_none());
            }
            other => panic!("parsed as {other:?}"),
        }
    }

    #[test]
    fn a_server_name_becomes_a_typeable_join_code() {
        for (name, code) in [
            ("Friday Game Night", "friday-game-night"),
            ("Teh's  server!", "tehs-server"),
            ("  Café Ñandú  ", "cafe-nandu"),
            ("Straße", "strasse"),
            ("v1.2_beta", "v1.2_beta"),
            ("A - B", "a-b"),
            ("ｗｉｄｅ", "wide"),
        ] {
            assert_eq!(join_code(name).as_deref(), Some(code), "{name}");
        }
    }

    #[test]
    fn a_join_code_is_its_own_code() {
        for code in ["purple-fox-42", "bitcoin-com", "v1.2_beta"] {
            assert_eq!(join_code(code).as_deref(), Some(code));
        }
    }

    #[test]
    fn a_name_with_nothing_typeable_has_no_code() {
        assert_eq!(join_code(""), None);
        assert_eq!(join_code(" -- !! "), None);
        assert_eq!(join_code("東京"), None);
    }

    #[test]
    fn a_long_name_is_cut_without_a_trailing_dash() {
        let code = join_code(&format!("{} b", "a".repeat(MAX_NAME_CHARS - 1))).unwrap();
        assert_eq!(code, "a".repeat(MAX_NAME_CHARS - 1));
    }
}
