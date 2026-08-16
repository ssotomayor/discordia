//! End-to-end encryption for direct messages.
//!
//! A DM is stored and forwarded by the server like any other message, which
//! means the operator reads it. This seals the text — and any attachment —
//! before it leaves the machine, so what the server holds is a blob it cannot
//! open. `protocol::Message::enc` carries it; `content` carries only
//! `ENCRYPTED_PLACEHOLDER`, for clients too old to know about the field.
//!
//! **Why this is tractable here and not for guild channels.** A DM has exactly
//! two participants, both with known Nostr keys, so the key is *agreed* rather
//! than distributed: ECDH between the two identities, derived identically on
//! both sides, needing no message, no epoch and no designated sender. Every
//! open question in `mediakey`'s orchestration — who generates, who hands it to
//! an arrival, what happens when two members act at once — simply does not
//! arise at group size two. A guild channel has none of that luxury and is
//! deliberately out of scope.
//!
//! **What this does not give you.** The secret is derived from two *static*
//! identity keys, so there is no forward secrecy: whoever obtains a private key
//! can decrypt every DM it ever took part in, past included. NIP-44 has the
//! same property; fixing it means a ratchet, which is a much larger project.
//! For the threat this closes — "the person running the server reads my DMs" —
//! static agreement is the right size. Metadata is untouched either way: the
//! server still sees who talks to whom, when, and roughly how much.
//!
//! **Why not NIP-44.** It is the obvious candidate — this is a Nostr identity,
//! and NIP-44 is the reviewed standard for exactly this. It was not used
//! because it would mean either hand-rolling a spec (its whole point is not
//! doing that) or taking on the `nostr` crate for one function. Instead this
//! reuses the construction `mediakey` already relies on, under its own domain,
//! and writes a version byte into every payload so moving to NIP-44 later is a
//! decode branch rather than a migration. See `TODO.md`.

use chacha20poly1305::aead::{Aead, KeyInit, OsRng, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};

/// Nonce length for XChaCha20-Poly1305.
///
/// The extended nonce is what makes random generation safe: at 24 bytes a
/// collision is not something to reason about, where a 12-byte nonce reused
/// across two messages under one key would leak both.
const NONCE_LEN: usize = 24;

/// Payload format version, first byte of every sealed blob.
///
/// Present so a later scheme — NIP-44, or a ratchet — can be introduced by
/// reading this and branching, instead of by a flag day where old and new
/// clients cannot read each other with no way to tell which they are facing.
const VERSION: u8 = 1;

/// Longest plaintext we will seal, matching the server's own message cap.
const MAX_PLAINTEXT: usize = 2000;

/// What actually gets sealed.
///
/// A struct rather than the raw text, because the attachment's key has to
/// travel *inside* the encryption — the whole point is that the server learns
/// neither the picture nor what kind of picture it is.
#[derive(Serialize, Deserialize)]
pub struct Sealed {
    /// The message text. Empty is legal when there is an attachment.
    pub text: String,
    /// Present when the message carries an attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<SealedImage>,
}

/// How to open an attachment whose bytes the server is holding but cannot read.
#[derive(Serialize, Deserialize)]
pub struct SealedImage {
    /// Hex key for the blob, generated per attachment.
    ///
    /// Per attachment, not per conversation, and that is deliberate: it means
    /// two encryptions of the same picture produce different ciphertext, so the
    /// content-addressed store cannot tell the operator that two people sent
    /// the same image.
    pub key: String,
    /// The real mime of the plaintext, e.g. `image/png`. Needed to rebuild a
    /// data URL the webview will render, and withheld from the server because
    /// "they sent a GIF" is itself worth something.
    pub mime: String,
}

/// Pad a plaintext so its ciphertext length says as little as possible.
///
/// Without this the blob length tracks the message length to the byte, and "you
/// replied with two characters" is legible to anyone holding the database. The
/// scheme is the simple one NIP-44 also uses: round up to a power of two (with
/// a floor, so short messages are indistinguishable from each other), and
/// prefix the true length so unpadding is exact.
///
/// It bounds the leak rather than removing it — a 5 KB message is still
/// visibly larger than a 20-byte one. Buying more than that means padding
/// everything to a constant, which costs bandwidth on every message to hide a
/// distinction few threats care about.
fn pad(plain: &[u8]) -> Vec<u8> {
    let len = plain.len();
    let target = padded_len(len);
    let mut out = Vec::with_capacity(2 + target);
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(plain);
    out.resize(2 + target, 0);
    out
}

