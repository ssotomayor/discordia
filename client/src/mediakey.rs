//! The channel media key, and how it reaches the people entitled to it.
//!
//! Stage 3's second half. `e2ee` proved media *can* be encrypted; this is what
//! makes it worth doing, because a key typed into two machines by hand is not a
//! feature. The requirement is narrow and awkward: every member of a voice
//! channel needs the same 32 bytes, the server has to carry them, and the
//! server must not learn them.
//!
//! So the key is never sent — it is *sealed*, once per recipient, to a secret
//! only that recipient and the sender can compute (ECDH over the Nostr identity
//! keys both already have; see `identity::shared_secret_with`). What crosses the
//! gateway is ciphertext addressed to one pubkey. The server routes it without
//! being able to read it, and needs no new trust.
//!
//! **Epochs, not rotation in place.** A key carries a number that only goes up.
//! A member kicked from a guild keeps whatever they captured, and keeps the key
//! they were given — so the remaining members move to a new epoch and the old
//! one becomes useless for anything published afterwards. Without that, kicking
//! somebody out of a call is theatre. The epoch is what lets a client tell "this
//! is the key I already have" from "this is newer, take it".
//!
//! What this does *not* do is hide who is talking to whom, or when. The server
//! sees a sealed key move from one member to another, and that is inherent to it
//! carrying anything at all.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

use crate::identity::Identity;

/// Bytes in a media key. LiveKit derives its own frame keys from this, so its
/// only job is to be unguessable.
pub const KEY_LEN: usize = 32;

/// Nonce length for XChaCha20-Poly1305.
///
/// The extended nonce is the reason for choosing XChaCha: a key is resealed
/// per member and per epoch with a random nonce, and 96 bits is not a
/// comfortable margin for that. 192 bits is.
const NONCE_LEN: usize = 24;

/// A freshly generated channel key.
pub fn generate() -> [u8; KEY_LEN] {
    use rand::RngCore;
    let mut key = [0u8; KEY_LEN];
    rand::rngs::OsRng.fill_bytes(&mut key);
    key
}

/// Seal `key` so that only the holder of `to_pubkey` can open it.
///
/// The epoch is authenticated but not hidden: it travels in the clear on the
/// wire anyway, and binding it here stops a relay or a server reordering
/// epochs to push members back onto a key a removed member still holds.
pub fn seal(
    key: &[u8; KEY_LEN],
    to_pubkey: &str,
    epoch: u32,
    identity: &Identity,
) -> Result<String, String> {
    let shared = identity.shared_secret_with(to_pubkey)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&shared));

    let mut nonce = [0u8; NONCE_LEN];
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(&mut nonce);

    let payload = chacha20poly1305::aead::Payload {
        msg: key.as_slice(),
        aad: &aad(epoch, to_pubkey),
    };
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), payload)
        .map_err(|_| "sealing the media key failed".to_string())?;

    let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ct);
    Ok(hex::encode(blob))
}

/// Open a key sealed to us by `from_pubkey`.
///
/// Failure here is ordinary, not exceptional: a stale epoch, a member who left
/// mid-rekey, or a blob addressed to somebody else all land here. The caller
/// keeps the key it has.
pub fn open(
    blob: &str,
    from_pubkey: &str,
    epoch: u32,
    identity: &Identity,
) -> Result<[u8; KEY_LEN], String> {
    let raw = hex::decode(blob).map_err(|e| format!("sealed key not hex: {e}"))?;
    if raw.len() <= NONCE_LEN {
        return Err("sealed key is too short to contain anything".into());
    }
    let (nonce, ct) = raw.split_at(NONCE_LEN);

    let shared = identity.shared_secret_with(from_pubkey)?;
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&shared));
    let payload = chacha20poly1305::aead::Payload {
        msg: ct,
        // Our *own* pubkey: the sender sealed it to us, so this is the only
        // value that can reproduce the tag. A blob addressed to somebody else
        // fails here rather than decrypting to nonsense.
        aad: &aad(epoch, &identity.pubkey),
    };
    let opened = cipher
        .decrypt(XNonce::from_slice(nonce), payload)
        .map_err(|_| "this sealed key is not for us, or has been tampered with".to_string())?;

    opened
        .try_into()
        .map_err(|_| "sealed key had the wrong length".to_string())
}

