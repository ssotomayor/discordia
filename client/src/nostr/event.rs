//! NIP-01 events: the envelope everything else in this module travels in.
//!
//! `blossom.rs` already built one of these by hand for its upload
//! authorization. This is the same construction, generalised — because gift
//! wrapping needs three of them per message, one signed by a key that exists
//! for a few microseconds and is never seen again.
//!
//! **The id is a hash of a canonical serialization, so the serialization has to
//! be exact.** `[0, pubkey, created_at, kind, tags, content]`, no whitespace,
//! JSON string escaping and nothing more. `serde_json` already emits precisely
//! that — compact, `\n`/`\t`/`\"`/`\\` in short form, other control characters
//! as `\u00xx`, UTF-8 passed through unescaped — which is why this builds a
//! `Value` and serializes it rather than formatting a string by hand. A single
//! byte of difference produces a different id, and an id that does not match
//! its content is an event every relay drops silently.

use secp256k1::{Keypair, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A signed Nostr event, as it appears on the wire.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Event {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u16,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

/// An event that is deliberately *not* signed.
///
/// NIP-59 calls this a rumor, and the lack of a signature is the point: it is
/// the innermost layer of a gift wrap, and a signature there would be a
/// portable proof of authorship that the recipient could show to anyone. Its id
/// is still computed, because that is what identifies the message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rumor {
    pub id: String,
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u16,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

/// The canonical serialization an event id is the SHA-256 of.
fn canonical(
    pubkey: &str,
    created_at: i64,
    kind: u16,
    tags: &[Vec<String>],
    content: &str,
) -> String {
    serde_json::json!([0, pubkey, created_at, kind, tags, content]).to_string()
}

/// `sha256` of the canonical form, as lowercase hex.
pub fn event_id(
    pubkey: &str,
    created_at: i64,
    kind: u16,
    tags: &[Vec<String>],
    content: &str,
) -> String {
    let bytes = canonical(pubkey, created_at, kind, tags, content);
    hex::encode(Sha256::digest(bytes.as_bytes()))
}

/// The x-only public key of `secret`, as the 64-char hex a Nostr pubkey is.
pub fn xonly_hex(secret: &SecretKey) -> String {
    let secp = Secp256k1::new();
    hex::encode(secret.x_only_public_key(&secp).0.serialize())
}

/// Build and sign an event with `secret`.
///
/// Takes a raw `SecretKey` rather than an `Identity` because the outermost
/// layer of a gift wrap is signed by a throwaway key that has no identity, no
/// display name and no persistence — that anonymity is the entire reason the
/// layer exists.
pub fn sign_with(
    secret: &SecretKey,
    created_at: i64,
    kind: u16,
    tags: Vec<Vec<String>>,
    content: String,
) -> Event {
    let pubkey = xonly_hex(secret);
    let id = event_id(&pubkey, created_at, kind, &tags, &content);
    let secp = Secp256k1::new();
    let keypair = Keypair::from_secret_key(&secp, secret);
    let digest: [u8; 32] = hex::decode(&id)
        .expect("event_id emits hex")
        .try_into()
        .expect("sha256 is 32 bytes");
    let msg = secp256k1::Message::from_digest(digest);
    let sig = hex::encode(secp.sign_schnorr_no_aux_rand(&msg, &keypair).serialize());
    Event {
        id,
        pubkey,
        created_at,
        kind,
        tags,
        content,
        sig,
    }
}

/// Build an unsigned rumor.
pub fn rumor(
    pubkey: &str,
    created_at: i64,
    kind: u16,
    tags: Vec<Vec<String>>,
    content: String,
) -> Rumor {
    Rumor {
        id: event_id(pubkey, created_at, kind, &tags, &content),
        pubkey: pubkey.to_string(),
        created_at,
        kind,
        tags,
        content,
    }
}

#[allow(dead_code)]
impl Event {
    /// Whether this event's id matches its content and its signature matches
    /// its id.
    ///
    /// Both halves matter and they fail differently: a wrong id means the event
    /// was rewritten in flight, a wrong signature means it was never signed by
    /// the key it claims. **Relays are not trusted to have checked either.**
    /// For a gift wrap this is the only thing standing between a relay and
    /// handing us a message attributed to someone who never sent it.
    pub fn verify(&self) -> bool {
        let expected = event_id(
            &self.pubkey,
            self.created_at,
            self.kind,
            &self.tags,
            &self.content,
        );
        if expected != self.id {
            return false;
        }
        let (Ok(id_bytes), Ok(sig_bytes), Ok(pk_bytes)) = (
            hex::decode(&self.id),
            hex::decode(&self.sig),
            hex::decode(&self.pubkey),
        ) else {
            return false;
        };
        let (Ok(digest), Ok(sig), Ok(pk)) = (
            <[u8; 32]>::try_from(id_bytes),
            secp256k1::schnorr::Signature::from_slice(&sig_bytes),
            secp256k1::XOnlyPublicKey::from_slice(&pk_bytes),
        ) else {
            return false;
        };
        Secp256k1::new()
            .verify_schnorr(&sig, &secp256k1::Message::from_digest(digest), &pk)
            .is_ok()
    }

    /// First value of the first tag named `name`, if any.
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.first().map(String::as_str) == Some(name))
            .and_then(|t| t.get(1))
            .map(String::as_str)
    }
}

