use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};

use crate::identity::Identity;

pub const KEY_LEN: usize = 32;

const NONCE_LEN: usize = 24;

pub fn generate() -> [u8; KEY_LEN] {
    use rand::RngCore;
    let mut key = [0u8; KEY_LEN];
    rand::rngs::OsRng.fill_bytes(&mut key);
    key
}

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
        aad: &aad(epoch, &identity.pubkey),
    };
    let opened = cipher
        .decrypt(XNonce::from_slice(nonce), payload)
        .map_err(|_| "this sealed key is not for us, or has been tampered with".to_string())?;

    opened
        .try_into()
        .map_err(|_| "sealed key had the wrong length".to_string())
}

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

    #[test]
    fn a_sealed_key_opens_for_its_recipient() {
        let alice = identity(1);
        let bob = identity(2);
        let key = generate();

        let blob = seal(&key, &bob.pubkey, 7, &alice).unwrap();
        assert_eq!(open(&blob, &alice.pubkey, 7, &bob).unwrap(), key);
    }

    #[test]
    fn nobody_else_can_open_it() {
        let alice = identity(1);
        let bob = identity(2);
        let eve = identity(3);
        let key = generate();

        let blob = seal(&key, &bob.pubkey, 7, &alice).unwrap();
        assert!(open(&blob, &alice.pubkey, 7, &eve).is_err());
        assert!(open(&blob, &eve.pubkey, 7, &bob).is_err());
    }

    #[test]
    fn an_epoch_cannot_be_swapped_under_the_ciphertext() {
        let alice = identity(1);
        let bob = identity(2);
        let key = generate();

        let blob = seal(&key, &bob.pubkey, 7, &alice).unwrap();
        assert!(open(&blob, &alice.pubkey, 8, &bob).is_err());
        assert!(open(&blob, &alice.pubkey, 6, &bob).is_err());
    }

    #[test]
    fn sealing_twice_does_not_repeat() {
        let alice = identity(1);
        let bob = identity(2);
        let key = generate();

        let a = seal(&key, &bob.pubkey, 1, &alice).unwrap();
        let b = seal(&key, &bob.pubkey, 1, &alice).unwrap();
        assert_ne!(a, b);
        assert_eq!(open(&a, &alice.pubkey, 1, &bob).unwrap(), key);
        assert_eq!(open(&b, &alice.pubkey, 1, &bob).unwrap(), key);
    }

    #[test]
    fn generated_keys_differ() {
        assert_ne!(generate(), generate());
        assert_ne!(generate(), [0u8; KEY_LEN]);
    }

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

use dioxus::prelude::*;

use crate::protocol::{ClientMessage, Id};
use crate::state::{use_app_state, use_gateway};

type Ledger = std::collections::HashMap<(Id, String), u32>;

static SENT: std::sync::Mutex<Option<Ledger>> = std::sync::Mutex::new(None);

fn needs_send(channel: Id, to: &str, epoch: u32) -> bool {
    let mut guard = SENT.lock().expect("media key ledger");
    let sent = guard.get_or_insert_with(Ledger::new);
    should_send(sent, channel, to, epoch)
}

fn sent(channel: Id, to: &str, epoch: u32) {
    let mut guard = SENT.lock().expect("media key ledger");
    let sent = guard.get_or_insert_with(Ledger::new);
    mark_sent(sent, channel, to, epoch);
}

fn should_send(sent: &Ledger, channel: Id, to: &str, epoch: u32) -> bool {
    !matches!(sent.get(&(channel, to.to_string())), Some(&already) if already >= epoch)
}

fn mark_sent(sent: &mut Ledger, channel: Id, to: &str, epoch: u32) {
    sent.insert((channel, to.to_string()), epoch);
}

fn forget_absent(sent: &mut Ledger, channel: Id, present: &[String]) -> usize {
    let before = sent.len();
    sent.retain(|(ch, to), _| *ch != channel || present.iter().any(|p| p == to));
    before - sent.len()
}

fn forget_absent_now(channel: Id, present: &[String]) -> usize {
    let mut guard = SENT.lock().expect("media key ledger");
    let sent = guard.get_or_insert_with(Ledger::new);
    forget_absent(sent, channel, present)
}

const KEY_WAIT: std::time::Duration = std::time::Duration::from_secs(4);

fn designated<'a>(present: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    present.min()
}

fn present_in(state: &crate::state::AppState, channel: Id) -> Vec<String> {
    state
        .voice_states
        .iter()
        .filter(|v| v.channel_id == Some(channel))
        .map(|v| v.user_pubkey.clone())
        .collect()
}

