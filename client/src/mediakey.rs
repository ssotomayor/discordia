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

/// What we have already handed to whom, so the same key is not sent twice.
///
/// The effect that sends keys reads `AppState`, so it re-runs on anything that
/// touches it — a speaking flag, a mic level, a heartbeat. Without a ledger
/// that meant re-sealing and re-sending the key to every member several times a
/// second, which is not merely wasteful: the gateway's outbound queue per
/// connection is bounded, and overflowing it drops the connection. A member was
/// flooded off voice by the mechanism meant to let them hear it.
///
/// Keyed by channel and recipient, holding the epoch last sent. A rekey raises
/// the epoch and so sends again, which is the one case that must not be
/// suppressed.
///
/// **The epoch does not identify the key, and this is why entries have to be
/// forgotten.** `net::supersedes` exists precisely because two members can hold
/// two different epoch-1 keys for the same channel. So "already sent epoch 1 to
/// them" is not the same claim as "they have our key", and treating it as one
/// is silence: see `forget_absent`.
type Ledger = std::collections::HashMap<(Id, String), u32>;

static SENT: std::sync::Mutex<Option<Ledger>> = std::sync::Mutex::new(None);

/// Whether this key still needs sending to this member.
///
/// **Asking does not record anything.** The ledger is written by `mark_sent`,
/// once the key is actually on its way — see there for what recording an
/// intention instead used to cost.
fn needs_send(channel: Id, to: &str, epoch: u32) -> bool {
    let mut guard = SENT.lock().expect("media key ledger");
    let sent = guard.get_or_insert_with(Ledger::new);
    should_send(sent, channel, to, epoch)
}

/// Note that this member has it, so the sending effect stops resealing it.
fn sent(channel: Id, to: &str, epoch: u32) {
    let mut guard = SENT.lock().expect("media key ledger");
    let sent = guard.get_or_insert_with(Ledger::new);
    mark_sent(sent, channel, to, epoch);
}

/// `needs_send` over a ledger we are handed, so it can be tested without owning
/// the process-wide one.
fn should_send(sent: &Ledger, channel: Id, to: &str, epoch: u32) -> bool {
    !matches!(sent.get(&(channel, to.to_string())), Some(&already) if already >= epoch)
}

/// The write half, kept apart from the question on purpose.
///
/// These used to be one call that answered and recorded at once, which meant the
/// ledger was written *before* the key was sealed. Sealing can fail — it is an
/// ECDH against a pubkey that arrived over the wire — and when it did, the
/// entry claiming we had sent the key survived the failure to send it. That
/// member then never got it for that epoch, by the same mechanism as the stale
/// entry in `forget_absent`: a ledger that overstates what the far side has is
/// silence, and silence here has no error attached to it.
fn mark_sent(sent: &mut Ledger, channel: Id, to: &str, epoch: u32) {
    sent.insert((channel, to.to_string()), epoch);
}

/// Drop what we remember about members no longer in this channel.
///
/// A member who left and came back is a **new session**, which may have restarted
/// the app and so lost the key entirely — its own `media_keys` and its own copy
/// of this ledger both start empty. Ours does not, and without this the stale
/// entry suppresses the one send that would have fixed them:
///
/// 1. they rejoin with no key, wait `KEY_WAIT`, and we send nothing
/// 2. they generate their own epoch-1 key and send it to us
/// 3. `supersedes` breaks the tie by pubkey — if ours wins we keep ours, and the
///    ledger stops us handing it over
///
/// Two keys, and silence in both directions with nothing on screen to say why.
/// There is no recovery from the far side either: the protocol has
/// `ShareMediaKey` and no request for one. The fix has to be here, on the side
/// that sends.
///
/// Scoped to one channel because `present` describes one channel; entries for
/// other channels say nothing about who is in them. Returns how many were
/// dropped, for the caller to log.
fn forget_absent(sent: &mut Ledger, channel: Id, present: &[String]) -> usize {
    let before = sent.len();
    sent.retain(|(ch, to), _| *ch != channel || present.iter().any(|p| p == to));
    before - sent.len()
}