impl Rumor {
    /// Same accessor as `Event::tag`, for the innermost layer.
    pub fn tag(&self, name: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.first().map(String::as_str) == Some(name))
            .and_then(|t| t.get(1))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SecretKey {
        SecretKey::from_slice(&[seed; 32]).expect("valid key")
    }

    /// A signed event verifies, and its id is a function of its content.
    #[test]
    fn a_signed_event_verifies() {
        let e = sign_with(&key(1), 1_700_000_000, 1, vec![], "hello".into());
        assert!(e.verify());
        assert_eq!(e.id.len(), 64);
        assert_eq!(e.pubkey, xonly_hex(&key(1)));
    }

    /// Every field is covered by the id, so editing any of them in flight is
    /// detected. This is what `verify` is for — relays are not trusted.
    #[test]
    fn editing_any_field_breaks_it() {
        let base = sign_with(
            &key(2),
            1_700_000_000,
            14,
            vec![vec!["p".into(), "abc".into()]],
            "pay me".into(),
        );
        for mutate in [
            (|e: &mut Event| e.content = "pay them".into()) as fn(&mut Event),
            |e: &mut Event| e.created_at += 1,
            |e: &mut Event| e.kind = 1,
            |e: &mut Event| e.tags.clear(),
        ] {
            let mut tampered = base.clone();
            mutate(&mut tampered);
            assert!(!tampered.verify(), "a rewritten event must not verify");
        }
        // And keeping the id while changing the signature fails the second half.
        let mut resigned = base.clone();
        resigned.sig = sign_with(
            &key(3),
            base.created_at,
            base.kind,
            base.tags.clone(),
            base.content.clone(),
        )
        .sig;
        assert!(
            !resigned.verify(),
            "a signature by another key must not verify"
        );
    }

    /// The canonical form is what the id hashes, so it must not gain
    /// whitespace, reorder fields, or escape anything JSON does not require.
    #[test]
    fn the_canonical_form_is_exactly_the_spec_s() {
        let s = canonical(
            "ab",
            1,
            14,
            &[vec!["p".into(), "cd".into()]],
            "hi \"you\"\n",
        );
        assert_eq!(s, r#"[0,"ab",1,14,[["p","cd"]],"hi \"you\"\n"]"#);
    }

    /// A rumor is identified the same way but carries no signature to strip.
    #[test]
    fn a_rumor_has_an_id_and_no_signature() {
        let pk = xonly_hex(&key(4));
        let r = rumor(&pk, 1_700_000_000, 14, vec![], "unsigned".into());
        assert_eq!(
            r.id,
            event_id(&pk, 1_700_000_000, 14, &[], "unsigned"),
            "a rumor is identified exactly like an event"
        );
        let json = serde_json::to_string(&r).expect("serialize");
        assert!(!json.contains("\"sig\""), "a rumor must carry no signature");
    }
}
