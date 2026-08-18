//! NIP-59 gift wrap — the layer that hides *who is talking* from the relay.
//!
//! NIP-44 hides what a message says. On its own that still leaves a relay
//! holding an event signed by your key and addressed to your correspondent, so
//! the conversation graph is public even though the words are not. Gift
//! wrapping fixes that with three nested layers:
//!
//! 1. **Rumor** — the real message, *unsigned*. Unsigned on purpose: a
//!    signature here would be a portable proof of authorship the recipient
//!    could show to anyone, so deniability is the default.
//! 2. **Seal** (kind 13) — the rumor, NIP-44 encrypted to the recipient and
//!    signed by the *sender's real key*. Only the recipient can open it, and
//!    only they learn who wrote it.
//! 3. **Gift wrap** (kind 1059) — the seal, encrypted again and signed by a
//!    **throwaway key generated for this one message and then dropped**. This
//!    is the layer the relay sees: an event from a pubkey that has never
//!    existed before and never will again, addressed to the recipient.
//!
//! So a relay learns "somebody sent this pubkey a message" and nothing else —
//! not who, not what, not even that it is a chat message rather than anything
//! else that gift wraps.
//!
//! **Timestamps are deliberately fuzzed.** Both outer layers are stamped up to
//! two days in the past, because an exact time is itself an identifier: two
//! wraps posted the same second correlate a sender with a recipient regardless
//! of what the signatures say.

use secp256k1::SecretKey;

use super::event::{self, Event, Rumor};
use super::nip44;

/// Kind of the middle layer.
pub const KIND_SEAL: u16 = 13;
/// Kind of the outermost layer, the only one a relay indexes.
pub const KIND_GIFT_WRAP: u16 = 1059;

/// How far back a wrapped event's timestamp may be pushed, in seconds.
const MAX_BACKDATE: i64 = 2 * 24 * 60 * 60;

/// A timestamp somewhere in the last two days.
///
/// Not a privacy nicety: without it, the seal and the wrap carry the same
/// second, which links the throwaway key to the moment the sender was online.
fn fuzzed(now: i64) -> i64 {
    use rand::Rng;
    now - rand::thread_rng().gen_range(0..=MAX_BACKDATE)
}

/// Wrap `rumor` for one recipient.
///
/// `now` is passed in rather than read here so a test can pin it; the fuzz is
/// applied on top.
pub fn wrap(
    sender_secret: &SecretKey,
    recipient_pubkey: &str,
    rumor: &Rumor,
    now: i64,
) -> Result<Event, String> {
    let seal_key = nip44::conversation_key(sender_secret, recipient_pubkey)?;
    let rumor_json = serde_json::to_string(rumor).map_err(|e| format!("encode rumor: {e}"))?;
    let sealed = nip44::encrypt(&seal_key, &rumor_json)?;
    // Tags must be empty. Anything here would be visible to whoever can open
    // the wrap, and there is nothing a seal needs to say in the clear.
    let seal = event::sign_with(sender_secret, fuzzed(now), KIND_SEAL, vec![], sealed);

    let ephemeral = random_secret();
    let wrap_key = nip44::conversation_key(&ephemeral, recipient_pubkey)?;
    let seal_json = serde_json::to_string(&seal).map_err(|e| format!("encode seal: {e}"))?;
    let wrapped = nip44::encrypt(&wrap_key, &seal_json)?;
    Ok(event::sign_with(
        &ephemeral,
        fuzzed(now),
        KIND_GIFT_WRAP,
        vec![vec!["p".to_string(), recipient_pubkey.to_string()]],
        wrapped,
    ))
}

/// Open a gift wrap addressed to us, returning the rumor inside.
///
/// Every check on the way in is load-bearing; see the comments. The one worth
/// naming here is the last: **the rumor's author must be the seal's signer.**
/// Without it, anyone could seal a rumor claiming to be from someone else, and
/// the recipient would render a message attributed to a person who never wrote
/// it — the whole scheme's authorship guarantee lives in that one comparison.
pub fn unwrap(our_secret: &SecretKey, gift: &Event) -> Result<Rumor, String> {
    if gift.kind != KIND_GIFT_WRAP {
        return Err(format!("not a gift wrap (kind {})", gift.kind));
    }
    // The relay is not trusted to have verified anything it hands us.
    if !gift.verify() {
        return Err("gift wrap signature does not verify".into());
    }
    let wrap_key = nip44::conversation_key(our_secret, &gift.pubkey)?;
    let seal_json = nip44::decrypt(&wrap_key, &gift.content)?;
    let seal: Event = serde_json::from_str(&seal_json).map_err(|e| format!("decode seal: {e}"))?;
    if seal.kind != KIND_SEAL {
        return Err(format!("inner event is not a seal (kind {})", seal.kind));
    }
    if !seal.verify() {
        return Err("seal signature does not verify".into());
    }
    let seal_key = nip44::conversation_key(our_secret, &seal.pubkey)?;
    let rumor_json = nip44::decrypt(&seal_key, &seal.content)?;
    let rumor: Rumor =
        serde_json::from_str(&rumor_json).map_err(|e| format!("decode rumor: {e}"))?;
    if rumor.pubkey != seal.pubkey {
        return Err("the message claims an author who did not seal it".into());
    }
    let expected = event::event_id(
        &rumor.pubkey,
        rumor.created_at,
        rumor.kind,
        &rumor.tags,
        &rumor.content,
    );
    if expected != rumor.id {
        return Err("the message id does not match its content".into());
    }
    Ok(rumor)
}