/// Associated data: what this ciphertext is *for*. Not secret, but bound, so a
/// blob cannot be replayed under a different epoch or at a different member.
fn aad(epoch: u32, recipient: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(8 + recipient.len());
    aad.extend_from_slice(b"dioxusfun/media-key/v1");
    aad.extend_from_slice(&epoch.to_be_bytes());
    aad.extend_from_slice(recipient.as_bytes());
    aad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(seed: u8) -> Identity {
        Identity::restore_from_private_key(hex::encode([seed; 32]), format!("id{seed}"))
            .expect("identity")
    }

    /// The whole point: the intended recipient gets the key back, byte for byte.
    #[test]
    fn a_sealed_key_opens_for_its_recipient() {
        let alice = identity(1);
        let bob = identity(2);
        let key = generate();

        let blob = seal(&key, &bob.pubkey, 7, &alice).unwrap();
        assert_eq!(open(&blob, &alice.pubkey, 7, &bob).unwrap(), key);
    }

    /// And the other half of the point: nobody else does. This is the property
    /// the server's ability to carry the blob depends on — it holds the
    /// ciphertext and cannot be a recipient.
    #[test]
    fn nobody_else_can_open_it() {
        let alice = identity(1);
        let bob = identity(2);
        let eve = identity(3);
        let key = generate();

        let blob = seal(&key, &bob.pubkey, 7, &alice).unwrap();
        assert!(open(&blob, &alice.pubkey, 7, &eve).is_err());
        // Not even by claiming a different sender.
        assert!(open(&blob, &eve.pubkey, 7, &bob).is_err());
    }

    /// The epoch is bound, so a blob cannot be replayed to push a member back
    /// onto a key a removed member still holds.
    #[test]
    fn an_epoch_cannot_be_swapped_under_the_ciphertext() {
        let alice = identity(1);
        let bob = identity(2);
        let key = generate();

        let blob = seal(&key, &bob.pubkey, 7, &alice).unwrap();
        assert!(open(&blob, &alice.pubkey, 8, &bob).is_err());
        assert!(open(&blob, &alice.pubkey, 6, &bob).is_err());
    }

    /// Two seals of the same key differ, because the nonce is fresh. Equal
    /// ciphertexts would leak that a rekey did not happen.
    #[test]
    fn sealing_twice_does_not_repeat() {
        let alice = identity(1);
        let bob = identity(2);
        let key = generate();

        let a = seal(&key, &bob.pubkey, 1, &alice).unwrap();
        let b = seal(&key, &bob.pubkey, 1, &alice).unwrap();
        assert_ne!(a, b);
        // Both still open to the same bytes.
        assert_eq!(open(&a, &alice.pubkey, 1, &bob).unwrap(), key);
        assert_eq!(open(&b, &alice.pubkey, 1, &bob).unwrap(), key);
    }

    /// Generated keys are not a constant, and not each other.
    #[test]
    fn generated_keys_differ() {
        assert_ne!(generate(), generate());
        assert_ne!(generate(), [0u8; KEY_LEN]);
    }

    /// ECDH is symmetric: each side derives the same secret from the other's
    /// public key. If this ever stopped holding, sealing would still "work" and
    /// opening would fail for everyone — so it is pinned directly.
    #[test]
    fn both_sides_derive_the_same_secret() {
        let alice = identity(1);
        let bob = identity(2);
        assert_eq!(
            alice.shared_secret_with(&bob.pubkey).unwrap(),
            bob.shared_secret_with(&alice.pubkey).unwrap()
        );
    }
}

// ---------------------------------------------------------------------------
// Orchestration: who generates a key, who hands it on, and when it changes
// ---------------------------------------------------------------------------

use dioxus::prelude::*;

use crate::protocol::{ClientMessage, Id};
use crate::state::{use_app_state, use_gateway};

/// How long a joiner waits to be given the channel's key before generating one.
///
/// Someone already in the channel should send it within a round trip. Waiting
/// past that means nobody is going to — every other member is on an older
/// build, or the message was lost — and a call with no key at all is worse than
/// a call whose key changed once at the start.
const KEY_WAIT: std::time::Duration = std::time::Duration::from_secs(4);

/// Who, of the members present, drives a *rekey* after somebody is removed.
///
/// The lowest pubkey — deterministic, needing no coordination and no state.
/// Exclusivity is worth having here because a removal is a discrete event every
/// remaining member observes at once, and without a rule each of them would
/// mint a key for the same event.
///
/// It is deliberately **not** used for the two jobs it used to do. Handing an
/// existing key to an arrival is gated on *holding* the key, because being
/// lowest says nothing about having one. And generating a first key is no
/// longer exclusive at all: `net::supersedes` makes two keys converge, so the
/// worse failure is a member who waits for a key nobody was going to send.
///
/// `None` when the set is empty, which is the caller's cue that there is nobody
/// to be responsible.
fn designated<'a>(present: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    present.min()
}

