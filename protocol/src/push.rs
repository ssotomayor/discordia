//! The push relay's wire: what a phone signs, what a server posts, and what
//! the relay hands FCM. Here so the relay, the server and the client share it.

use serde::{Deserialize, Serialize};

/// The relay official builds use; a server and a client must name the same one
/// for a handle minted at one to mean anything to the other.
pub const DEFAULT_PUSH_RELAY: &str = "https://push.discordia.world";

/// First in every signed relay request, so none can equal a login, a
/// rendezvous ownership proof or a Nostr event id preimage.
pub const PUSH_DOMAIN: &str = "discordia-push-v1";

pub const PUBKEY_HEADER: &str = "X-Discordia-Pubkey";
pub const TIME_HEADER: &str = "X-Discordia-Time";
pub const SIGNATURE_HEADER: &str = "X-Discordia-Signature";

/// How far a signed request's time may sit from the relay's clock, either way.
pub const SIGNED_REQUEST_WINDOW_SECS: u64 = 300;

/// The largest `/v1/push` body. FCM caps a data message at 4096 bytes and the
/// handle, keys and framing ride beside it.
pub const MAX_PUSH_BODY_BYTES: usize = 2800;

/// A gift wrap up to this size travels inside the push; a larger one by id.
pub const INLINE_WRAP_MAX_BYTES: usize = 3000;

pub const HANDLE_BYTES: usize = 32;

/// The host part of the relay URL a client dialled, lowercased; the port is
/// left out so a TLS proxy in front does not change it.
pub fn push_audience(url: &str) -> Option<String> {
    let url = url::Url::parse(url.trim()).ok()?;
    Some(url.host_str()?.to_ascii_lowercase())
}

/// The bytes a phone signs and the relay verifies. `None` when a field could
/// blur the framing, and then neither side signs nor accepts.
pub fn request_payload(
    method: &str,
    path: &str,
    audience: &str,
    unix_secs: u64,
    pubkey: &str,
    body_sha256_hex: &str,
) -> Option<Vec<u8>> {
    let framed = |s: &str| !s.is_empty() && !s.chars().any(|c| c.is_control() || c == ' ');
    let method_ok = !method.is_empty() && method.bytes().all(|b| b.is_ascii_uppercase());
    let hash_ok = body_sha256_hex.len() == 64
        && body_sha256_hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    (method_ok
        && path.starts_with('/')
        && framed(path)
        && framed(audience)
        && crate::is_canonical_pubkey(pubkey)
        && hash_ok)
        .then(|| {
            format!("{PUSH_DOMAIN}\n{method}\n{path}\n{audience}\n{unix_secs}\n{pubkey}\n{body_sha256_hex}")
                .into_bytes()
        })
}

/// `PUT /v1/devices`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterDevice {
    pub token: String,
    pub package: String,
    /// Whether the relay should watch Nostr for this key's DMs.
    #[serde(default)]
    pub dm: bool,
    /// The relays this person's DMs arrive on.
    #[serde(default)]
    pub relays: Vec<String>,
    /// A rotated token's predecessor; its handles move to `token`.
    #[serde(default)]
    pub replaces: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRegistered {
    pub expires_at: u64,
}

/// `DELETE /v1/devices`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnregisterDevice {
    pub token: String,
}

/// `POST /v1/handles`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateHandle {
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleCreated {
    pub handle: String,
}

/// An FCM data message. FCM takes only string values, so every variant
/// serialises to a flat string-to-string map keyed by `t`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum PushData {
    #[serde(rename = "dm")]
    Dm {
        /// The recipient's key.
        p: String,
        #[serde(flatten)]
        wrap: DmWrap,
    },
    #[serde(rename = "m")]
    Mention {
        h: String,
        /// The server's NIP-44 ciphertext, as posted.
        c: String,
    },
    #[serde(rename = "ping")]
    Ping,
    /// A `t` from a newer relay; the phone ignores it.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DmWrap {
    /// The gift wrap event as JSON.
    Inline { e: String },
    /// Too large to carry: the phone fetches event `id` from relay `r`.
    ById { id: String, r: String },
}

/// What a mention push says once the phone opens it. Ciphertext to the relay
/// and to Google: `nip44::encrypt` under the handle's key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MentionPayload {
    pub v: u32,
    pub k: PushReason,
    pub guild: crate::Id,
    pub channel: crate::Id,
    pub message: crate::Id,
    pub author: String,
    #[serde(default)]
    pub author_name: String,
    #[serde(default)]
    pub guild_name: String,
    #[serde(default)]
    pub channel_name: String,
    /// Unix seconds; a read mark is milliseconds, so the phone scales before comparing.
    pub at: i64,
}