/// A fresh secp256k1 secret, for one gift wrap and then discarded.
fn random_secret() -> SecretKey {
    use rand::RngCore;
    loop {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        // Rejection sampling: a random 32 bytes is astronomically unlikely to
        // fall outside the curve order, but "astronomically unlikely" is not
        // "impossible", and the alternative is an unwrap that panics one time
        // in 2^128.
        if let Ok(k) = SecretKey::from_slice(&bytes) {
            return k;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::event::xonly_hex;

    fn key(seed: u8) -> SecretKey {
        SecretKey::from_slice(&[seed; 32]).expect("valid key")
    }

    fn a_rumor(from: &SecretKey, text: &str) -> Rumor {
        event::rumor(&xonly_hex(from), 1_700_000_000, 14, vec![], text.into())
    }

    /// The round trip: what one wraps, the other opens, and the author survives.
    #[test]
    fn a_wrapped_message_opens_for_its_recipient() {
        let (alice, bob) = (key(1), key(2));
        let r = a_rumor(&alice, "meet at the usual place");
        let gift = wrap(&alice, &xonly_hex(&bob), &r, 1_700_000_000).expect("wrap");
        let got = unwrap(&bob, &gift).expect("unwrap");
        assert_eq!(got.content, "meet at the usual place");
        assert_eq!(
            got.pubkey,
            xonly_hex(&alice),
            "authorship survives the wrap"
        );
    }

    /// What a relay can see: an event from a key that is not the sender's,
    /// addressed to the recipient, with nothing else in the clear.
    #[test]
    fn the_relay_learns_nothing_but_the_recipient() {
        let (alice, bob) = (key(1), key(2));
        let gift = wrap(
            &alice,
            &xonly_hex(&bob),
            &a_rumor(&alice, "secret"),
            1_700_000_000,
        )
        .expect("wrap");
        assert_ne!(
            gift.pubkey,
            xonly_hex(&alice),
            "the sender must not be on the wrap"
        );
        assert_ne!(gift.pubkey, xonly_hex(&bob));
        assert_eq!(gift.kind, KIND_GIFT_WRAP);
        assert_eq!(gift.tag("p"), Some(xonly_hex(&bob).as_str()));
        assert!(!gift.content.contains("secret"));
        assert_eq!(
            gift.tags.len(),
            1,
            "nothing but the recipient may be in the clear"
        );
    }

    /// Two wraps of the same message share no bytes and no ephemeral key, so a
    /// relay cannot link them.
    #[test]
    fn two_wraps_of_one_message_are_unlinkable() {
        let (alice, bob) = (key(1), key(2));
        let r = a_rumor(&alice, "same words");
        let a = wrap(&alice, &xonly_hex(&bob), &r, 1_700_000_000).expect("wrap");
        let b = wrap(&alice, &xonly_hex(&bob), &r, 1_700_000_000).expect("wrap");
        assert_ne!(a.pubkey, b.pubkey, "each wrap needs its own throwaway key");
        assert_ne!(a.content, b.content);
        assert_ne!(a.id, b.id);
    }

    /// A third party holding a perfectly good key gets nothing.
    #[test]
    fn an_outsider_cannot_open_it() {
        let (alice, bob, mallory) = (key(1), key(2), key(3));
        let gift = wrap(
            &alice,
            &xonly_hex(&bob),
            &a_rumor(&alice, "private"),
            1_700_000_000,
        )
        .expect("wrap");
        assert!(unwrap(&mallory, &gift).is_err());
    }

    /// **The forgery this scheme would otherwise allow.** Mallory seals a rumor
    /// that claims Alice wrote it. The seal is validly signed — by Mallory —
    /// so every signature check passes; only comparing the rumor's author to
    /// the seal's signer catches it.
    #[test]
    fn a_rumor_cannot_claim_an_author_who_did_not_seal_it() {
        let (alice, bob, mallory) = (key(1), key(2), key(3));
        let forged = event::rumor(
            &xonly_hex(&alice),
            1_700_000_000,
            14,
            vec![],
            "transfer everything to mallory".into(),
        );
        let gift = wrap(&mallory, &xonly_hex(&bob), &forged, 1_700_000_000).expect("wrap");
        let err = unwrap(&bob, &gift).expect_err("must be refused");
        assert!(err.contains("did not seal it"), "unexpected error: {err}");
    }

    /// Timestamps are pushed into the past, and not to the same value twice.
    #[test]
    fn timestamps_are_fuzzed_into_the_past() {
        let (alice, bob) = (key(1), key(2));
        let now = 1_700_000_000;
        let stamps: Vec<i64> = (0..12)
            .map(|_| {
                wrap(&alice, &xonly_hex(&bob), &a_rumor(&alice, "x"), now)
                    .expect("wrap")
                    .created_at
            })
            .collect();
        assert!(stamps.iter().all(|t| *t <= now && *t >= now - MAX_BACKDATE));
        assert!(
            stamps
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                > 1,
            "an unfuzzed timestamp would correlate sender and recipient"
        );
    }

    /// A wrap whose content was edited fails before anything is decrypted.
    #[test]
    fn a_tampered_wrap_is_refused() {
        let (alice, bob) = (key(1), key(2));
        let mut gift = wrap(
            &alice,
            &xonly_hex(&bob),
            &a_rumor(&alice, "original"),
            1_700_000_000,
        )
        .expect("wrap");
        gift.content.push('A');
        assert!(unwrap(&bob, &gift).is_err());
    }
}