/// The padded size for a plaintext of `len` bytes: the next power of two, never
/// below 32.
fn padded_len(len: usize) -> usize {
    len.max(32).next_power_of_two()
}

/// Undo `pad`, rejecting a length prefix that does not fit what arrived.
///
/// The check matters: the length is attacker-controlled in the sense that a
/// corrupted or hostile payload can carry any value, and slicing on it blindly
/// is how a decrypt turns into a panic.
fn unpad(padded: &[u8]) -> Result<Vec<u8>, String> {
    if padded.len() < 2 {
        return Err("sealed payload is too short to carry a length".into());
    }
    let len = u16::from_be_bytes([padded[0], padded[1]]) as usize;
    let body = &padded[2..];
    if len > body.len() {
        return Err("sealed payload claims more text than it carries".into());
    }
    Ok(body[..len].to_vec())
}

/// Seal a message to the other participant in a DM.
///
/// `peer_pubkey` is the *other* person, whichever direction the message goes:
/// ECDH is symmetric, so the sender sealing to them and the recipient opening
/// from us derive the same secret. That is why nothing here needs to know who
/// wrote the message.
pub fn seal(
    sealed: &Sealed,
    peer_pubkey: &str,
    identity: &crate::identity::Identity,
) -> Result<String, String> {
    if sealed.text.len() > MAX_PLAINTEXT {
        return Err(format!("message is longer than {MAX_PLAINTEXT} characters"));
    }
    let json = serde_json::to_vec(sealed).map_err(|e| format!("cannot encode the message: {e}"))?;
    let secret = identity.dm_secret_with(peer_pubkey)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&secret));

    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);

    // The version byte is authenticated as associated data rather than merely
    // prepended, so it cannot be rewritten to steer a future client down a
    // different decode path without the tag failing.
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: &pad(&json),
                aad: &[VERSION],
            },
        )
        .map_err(|_| "sealing the message failed".to_string())?;

    let mut blob = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    blob.push(VERSION);
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(base64_encode(&blob))
}

/// Open a sealed message from the other participant.
///
/// Failure is ordinary rather than exceptional — a message sealed to a key we
/// no longer hold, or by a client speaking a version we do not — so this
/// returns an error the UI can render in place of the text instead of anything
/// louder.
pub fn open(
    payload: &str,
    peer_pubkey: &str,
    identity: &crate::identity::Identity,
) -> Result<Sealed, String> {
    let blob = base64_decode(payload)?;
    if blob.len() < 1 + NONCE_LEN {
        return Err("sealed payload is truncated".into());
    }
    let version = blob[0];
    if version != VERSION {
        return Err(format!(
            "this message uses encryption version {version}; this build understands {VERSION}"
        ));
    }
    let nonce = XNonce::from_slice(&blob[1..1 + NONCE_LEN]);
    let secret = identity.dm_secret_with(peer_pubkey)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&secret));
    let padded = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &blob[1 + NONCE_LEN..],
                aad: &[version],
            },
        )
        .map_err(|_| "could not decrypt — wrong key, or the message was altered".to_string())?;
    let json = unpad(&padded)?;
    serde_json::from_slice(&json).map_err(|e| format!("sealed payload is not a message: {e}"))
}

/// Seal attachment bytes under their own one-off key.
///
/// Returns the ciphertext and the hex key to put in `SealedImage`. The bytes go
/// through the ordinary blob path afterwards, so the server still hashes,
/// shares and sweeps them — it simply cannot decode them.
pub fn seal_image(plain: &[u8]) -> Result<(Vec<u8>, String), String> {
    let mut key_bytes = [0u8; 32];
    OsRng.fill_bytes(&mut key_bytes);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key_bytes));
    let mut nonce_bytes = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plain)
        .map_err(|_| "sealing the attachment failed".to_string())?;
    let mut blob = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    blob.push(VERSION);
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok((blob, hex::encode(key_bytes)))
}

