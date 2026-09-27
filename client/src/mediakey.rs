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

/// A generation and the key itself. The key is part of the identity: two
/// members can both be at generation 1 with different keys.
pub type Held = (u32, [u8; KEY_LEN]);

/// Every app ranks two keys the same way, whoever passed them on: the later
/// generation, then the smaller key. Ranking by the sender's pubkey, as before,
/// let a forwarded key win on one side and lose on another, and the split stayed.
pub fn beats(a: Held, b: Held) -> bool {
    a.0 > b.0 || (a.0 == b.0 && a.1 < b.1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Adopt,
    Keep,
    /// Theirs loses to ours: send ours back, since our ledger may say they have it.
    Answer,
}

pub fn judge(held: Option<Held>, offered: Held) -> Verdict {
    match held {
        None => Verdict::Adopt,
        Some(h) if h == offered => Verdict::Keep,
        Some(h) if beats(offered, h) => Verdict::Adopt,
        Some(_) => Verdict::Answer,
    }
}

/// What each member was last sent, by key rather than generation: a new key
/// at the same generation is still owed.
type Ledger = std::collections::HashMap<(Id, String), Held>;

static SENT: std::sync::Mutex<Option<Ledger>> = std::sync::Mutex::new(None);

fn needs_send(channel: Id, to: &str, key: Held) -> bool {
    let mut guard = SENT.lock().expect("media key ledger");
    let sent = guard.get_or_insert_with(Ledger::new);
    should_send(sent, channel, to, key)
}

pub(crate) fn note_sent(channel: Id, to: &str, key: Held) {
    let mut guard = SENT.lock().expect("media key ledger");
    let sent = guard.get_or_insert_with(Ledger::new);
    mark_sent(sent, channel, to, key);
}

fn should_send(sent: &Ledger, channel: Id, to: &str, key: Held) -> bool {
    sent.get(&(channel, to.to_string())) != Some(&key)
}

fn mark_sent(sent: &mut Ledger, channel: Id, to: &str, key: Held) {
    sent.insert((channel, to.to_string()), key);
}

fn forget_absent(sent: &mut Ledger, channel: Id, present: &[String]) -> usize {
    let before = sent.len();
    sent.retain(|(ch, to), _| *ch != channel || present.iter().any(|p| p == to));
    before - sent.len()
}

/// What we sent in a channel we left says nothing about who holds which key
/// when we come back, and a stale entry would stop us sending a new one.
fn forget_other_channels(sent: &mut Ledger, channel: Id) -> usize {
    let before = sent.len();
    sent.retain(|(ch, _), _| *ch == channel);
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

        let left_behind = {
            let mut guard = SENT.lock().expect("media key ledger");
            forget_other_channels(guard.get_or_insert_with(Ledger::new), channel)
        };
        if left_behind > 0 {
            tracing::debug!(%channel, left_behind, "forgot the media key ledger of channels we left");
        }
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
                    if !needs_send(channel, to, (epoch, key)) {
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
                            note_sent(channel, to, (epoch, key));
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
                        if to == me || !needs_send(channel, &to, (1, key)) {
                            continue;
                        }
                        if let Ok(blob) = seal(&key, &to, 1, &identity) {
                            send.send(ClientMessage::ShareMediaKey {
                                channel_id: channel,
                                to: to.clone(),
                                epoch: 1,
                                blob,
                            });
                            note_sent(channel, &to, (1, key));
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
        if to == &me || !needs_send(channel, to, (next, key)) {
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
                note_sent(channel, to, (next, key));
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

    const K1: [u8; super::KEY_LEN] = [1; super::KEY_LEN];
    const K2: [u8; super::KEY_LEN] = [2; super::KEY_LEN];

    #[test]
    fn a_key_is_sent_once_per_member_and_a_new_one_always() {
        use super::{Ledger, mark_sent, should_send};
        let channel = crate::protocol::Id::new_v4();
        let other = crate::protocol::Id::new_v4();
        let mut sent = Ledger::new();

        assert!(
            should_send(&sent, channel, "alice", (1, K1)),
            "first send must go"
        );
        mark_sent(&mut sent, channel, "alice", (1, K1));
        assert!(
            !should_send(&sent, channel, "alice", (1, K1)),
            "the same key does not repeat"
        );
        assert!(
            should_send(&sent, channel, "bob", (1, K1)),
            "a different member still needs it"
        );
        assert!(
            should_send(&sent, channel, "alice", (1, K2)),
            "another key at the same generation is still owed"
        );
        assert!(
            should_send(&sent, channel, "alice", (2, K1)),
            "a rekey always goes out"
        );
        assert!(should_send(&sent, other, "alice", (1, K1)));
    }

    #[test]
    fn a_member_who_left_is_sent_the_key_again_on_return() {
        use super::{Ledger, forget_absent, mark_sent, should_send};
        let channel = crate::protocol::Id::new_v4();
        let mut sent = Ledger::new();
        let present = ["alice".to_string(), "bob".to_string()];

        mark_sent(&mut sent, channel, "bob", (1, K1));
        assert_eq!(forget_absent(&mut sent, channel, &present), 0);
        assert!(!should_send(&sent, channel, "bob", (1, K1)));
        assert_eq!(forget_absent(&mut sent, channel, &["alice".to_string()]), 1);
        assert!(
            should_send(&sent, channel, "bob", (1, K1)),
            "a member who left and came back must be sent the key again"
        );
    }

    #[test]
    fn a_key_that_could_not_be_sealed_is_still_owed() {
        use super::{Ledger, mark_sent, should_send};
        let channel = crate::protocol::Id::new_v4();
        let mut sent = Ledger::new();
        assert!(should_send(&sent, channel, "bob", (1, K1)));
        assert!(
            should_send(&sent, channel, "bob", (1, K1)),
            "asking must not record; a failed seal has to be retried"
        );
        mark_sent(&mut sent, channel, "bob", (1, K1));
        assert!(!should_send(&sent, channel, "bob", (1, K1)));
    }

    #[test]
    fn the_ranking_is_the_same_whichever_side_judges() {
        use super::{Verdict, beats, judge};
        let keys = [(1, K1), (1, K2), (2, K2), (2, K1), (3, [0; super::KEY_LEN])];
        for a in keys {
            for b in keys {
                if a == b {
                    assert_eq!(judge(Some(a), b), Verdict::Keep);
                    continue;
                }
                assert!(beats(a, b) != beats(b, a), "exactly one of two keys wins");
                let (win, lose) = if beats(a, b) { (a, b) } else { (b, a) };
                assert_eq!(judge(Some(lose), win), Verdict::Adopt);
                assert_eq!(judge(Some(win), lose), Verdict::Answer);
            }
        }
        assert_eq!(judge(None, (1, K2)), Verdict::Adopt);
    }

    #[test]
    fn forgetting_is_scoped_to_the_channel_it_was_told_about() {
        use super::{Ledger, forget_absent, mark_sent, should_send};
        let channel = crate::protocol::Id::new_v4();
        let other = crate::protocol::Id::new_v4();
        let mut sent = Ledger::new();

        mark_sent(&mut sent, channel, "bob", (1, K1));
        mark_sent(&mut sent, other, "bob", (1, K1));

        assert_eq!(forget_absent(&mut sent, channel, &[]), 1);
        assert!(
            !should_send(&sent, other, "bob", (1, K1)),
            "the other channel's entry must survive"
        );
    }

    #[test]
    fn coming_back_to_a_channel_owes_its_members_the_key_again() {
        use super::{Ledger, forget_other_channels, mark_sent, should_send};
        let first = crate::protocol::Id::new_v4();
        let second = crate::protocol::Id::new_v4();
        let mut sent = Ledger::new();

        mark_sent(&mut sent, first, "bob", (1, K1));
        assert_eq!(forget_other_channels(&mut sent, second), 1);
        assert!(
            should_send(&sent, first, "bob", (1, K1)),
            "a key sent before we left is sent again after we return"
        );
    }
}

/// A call with several members, driven by the same decisions the app makes:
/// what the bridge sends (`should_send`) and what the gateway arm does with a key
/// (`judge`). Whatever the order, every member must end on one key.
#[cfg(test)]
mod convergence {
    use super::{Held, KEY_LEN, Ledger, Verdict, forget_absent, judge, mark_sent, should_send};
    use crate::protocol::Id;
    use std::collections::VecDeque;

    struct Member {
        pk: String,
        held: Option<Held>,
        sent: Ledger,
    }

    struct Call {
        members: Vec<Member>,
        present: Vec<usize>,
        wire: VecDeque<(usize, usize, Held)>,
        channel: Id,
        seed: u8,
        wipe_next_to: Option<usize>,
    }

    impl Call {
        fn new(n: usize, seed: u8) -> Self {
            Call {
                members: (0..n)
                    .map(|i| Member {
                        pk: format!("{:02}", (i * 7 + seed as usize) % 10),
                        held: None,
                        sent: Ledger::new(),
                    })
                    .collect(),
                present: Vec::new(),
                wire: VecDeque::new(),
                channel: Id::new_v4(),
                seed,
                wipe_next_to: None,
            }
        }

        fn fresh_key(&mut self) -> [u8; KEY_LEN] {
            self.seed = self.seed.wrapping_mul(73).wrapping_add(41);
            [self.seed; KEY_LEN]
        }

        fn pk(&self, i: usize) -> String {
            self.members[i].pk.clone()
        }

        /// The bridge effect of every member present.
        fn send_round(&mut self) {
            for i in self.present.clone() {
                let Some(held) = self.members[i].held else {
                    continue;
                };
                for j in self.present.clone() {
                    let to = self.pk(j);
                    if i != j && should_send(&self.members[i].sent, self.channel, &to, held) {
                        mark_sent(&mut self.members[i].sent, self.channel, &to, held);
                        self.wire.push_back((i, j, held));
                    }
                }
            }
        }

        /// The gateway arm, behind a server that relays only within the call.
        fn deliver(&mut self) {
            while let Some((from, to, offered)) = self.wire.pop_front() {
                if !self.present.contains(&from) || !self.present.contains(&to) {
                    continue;
                }
                if self.wipe_next_to == Some(to) {
                    self.wipe_next_to = None;
                    continue;
                }
                match judge(self.members[to].held, offered) {
                    Verdict::Adopt => self.members[to].held = Some(offered),
                    Verdict::Keep => {}
                    Verdict::Answer => {
                        let ours = self.members[to].held.expect("answering needs a key");
                        let back = self.pk(from);
                        mark_sent(&mut self.members[to].sent, self.channel, &back, ours);
                        self.wire.push_back((to, from, ours));
                    }
                }
            }
        }

        /// Until nothing moves; whoever is still keyless then makes one, as
        /// the bridge does once `KEY_WAIT` runs out.
        fn settle(&mut self) {
            for _ in 0..64 {
                self.send_round();
                if !self.wire.is_empty() {
                    self.deliver();
                    continue;
                }
                let keyless: Vec<usize> = self
                    .present
                    .iter()
                    .copied()
                    .filter(|&i| self.members[i].held.is_none())
                    .collect();
                if keyless.is_empty() {
                    return;
                }
                for i in keyless {
                    let key = self.fresh_key();
                    self.members[i].held = Some((1, key));
                }
            }
            panic!("the call never settled");
        }

        fn join(&mut self, i: usize) {
            self.present.push(i);
        }

        fn leave(&mut self, i: usize) {
            self.present.retain(|&p| p != i);
            self.members[i].held = None;
            let present: Vec<String> = self.present.iter().map(|&p| self.pk(p)).collect();
            for m in &mut self.members {
                forget_absent(&mut m.sent, self.channel, &present);
            }
        }

        fn agreed(&self) -> bool {
            let first = self.members[self.present[0]].held;
            first.is_some() && self.present.iter().all(|&i| self.members[i].held == first)
        }
    }

    fn orders() -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        for a in 0..3 {
            for b in 0..3 {
                for c in 0..3 {
                    if a != b && b != c && a != c {
                        out.push(vec![a, b, c]);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn one_after_another_in_every_order() {
        for seed in 0..16 {
            for order in orders() {
                let mut call = Call::new(3, seed);
                for i in order {
                    call.join(i);
                    call.settle();
                    assert!(call.agreed(), "seed {seed}: split after a join");
                }
            }
        }
    }

    #[test]
    fn everyone_at_once() {
        for seed in 0..16 {
            let mut call = Call::new(3, seed);
            for i in 0..3 {
                call.join(i);
            }
            call.settle();
            assert!(call.agreed(), "seed {seed}");
        }
    }

    /// The move race: the key a newcomer was sent is wiped on arrival, the
    /// sender's ledger says it went, and the newcomer makes its own.
    #[test]
    fn a_key_wiped_on_arrival_still_ends_in_one_key() {
        for seed in 0..16 {
            for order in orders() {
                let mut call = Call::new(3, seed);
                call.join(order[0]);
                call.settle();
                call.join(order[1]);
                call.wipe_next_to = Some(order[1]);
                call.settle();
                call.join(order[2]);
                call.wipe_next_to = Some(order[2]);
                call.settle();
                assert!(call.agreed(), "seed {seed}, order {order:?}");
            }
        }
    }

    /// Two members made keys alone at the same time, then a third joins and
    /// forwards one of them: the old sender-ranked tiebreak split this.
    #[test]
    fn two_keys_made_at_once_and_a_forwarder() {
        for seed in 0..16 {
            let mut call = Call::new(3, seed);
            call.join(0);
            call.settle();
            let rival = call.fresh_key();
            call.members[1].held = Some((1, rival));
            call.join(1);
            call.join(2);
            call.settle();
            assert!(call.agreed(), "seed {seed}");
        }
    }

    #[test]
    fn leaving_and_coming_back_and_a_rekey() {
        for seed in 0..16 {
            let mut call = Call::new(3, seed);
            for i in 0..3 {
                call.join(i);
                call.settle();
            }
            call.leave(1);
            call.settle();
            call.join(1);
            call.wipe_next_to = Some(1);
            call.settle();
            assert!(call.agreed(), "seed {seed}: after a return");
            let next = call.members[0].held.unwrap().0 + 1;
            let key = call.fresh_key();
            call.members[0].held = Some((next, key));
            call.settle();
            assert!(call.agreed(), "seed {seed}: after a rekey");
            assert_eq!(call.members[2].held.unwrap().0, next);
        }
    }
}