#[component]
pub fn MediaKeyBridge() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();

    let rekey_gateway = gateway.clone();
    use_effect(move || {
        if !state.read().pending_rekey {
            return;
        }
        state.write().pending_rekey = false;
        rekey_after_removal(state, rekey_gateway.clone());
    });

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

        let forgotten = forget_absent_now(channel, &present);
        if forgotten > 0 {
            tracing::debug!(%channel, forgotten, "forgot the media key ledger for members who left");
        }

        match held {
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
                            sent(channel, to, epoch);
                        }
                        Err(e) => {
                            tracing::warn!(%to, error = %e, "could not seal the media key")
                        }
                    }
                }
            }
            None => {
                let alone = present.iter().all(|p| p == &me);
                let mut state = state;
                let send = send.clone();
                let identity = identity.clone();
                spawn(async move {
                    if !alone {
                        tokio::time::sleep(KEY_WAIT).await;
                        if state.peek().media_keys.contains_key(&channel) {
                            return;
                        }
                        tracing::info!(
                            %channel,
                            "no key arrived while waiting — making one rather than waiting longer"
                        );
                    }
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

    let present = present_in(&state.peek(), channel);
    if designated(present.iter().map(String::as_str)) != Some(me.as_str()) {
        return;
    }

    let key = generate();
    let next = epoch.saturating_add(1);
    tracing::info!(%channel, epoch = next, "rekeying after a member was removed");

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
            Err(e) => tracing::warn!(%to, error = %e, "could not seal the rekey"),
        }
    }
    state.write().media_keys.insert(channel, (next, key));
    crate::e2ee::apply_key(&key, next);
}

#[cfg(test)]
mod orchestration_tests {
    use super::designated;

    #[test]
    fn the_designated_sender_is_the_same_from_every_side() {
        let members = ["cc", "aa", "bb"];
        assert_eq!(designated(members.iter().copied()), Some("aa"));
        let reversed: Vec<&str> = members.iter().copied().rev().collect();
        assert_eq!(designated(reversed.into_iter()), Some("aa"));
    }

    #[test]
    fn nobody_is_responsible_for_an_empty_channel() {
        assert_eq!(designated(std::iter::empty()), None);
    }

    #[test]
    fn a_key_is_sent_once_per_epoch_per_member() {
        use super::{needs_send, sent};
        let channel = crate::protocol::Id::new_v4();
        let other = crate::protocol::Id::new_v4();

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
        assert!(!needs_send(channel, "alice", 1));
        assert!(needs_send(other, "alice", 1));
    }

    #[test]
    fn a_member_who_left_is_sent_the_key_again_on_return() {
        use super::{Ledger, forget_absent, mark_sent, should_send};
        let channel = crate::protocol::Id::new_v4();
        let mut sent = Ledger::new();
        let present = ["alice".to_string(), "bob".to_string()];

        assert!(should_send(&sent, channel, "bob", 1));
        mark_sent(&mut sent, channel, "bob", 1);
        assert!(!should_send(&sent, channel, "bob", 1));

        assert_eq!(forget_absent(&mut sent, channel, &present), 0);
        assert!(!should_send(&sent, channel, "bob", 1));

        assert_eq!(forget_absent(&mut sent, channel, &["alice".to_string()]), 1);
        assert!(
            should_send(&sent, channel, "bob", 1),
            "a member who left and came back must be sent the key again"
        );
    }

    #[test]
    fn a_key_that_could_not_be_sealed_is_still_owed() {
        use super::{Ledger, mark_sent, should_send};
        let channel = crate::protocol::Id::new_v4();
        let mut sent = Ledger::new();

        assert!(should_send(&sent, channel, "bob", 1));
        assert!(
            should_send(&sent, channel, "bob", 1),
            "asking must not record; a failed seal has to be retried"
        );

        mark_sent(&mut sent, channel, "bob", 1);
        assert!(!should_send(&sent, channel, "bob", 1));
        assert!(should_send(&sent, channel, "bob", 2));
    }

    #[test]
    fn forgetting_is_scoped_to_the_channel_it_was_told_about() {
        use super::{Ledger, forget_absent, mark_sent, should_send};
        let channel = crate::protocol::Id::new_v4();
        let other = crate::protocol::Id::new_v4();
        let mut sent = Ledger::new();

        mark_sent(&mut sent, channel, "bob", 1);
        mark_sent(&mut sent, other, "bob", 1);

        assert_eq!(forget_absent(&mut sent, channel, &[]), 1);
        assert!(
            !should_send(&sent, other, "bob", 1),
            "the other channel's entry must survive"
        );
    }
}