/// Open attachment bytes with the key from the message's sealed payload.
pub fn open_image(blob: &[u8], key_hex: &str) -> Result<Vec<u8>, String> {
    if blob.len() < 1 + NONCE_LEN {
        return Err("attachment is truncated".into());
    }
    if blob[0] != VERSION {
        return Err(format!("attachment uses encryption version {}", blob[0]));
    }
    let key_bytes = hex::decode(key_hex).map_err(|e| format!("attachment key not hex: {e}"))?;
    if key_bytes.len() != 32 {
        return Err("attachment key is not 32 bytes".into());
    }
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key_bytes));
    let nonce = XNonce::from_slice(&blob[1..1 + NONCE_LEN]);
    cipher
        .decrypt(nonce, &blob[1 + NONCE_LEN..])
        .map_err(|_| "could not decrypt the attachment".to_string())
}

/// Turn a composed message into the three wire fields a sealed DM travels as.
///
/// Returns `(content, image, enc)`:
/// - `content` is the placeholder, because the real text is inside `enc`;
/// - `image` is the attachment re-encrypted under its own key and re-wrapped as
///   a data URL the media store will accept and hash without being able to read;
/// - `enc` is the sealed payload carrying the text and the attachment's key.
///
/// The attachment's *mime* goes inside the seal rather than staying on the data
/// URL, so the server does not learn what kind of file it is holding.
pub fn seal_for_dm(
    text: &str,
    image: Option<&str>,
    peer_pubkey: &str,
    identity: &crate::identity::Identity,
) -> Result<(String, Option<String>, Option<String>), String> {
    let (sealed_image, wire_image) = match image {
        Some(data_url) => {
            let (mime, bytes) = decode_data_url(data_url)?;
            let (ciphertext, key) = seal_image(&bytes)?;
            let wire = format!(
                "data:{};base64,{}",
                ENCRYPTED_BLOB_MIME,
                base64_encode(&ciphertext)
            );
            (Some(SealedImage { key, mime }), Some(wire))
        }
        None => (None, None),
    };
    let payload = seal(
        &Sealed {
            text: text.to_string(),
            image: sealed_image,
        },
        peer_pubkey,
        identity,
    )?;
    Ok((
        crate::protocol::ENCRYPTED_PLACEHOLDER.to_string(),
        wire_image,
        Some(payload),
    ))
}

/// Undo `seal_for_dm` on the receiving side, in place.
///
/// Rewrites `content` to the decrypted text and `image` back to a real data
/// URL. On failure it leaves a readable explanation in `content` rather than
/// erroring: a DM that cannot be opened still has to render as *something*, and
/// a blank bubble is indistinguishable from a bug.
pub fn open_in_place(
    message: &mut crate::protocol::Message,
    peer_pubkey: &str,
    identity: &crate::identity::Identity,
) {
    let Some(payload) = message.enc.clone() else {
        return;
    };
    match open(&payload, peer_pubkey, identity) {
        Ok(sealed) => {
            message.content = sealed.text;
            message.image = match (sealed.image, message.image.take()) {
                (Some(info), Some(wire)) => decode_data_url(&wire)
                    .and_then(|(_, bytes)| open_image(&bytes, &info.key))
                    .map(|plain| format!("data:{};base64,{}", info.mime, base64_encode(&plain)))
                    .ok(),
                _ => None,
            };
        }
        Err(e) => {
            message.content = format!("[could not decrypt: {e}]");
            message.image = None;
        }
    }
}

/// Split a `data:<mime>;base64,<payload>` URL into its mime and bytes.
fn decode_data_url(url: &str) -> Result<(String, Vec<u8>), String> {
    let rest = url
        .strip_prefix("data:")
        .ok_or_else(|| "attachment is not a data URL".to_string())?;
    let (mime, payload) = rest
        .split_once(";base64,")
        .ok_or_else(|| "attachment is not base64".to_string())?;
    Ok((mime.to_string(), base64_decode(payload)?))
}

/// Mime marking a blob whose bytes the client sealed. Must match the server's
/// `media::ENCRYPTED_BLOB_MIME` — the store rejects anything it does not know.
const ENCRYPTED_BLOB_MIME: &str = "application/vnd.discordia.enc";

