//! NIP-01 kind:0 — what someone says about themselves.
//!
//! The last thing on the DM surface that still needed a server. A name and an
//! avatar used to come from the gateway's roster, so a conversation opened
//! without one showed a truncated hex key and kept showing it forever, because
//! `open_conversation` wrote the name it resolved at the time into `DmInfo`.
//! Nostr has always carried this; we simply never asked.
//!
//! **Anything here is self-declared and unverified beyond the signature.** The
//! event proves a key said it, not that any of it is true — the same standing
//! as `bot` and `client_version` in the Identify handshake. Two keys may claim
//! the same name, so wherever a name is shown the key has to remain reachable.
//!
//! Kind 0 is replaceable, like the contact list: relays keep only the newest
//! per author, so what arrives is the whole current claim rather than a patch.

use super::event::Event;

/// A profile metadata event.
pub const KIND_METADATA: u16 = 0;

/// The fields we use out of a kind:0. Everything else in there is ignored
/// rather than modelled — the spec puts no bound on what a client may add.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub name: Option<String>,
    pub picture: Option<String>,
}

/// Announce what to call us.
///
/// Reading kind:0 without writing one is half a feature and the wrong half.
/// Two Discordia users talking to each other would each ask the relays for the
/// other's name, and neither would ever have published one — so both would see
/// a hex key forever and conclude the lookup was broken. It is not; nobody was
/// answering.
///
/// Only `name` is set. The avatar lives on the gateway as a data URL and a
/// kind:0 `picture` is a URL other clients will fetch, so republishing one here
/// would mean hosting it somewhere public — a different decision, and not one
/// to make as a side effect of typing a display name.
pub fn ours(secret: &secp256k1::SecretKey, name: &str, now: i64) -> Event {
    let content = serde_json::json!({ "name": name }).to_string();
    super::event::sign_with(secret, now, KIND_METADATA, Vec::new(), content)
}

/// Read a kind:0 into the two fields we render.
///
/// `content` is a JSON *string* holding an object, so a malformed one is
/// ordinary rather than exceptional — other clients write what they like there,
/// and a wrong kind is simply not this. Both cases give `None` instead of an
/// error nobody could act on.
pub fn parse(event: &Event) -> Option<Metadata> {
    if event.kind != KIND_METADATA {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&event.content).ok()?;

    // `display_name` is the longer, prettier one and `name` is the handle.
    // Preferring display_name matches what other Nostr clients show, and
    // `displayName` is the misspelling enough of them emit to be worth reading.
    let name = ["display_name", "displayName", "name"]
        .iter()
        .find_map(|k| non_empty(v.get(k)));
    let picture = non_empty(v.get("picture"));

    // A kind:0 with neither is not worth a map entry; it would only shadow a
    // name we could otherwise get from a roster.
    (name.is_some() || picture.is_some()).then_some(Metadata { name, picture })
}

/// A JSON string field, if it is a string and holds more than whitespace.
///
/// Empty strings are common in the wild and are not a claim — treating one as a
/// name would replace a usable fallback with nothing.
fn non_empty(v: Option<&serde_json::Value>) -> Option<String> {
    let s = v?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: u16, content: &str) -> Event {
        Event {
            id: String::new(),
            pubkey: "a".repeat(64),
            created_at: 0,
            kind,
            tags: Vec::new(),
            content: content.into(),
            sig: String::new(),
        }
    }

    #[test]
    fn a_name_and_a_picture_are_read() {
        let m = parse(&ev(
            0,
            r#"{"name":"ana","picture":"https://example.test/a.png"}"#,
        ))
        .expect("metadata");
        assert_eq!(m.name.as_deref(), Some("ana"));
        assert_eq!(m.picture.as_deref(), Some("https://example.test/a.png"));
    }

    /// What other clients show, so what we show.
    #[test]
    fn display_name_wins_over_the_handle() {
        let m = parse(&ev(0, r#"{"name":"ana","display_name":"Ana Pérez"}"#)).expect("metadata");
        assert_eq!(m.name.as_deref(), Some("Ana Pérez"));
    }

    #[test]
    fn the_misspelled_display_name_is_read_too() {
        let m = parse(&ev(0, r#"{"name":"ana","displayName":"Ana Pérez"}"#)).expect("metadata");
        assert_eq!(m.name.as_deref(), Some("Ana Pérez"));
    }

    /// An empty string is not a claim. Storing one would shadow a name we could
    /// still get from a server's roster.
    #[test]
    fn blank_fields_are_not_claims() {
        assert_eq!(parse(&ev(0, r#"{"name":"   ","picture":""}"#)), None);
    }

    /// Other clients put whatever they like in `content`; a wrong shape is
    /// ordinary here, not exceptional.
    #[test]
    fn junk_content_is_not_an_error() {
        assert_eq!(parse(&ev(0, "not json at all")), None);
        assert_eq!(parse(&ev(0, "[1,2,3]")), None);
    }

    /// Reading kind:0 without writing one leaves every Discordia user asking a
    /// question none of them answers.
    #[test]
    fn our_own_metadata_round_trips_through_the_parser() {
        let secret = secp256k1::SecretKey::from_slice(&[7u8; 32]).expect("key");
        let event = ours(&secret, "RecknockT", 1_700_000_000);

        assert_eq!(event.kind, KIND_METADATA);
        assert_eq!(
            parse(&event).and_then(|m| m.name).as_deref(),
            Some("RecknockT")
        );
    }

    /// The avatar is a data URL on the gateway and `picture` is a URL other
    /// clients will fetch, so publishing one would mean hosting it somewhere
    /// public. That is a decision, not a side effect of typing a name.
    #[test]
    fn our_own_metadata_claims_no_picture() {
        let secret = secp256k1::SecretKey::from_slice(&[8u8; 32]).expect("key");
        let m = parse(&ours(&secret, "someone", 1)).expect("metadata");
        assert_eq!(m.picture, None);
    }

    #[test]
    fn another_kind_is_not_metadata() {
        assert_eq!(parse(&ev(1, r#"{"name":"ana"}"#)), None);
    }
}