/// Everyone in `channel`, by pubkey.
fn present_in(state: &crate::state::AppState, channel: Id) -> Vec<String> {
    state
        .voice_states
        .iter()
        .filter(|v| v.channel_id == Some(channel))
        .map(|v| v.user_pubkey.clone())
        .collect()
}

/// Keeps a channel's media key alive: generates one when nobody else will, hands
/// it to arrivals, and replaces it when someone is removed from the guild.
///
/// Renders nothing. Mounted once at the workspace root beside the other
/// bridges — it reacts to the voice roster, which is already kept current by
/// `VoiceStateUpdate`, so it needs no protocol of its own beyond the sealed
/// hand-off.
#[component]
pub fn MediaKeyBridge() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();

    // A member was removed from a guild: roll the key so what follows is beyond
    // them. Watched here rather than done in `apply`, which has no gateway to
    // send on.
    let rekey_gateway = gateway.clone();
    use_effect(move || {
        if !state.read().pending_rekey {
            return;
        }
        state.write().pending_rekey = false;
        rekey_after_removal(state, rekey_gateway.clone());
    });

    // Everyone in our channel, as a sorted list, so this only re-runs when the
    // membership actually changes rather than on every mute and speaking flag.
    let roster = use_memo(move || {
        let s = state.read();
        let channel = s.voice.channel_id?;
        let mut present = present_in(&s, channel);
        present.sort();
        Some((channel, present))
    });

    let send = gateway.clone();
    use_effect(move || {
        let Some((channel, present)) = roster() else {
            return;
        };
        let (identity, me, held) = {
            let s = state.read();
            (
                s.identity.clone(),
                s.self_user.as_ref().map(|u| u.pubkey.clone()),
                s.media_keys.get(&channel).copied(),
            )
        };
        let (Some(identity), Some(me)) = (identity, me) else {
            return;
        };

        match held {
            // We hold the key: hand it to everyone else here.
            //
            // **Not gated on being the designated member**, and that was a bug
            // with a symptom worth remembering: the rule used to be "only the
            // lowest pubkey hands the key on", which says nothing about whether
            // the lowest pubkey *has* one. A host who generated the key while
            // alone, then had somebody join whose pubkey sorted lower, would sit
            // on it and send nothing — and the newcomer, finding itself lowest,
            // would generate a second key. Two keys, both rooms encrypting,
            // silence in both directions.
            //
            // Holding the key is the only qualification that matters. Duplicate
            // sends are harmless: a recipient that already has this epoch
            // ignores it.
            Some((epoch, key)) => {
                let others: Vec<&str> = present
                    .iter()
                    .map(String::as_str)
                    .filter(|p| *p != me)
                    .collect();
                if others.is_empty() {
                    tracing::debug!(%channel, epoch, "hold the key; nobody else here yet");
                    return;
                }
                for to in others {
                    match seal(&key, to, epoch, &identity) {
                        Ok(blob) => {
                            tracing::info!(%to, epoch, "sending the media key");
                            send.send(ClientMessage::ShareMediaKey {
                                channel_id: channel,
                                to: to.to_string(),
                                epoch,
                                blob,
                            })
                        }
                        Err(e) => {
                            tracing::warn!(%to, error = %e, "could not seal the media key")
                        }
                    }
                }
            }
            // No key yet. Alone in the channel, there is nobody to wait for.
            // With others present, someone should be sending us one — wait
            // before assuming they will not.
            None => {
                let alone = present.iter().all(|p| p == &me);
                let mut state = state;
                let send = send.clone();
                let identity = identity.clone();
                spawn(async move {
                    if !alone {
                        tokio::time::sleep(KEY_WAIT).await;
                        // Somebody obliged while we waited.
                        if state.peek().media_keys.contains_key(&channel) {
                            return;
                        }
                        // Still nobody, so generate one — *whoever we are*.
                        //
                        // This used to defer to the lowest pubkey, on the
                        // reasoning that two members generating two keys would
                        // be a disaster. It was: two epoch-1 keys that could
                        // never converge. But that is fixed at the receiving
                        // end now (`net::supersedes` breaks a tie by pubkey),
                        // and with convergence in place the exclusivity is not
                        // only unnecessary, it is harmful — it means a member
                        // who never receives a key also never makes one, and
                        // sits silent forever waiting for a client that had no
                        // reason to send.
                        //
                        // Generating too often costs a brief second key that
                        // both sides then agree to discard. Generating too
                        // rarely costs the call.
                        tracing::info!(
                            %channel,
                            "no key arrived while waiting — making one rather than waiting longer"
                        );
                    }
                    // Epoch 1: the first key a channel has had this session. A
                    // rekey counts up from whatever we were last given.
                    let key = generate();
                    tracing::info!(
                        %channel,
                        alone,
                        "generating a media key — nobody else offered one"
                    );
                    state.write().media_keys.insert(channel, (1, key));
                    crate::e2ee::apply_key(&key);
                    for to in present_in(&state.peek(), channel) {
                        if to == me {
                            continue;
                        }
                        if let Ok(blob) = seal(&key, &to, 1, &identity) {
                            send.send(ClientMessage::ShareMediaKey {
                                channel_id: channel,
                                to,
                                epoch: 1,
                                blob,
                            });
                        }
                    }
                });
            }
        }
    });

    rsx! { Fragment {} }
}