// Standard alphabet with padding, matching what the rest of the client uses for
// data URLs.
fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|e| format!("sealed payload is not base64: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    /// Deterministic identities, matching `mediakey`'s test helper — a seeded
    /// key makes a failure reproducible instead of a coin flip.
    fn identity(seed: u8) -> Identity {
        Identity::restore_from_private_key(hex::encode([seed; 32]), format!("id{seed}"))
            .expect("identity")
    }

    fn pair() -> (Identity, Identity) {
        (identity(1), identity(2))
    }

    /// The property the whole feature rests on: what one seals, the other
    /// opens, without either having sent the other a key.
    #[test]
    fn a_message_survives_the_round_trip() {
        let (alice, bob) = pair();
        let msg = Sealed {
            text: "meet me at the usual place".into(),
            image: None,
        };
        let blob = seal(&msg, &bob.pubkey, &alice).unwrap();
        let opened = open(&blob, &alice.pubkey, &bob).unwrap();
        assert_eq!(opened.text, "meet me at the usual place");
    }

    /// And the sender can read their own history back — they hold only the
    /// peer's pubkey, not a copy of what they sent.
    #[test]
    fn the_sender_can_reopen_their_own_message() {
        let (alice, bob) = pair();
        let msg = Sealed {
            text: "note to self, via bob".into(),
            image: None,
        };
        let blob = seal(&msg, &bob.pubkey, &alice).unwrap();
        let opened = open(&blob, &bob.pubkey, &alice).unwrap();
        assert_eq!(opened.text, "note to self, via bob");
    }

    /// A third party with a perfectly good key of their own gets nothing.
    #[test]
    fn an_outsider_cannot_open_it() {
        let (alice, bob) = pair();
        let mallory = identity(3);
        let blob = seal(
            &Sealed {
                text: "private".into(),
                image: None,
            },
            &bob.pubkey,
            &alice,
        )
        .unwrap();
        assert!(open(&blob, &alice.pubkey, &mallory).is_err());
        assert!(open(&blob, &mallory.pubkey, &bob).is_err());
    }

    /// The DM secret must not be the media-key secret. If these ever coincide,
    /// everyone in a voice channel holds the key to your DMs with them.
    #[test]
    fn the_dm_secret_is_not_the_media_secret() {
        let (alice, bob) = pair();
        assert_ne!(
            alice.dm_secret_with(&bob.pubkey).unwrap(),
            alice.shared_secret_with(&bob.pubkey).unwrap()
        );
        // Still symmetric, which is what lets both ends derive it alone.
        assert_eq!(
            alice.dm_secret_with(&bob.pubkey).unwrap(),
            bob.dm_secret_with(&alice.pubkey).unwrap()
        );
    }

    /// Tampering is detected rather than decoded into something plausible.
    #[test]
    fn a_flipped_bit_fails_the_tag() {
        let (alice, bob) = pair();
        let blob = seal(
            &Sealed {
                text: "transfer 10".into(),
                image: None,
            },
            &bob.pubkey,
            &alice,
        )
        .unwrap();
        let mut raw = base64_decode(&blob).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        assert!(open(&base64_encode(&raw), &alice.pubkey, &bob).is_err());
    }

    /// Short messages must not be distinguishable by length. Two messages of
    /// very different sizes below the floor should seal to the same size.
    #[test]
    fn padding_hides_short_lengths() {
        let (alice, bob) = pair();
        let seal_len = |t: &str| {
            base64_decode(
                &seal(
                    &Sealed {
                        text: t.into(),
                        image: None,
                    },
                    &bob.pubkey,
                    &alice,
                )
                .unwrap(),
            )
            .unwrap()
            .len()
        };
        assert_eq!(seal_len("y"), seal_len("no"));
        assert_eq!(padded_len(1), 32);
        assert_eq!(padded_len(33), 64);
        assert_eq!(padded_len(500), 512);
    }

    /// A hostile length prefix must be refused, not sliced on.
    #[test]
    fn a_lying_length_prefix_is_refused() {
        let mut padded = vec![0xff, 0xff];
        padded.extend_from_slice(b"short");
        assert!(unpad(&padded).is_err());
        assert!(unpad(&[]).is_err());
    }

    /// Attachments round-trip under their own key, and two seals of identical
    /// bytes differ — which is what stops the blob store revealing that two
    /// people sent the same picture.
    #[test]
    fn an_attachment_round_trips_and_does_not_dedup() {
        let plain = b"\x89PNG\r\n\x1a\n pretend this is a picture";
        let (blob_a, key_a) = seal_image(plain).unwrap();
        let (blob_b, _key_b) = seal_image(plain).unwrap();
        assert_ne!(blob_a, blob_b, "identical bytes must not seal identically");
        assert_eq!(open_image(&blob_a, &key_a).unwrap(), plain);
        assert!(open_image(&blob_a, &hex::encode([0u8; 32])).is_err());
    }
}