/// `forget_absent` against the process-wide ledger.
fn forget_absent_now(channel: Id, present: &[String]) -> usize {
    let mut guard = SENT.lock().expect("media key ledger");
    let sent = guard.get_or_insert_with(Ledger::new);
    forget_absent(sent, channel, present)
}

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

        // Before deciding what to send, forget whoever is no longer here. Ahead
        // of the `match` because it applies to both arms — and because the arm
        // that holds a key returns early when nobody else is present, which is
        // exactly the moment the last departure has to be recorded.
        let forgotten = forget_absent_now(channel, &present);
        if forgotten > 0 {
            tracing::debug!(%channel, forgotten, "forgot the media key ledger for members who left");
        }

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
                    if !needs_send(channel, to, epoch) {
                        continue;
                    }
                    match seal(&key, to, epoch, &identity) {
                        Ok(blob) => {
                            tracing::info!(%to, epoch, "sending the media key");
                            send.send(ClientMessage::ShareMediaKey {
                                channel_id: channel,
                                to: to.to_string(),
                                epoch,
                                blob,
                            });
                            // Only now. A seal that failed must leave this
                            // member eligible, or the failure is permanent.
                            sent(channel, to, epoch);
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
                    crate::e2ee::apply_key(&key, 1);
                    for to in present_in(&state.peek(), channel) {
                        if to == me || !needs_send(channel, &to, 1) {
                            continue;
                        }
                        if let Ok(blob) = seal(&key, &to, 1, &identity) {
                            send.send(ClientMessage::ShareMediaKey {
                                channel_id: channel,
                                to: to.clone(),
                                epoch: 1,
                                blob,
                            });
                            sent(channel, &to, 1);
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
        if to == &me || !needs_send(channel, to, next) {
            continue;
        }
        match seal(&key, to, next, &identity) {
            Ok(blob) => {
                gateway.send(ClientMessage::ShareMediaKey {
                    channel_id: channel,
                    to: to.clone(),
                    epoch: next,
                    blob,
                });
                sent(channel, to, next);
            }
            // A rekey that cannot be sealed for one member is the worst place
            // to record it as delivered: the point of the new epoch is that
            // the removed member's key stops working, and this member would be
            // left on the old one with no second attempt.
            Err(e) => tracing::warn!(%to, error = %e, "could not seal the rekey"),
        }
    }
    state.write().media_keys.insert(channel, (next, key));
    crate::e2ee::apply_key(&key, next);
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

    /// The ledger that stopped a member being flooded off voice.
    ///
    /// The sending effect re-runs on any change to `AppState` — a speaking
    /// flag is enough — so "send the key to everyone present" ran several
    /// times a second. What must survive that is the rekey: a raised epoch has
    /// to go out even though the same recipient was written to a moment ago.
    #[test]
    fn a_key_is_sent_once_per_epoch_per_member() {
        use super::{needs_send, sent};
        let channel = crate::protocol::Id::new_v4();
        let other = crate::protocol::Id::new_v4();

        // Through the process-wide ledger, so the pair that the sending effect
        // actually calls is exercised and not only the functions underneath.
        // Safe to share it: every channel here is a fresh `Id`.
        assert!(needs_send(channel, "alice", 1), "first send must go");
        sent(channel, "alice", 1);
        assert!(
            !needs_send(channel, "alice", 1),
            "the same epoch must not repeat"
        );
        assert!(
            needs_send(channel, "bob", 1),
            "a different member still needs it"
        );
        assert!(
            needs_send(channel, "alice", 2),
            "a rekey must always go out"
        );
        sent(channel, "alice", 2);
        assert!(!needs_send(channel, "alice", 2));
        // An older epoch arriving late must not re-open the gate.
        assert!(!needs_send(channel, "alice", 1));
        // Channels are tracked apart, or joining a second one would go silent.
        assert!(needs_send(other, "alice", 1));
    }

    /// The ledger entry that outlived the member it described.
    ///
    /// Restarting the app empties that member's own key and its own ledger; ours
    /// keeps neither of those facts. Left alone, "already sent them epoch 1"
    /// suppresses the send that would have handed the key back, both sides end
    /// up on two different epoch-1 keys, and `net::supersedes` resolves the tie
    /// against whichever of them nobody is willing to hand over. Silence, both
    /// ways, permanently — the protocol has no request to recover with.
    ///
    /// Tested on the ledger directly rather than through `SENT`, which is
    /// process-wide and no test can own.
    #[test]
    fn a_member_who_left_is_sent_the_key_again_on_return() {
        use super::{Ledger, forget_absent, mark_sent, should_send};
        let channel = crate::protocol::Id::new_v4();
        let mut sent = Ledger::new();
        let present = ["alice".to_string(), "bob".to_string()];

        assert!(should_send(&sent, channel, "bob", 1));
        mark_sent(&mut sent, channel, "bob", 1);
        assert!(!should_send(&sent, channel, "bob", 1));

        // Everyone still here: nothing is forgotten, and nothing repeats.
        assert_eq!(forget_absent(&mut sent, channel, &present), 0);
        assert!(!should_send(&sent, channel, "bob", 1));

        // Bob leaves.
        assert_eq!(forget_absent(&mut sent, channel, &["alice".to_string()]), 1);
        assert!(
            should_send(&sent, channel, "bob", 1),
            "a member who left and came back must be sent the key again"
        );
    }

    /// Sealing is an ECDH against a pubkey that arrived over the wire, so it can
    /// fail. When it does, nothing was sent — and the ledger must not claim
    /// otherwise, or that member is stranded on this epoch with no second
    /// attempt and no error anywhere the user can see.
    ///
    /// This is why asking and recording are two calls. As one, the question
    /// wrote the answer before the send it was asking about had happened.
    #[test]
    fn a_key_that_could_not_be_sealed_is_still_owed() {
        use super::{Ledger, mark_sent, should_send};
        let channel = crate::protocol::Id::new_v4();
        let mut sent = Ledger::new();

        // Asked, then the seal failed: nothing is recorded, so we owe it still.
        assert!(should_send(&sent, channel, "bob", 1));
        assert!(
            should_send(&sent, channel, "bob", 1),
            "asking must not record; a failed seal has to be retried"
        );

        // Asked, sealed, sent: now it is owed to nobody.
        mark_sent(&mut sent, channel, "bob", 1);
        assert!(!should_send(&sent, channel, "bob", 1));
        // And a rekey still overrides, which is the case that must never be
        // suppressed.
        assert!(should_send(&sent, channel, "bob", 2));
    }

    /// `present` describes one channel, so it may only be used to judge that
    /// channel. Forgetting another one's members would re-send the key to
    /// everybody there on the next roster change.
    #[test]
    fn forgetting_is_scoped_to_the_channel_it_was_told_about() {
        use super::{Ledger, forget_absent, mark_sent, should_send};
        let channel = crate::protocol::Id::new_v4();
        let other = crate::protocol::Id::new_v4();
        let mut sent = Ledger::new();

        mark_sent(&mut sent, channel, "bob", 1);
        mark_sent(&mut sent, other, "bob", 1);

        // Bob is absent from `channel`; that says nothing about `other`.
        assert_eq!(forget_absent(&mut sent, channel, &[]), 1);
        assert!(
            !should_send(&sent, other, "bob", 1),
            "the other channel's entry must survive"
        );
    }
}