/// Move the channel to a new key, because somebody who had the old one should
/// no longer be able to use it.
///
/// Called when a member is removed from the guild. The key they hold cannot be
/// taken back — nothing can un-give bytes — so the point is that everything
/// published from now on is under a key they never had. Whatever they already
/// captured stays captured, which is the honest limit of rekeying.
pub fn rekey_after_removal(
    mut state: Signal<crate::state::AppState>,
    gateway: crate::state::GatewayTx,
) {
    let (channel, identity, me, epoch) = {
        let s = state.read();
        let Some(channel) = s.voice.channel_id else {
            return;
        };
        let Some(identity) = s.identity.clone() else {
            return;
        };
        let Some(me) = s.self_user.as_ref().map(|u| u.pubkey.clone()) else {
            return;
        };
        let Some((epoch, _)) = s.media_keys.get(&channel).copied() else {
            return;
        };
        (channel, identity, me, epoch)
    };

    // One of us, deterministically, or the channel gets as many new keys as it
    // has members and settles on whichever arrived last.
    let present = present_in(&state.peek(), channel);
    if designated(present.iter().map(String::as_str)) != Some(me.as_str()) {
        return;
    }

    let key = generate();
    let next = epoch.saturating_add(1);
    tracing::info!(%channel, epoch = next, "rekeying after a member was removed");

    // Send first, adopt second, and this order is the point. Everyone switches
    // at a slightly different moment, and during that gap frames published
    // under the new key cannot be read by anyone still on the old one. We are
    // the only publisher of new-key frames until we adopt, so adopting *last*
    // shrinks the window to roughly one network hop instead of one hop plus
    // however long our own sealing loop took.
    //
    // It is a smaller gap, not no gap. The proper fix is LiveKit's key ring —
    // publish under a new index while still accepting the old — and the JS
    // SDK's `ExternalE2EEKeyProvider.setKey` takes no index, so it is not
    // reachable without reimplementing a key provider against minified
    // internals. Recorded in TODO.md rather than half-done here.
    for to in &present {
        if to == &me {
            continue;
        }
        match seal(&key, to, next, &identity) {
            Ok(blob) => gateway.send(ClientMessage::ShareMediaKey {
                channel_id: channel,
                to: to.clone(),
                epoch: next,
                blob,
            }),
            Err(e) => tracing::warn!(%to, error = %e, "could not seal the rekey"),
        }
    }
    state.write().media_keys.insert(channel, (next, key));
    crate::e2ee::apply_key(&key);
}

#[cfg(test)]
mod orchestration_tests {
    use super::designated;

    /// Every client has to reach the same answer without talking about it —
    /// that is the entire reason the rule is "lowest pubkey" and not an
    /// election. Order of the input must not matter.
    #[test]
    fn the_designated_sender_is_the_same_from_every_side() {
        let members = ["cc", "aa", "bb"];
        assert_eq!(designated(members.iter().copied()), Some("aa"));
        let reversed: Vec<&str> = members.iter().copied().rev().collect();
        assert_eq!(designated(reversed.into_iter()), Some("aa"));
    }

    /// An empty channel has nobody responsible, which the caller has to handle
    /// rather than unwrap.
    #[test]
    fn nobody_is_responsible_for_an_empty_channel() {
        assert_eq!(designated(std::iter::empty()), None);
    }
}