/// Why a server pushed. New reasons are new variants; an older phone reads
/// them as `Unknown` rather than failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushReason {
    Mention,
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const KEY: &str = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
    const HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn payload() -> Vec<u8> {
        request_payload(
            "PUT",
            "/v1/devices",
            "push.example",
            1_700_000_000,
            KEY,
            HASH,
        )
        .unwrap()
    }

    #[test]
    fn the_payload_is_the_documented_framing() {
        assert_eq!(
            String::from_utf8(payload()).unwrap(),
            format!("discordia-push-v1\nPUT\n/v1/devices\npush.example\n1700000000\n{KEY}\n{HASH}")
        );
    }

    #[test]
    fn every_field_changes_the_payload() {
        let other_key = "c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5";
        let other_hash = "a".repeat(64);
        let variants = [
            request_payload(
                "DELETE",
                "/v1/devices",
                "push.example",
                1_700_000_000,
                KEY,
                HASH,
            ),
            request_payload(
                "PUT",
                "/v1/handles",
                "push.example",
                1_700_000_000,
                KEY,
                HASH,
            ),
            request_payload(
                "PUT",
                "/v1/devices",
                "evil.example",
                1_700_000_000,
                KEY,
                HASH,
            ),
            request_payload(
                "PUT",
                "/v1/devices",
                "push.example",
                1_700_000_001,
                KEY,
                HASH,
            ),
            request_payload(
                "PUT",
                "/v1/devices",
                "push.example",
                1_700_000_000,
                other_key,
                HASH,
            ),
            request_payload(
                "PUT",
                "/v1/devices",
                "push.example",
                1_700_000_000,
                KEY,
                &other_hash,
            ),
        ];
        for v in variants {
            assert_ne!(v.unwrap(), payload());
        }
    }

    #[test]
    fn a_field_that_could_blur_the_framing_is_refused() {
        let upper = KEY.to_ascii_uppercase();
        let upper_hash = HASH.to_ascii_uppercase();
        let refused = [
            request_payload("put", "/v1/devices", "push.example", 0, KEY, HASH),
            request_payload("PUT", "v1/devices", "push.example", 0, KEY, HASH),
            request_payload("PUT", "/v1/devices\nx", "push.example", 0, KEY, HASH),
            request_payload("PUT", "/v1/devices", "", 0, KEY, HASH),
            request_payload("PUT", "/v1/devices", "push.example\n", 0, KEY, HASH),
            request_payload("PUT", "/v1/devices", "push.example", 0, &upper, HASH),
            request_payload("PUT", "/v1/devices", "push.example", 0, KEY, &upper_hash),
            request_payload("PUT", "/v1/devices", "push.example", 0, KEY, "abc"),
        ];
        for r in refused {
            assert!(r.is_none());
        }
    }

    #[test]
    fn the_audience_is_the_lowercased_host_without_port() {
        assert_eq!(
            push_audience("https://Push.Example:8443/x").as_deref(),
            Some("push.example")
        );
        assert_eq!(push_audience("not a url"), None);
    }

    #[test]
    fn relay_dtos_round_trip_and_optional_fields_default() {
        let full = RegisterDevice {
            token: "tok".into(),
            package: "com.discordia.app".into(),
            dm: true,
            relays: vec!["wss://relay.example".into()],
            replaces: Some("old".into()),
        };
        let back: RegisterDevice =
            serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
        assert_eq!(back, full);
        let lean: RegisterDevice = serde_json::from_str(r#"{"token":"t","package":"p"}"#).unwrap();
        assert!(!lean.dm && lean.relays.is_empty() && lean.replaces.is_none());

        let reg = DeviceRegistered { expires_at: 42 };
        assert_eq!(
            serde_json::from_str::<DeviceRegistered>(&serde_json::to_string(&reg).unwrap())
                .unwrap(),
            reg
        );
        let h = HandleCreated { handle: "h".into() };
        assert_eq!(serde_json::to_string(&h).unwrap(), r#"{"handle":"h"}"#);
        let c: CreateHandle = serde_json::from_str(r#"{"token":"t"}"#).unwrap();
        assert_eq!(c.token, "t");
        let u: UnregisterDevice = serde_json::from_str(r#"{"token":"t"}"#).unwrap();
        assert_eq!(u.token, "t");
    }

    fn as_map(data: &PushData) -> BTreeMap<String, String> {
        serde_json::from_value(serde_json::to_value(data).unwrap())
            .expect("FCM data must be a flat map of strings")
    }

    #[test]
    fn push_data_is_a_flat_string_map_in_every_shape() {
        let cases = [
            (
                PushData::Dm {
                    p: KEY.into(),
                    wrap: DmWrap::Inline { e: "{}".into() },
                },
                vec![("t", "dm"), ("p", KEY), ("e", "{}")],
            ),
            (
                PushData::Dm {
                    p: KEY.into(),
                    wrap: DmWrap::ById {
                        id: "ab".into(),
                        r: "wss://r".into(),
                    },
                },
                vec![("t", "dm"), ("p", KEY), ("id", "ab"), ("r", "wss://r")],
            ),
            (
                PushData::Mention {
                    h: "hh".into(),
                    c: "cc".into(),
                },
                vec![("t", "m"), ("h", "hh"), ("c", "cc")],
            ),
            (PushData::Ping, vec![("t", "ping")]),
        ];
        for (data, fields) in cases {
            let map = as_map(&data);
            let expected: BTreeMap<String, String> = fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            assert_eq!(map, expected);
            let back: PushData =
                serde_json::from_value(serde_json::to_value(&map).unwrap()).unwrap();
            assert_eq!(back, data);
        }
    }

    #[test]
    fn an_unknown_push_kind_still_parses() {
        let data: PushData = serde_json::from_str(r#"{"t":"call","x":"y"}"#).unwrap();
        assert_eq!(data, PushData::Unknown);
    }

    #[test]
    fn mention_payload_round_trips_and_tolerates_a_new_reason() {
        let m = MentionPayload {
            v: 1,
            k: PushReason::Mention,
            guild: crate::Id::from_u128(1),
            channel: crate::Id::from_u128(2),
            message: crate::Id::from_u128(3),
            author: KEY.into(),
            author_name: "ana".into(),
            guild_name: "g".into(),
            channel_name: "c".into(),
            at: 1_700_000_000,
        };
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(json["k"], "mention");
        assert_eq!(
            serde_json::from_value::<MentionPayload>(json.clone()).unwrap(),
            m
        );

        let mut newer = json;
        newer["k"] = "reply".into();
        newer.as_object_mut().unwrap().remove("author_name");
        let read: MentionPayload = serde_json::from_value(newer).unwrap();
        assert_eq!(read.k, PushReason::Unknown);
        assert_eq!(read.author_name, "");
    }
}
