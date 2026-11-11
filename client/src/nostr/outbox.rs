//! `step` is the only place an outgoing DM changes stage; the UI's marks and the saved
//! outbox are both read from `Outbox`, so they cannot disagree.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use super::delivery::Delivery;
use super::event::Event;
use crate::protocol::Id;

/// Most messages that may wait at once, as `Outbox::pending` counts them.
pub const CAP: usize = 128;
const PARALLEL: usize = 8;
const SYSTEM_RETRIES: u8 = 5;
const RETRY_DELAY: Duration = Duration::from_secs(2);

/// Whether `adding` more fit beside `pending`; the composer asks before it holds a message.
pub fn has_room(pending: usize, adding: usize) -> bool {
    pending + adding <= CAP
}

/// What the person wrote, before it can be wrapped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub peer: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub emoji: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wraps {
    pub peer: String,
    pub theirs: Event,
    pub ours: Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Written by the person: drawn with its mark, sent again only when they ask.
    Chat,
    /// An edit, deletion or call record: never drawn, retried on its own.
    System { attempts: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Track {
    /// Drawn with the clock while an upload runs: the composer's emoji, or an encrypted image.
    Held,
    /// Its upload or its wrapping failed; Retry holds it again.
    Refused,
    Queued {
        kind: Kind,
        due: Instant,
    },
    InFlight {
        kind: Kind,
    },
    /// No relay took it.
    Failed {
        kind: Kind,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Dispatch,
    Answer {
        accepted: bool,
    },
    Refuse,
    Retry,
    /// A relay came back: a system delivery that ran out of patience starts over.
    Reconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    /// Send the wraps: theirs to the recipient's relays, ours to our own.
    Publish,
    /// Gone from the outbox; the saved copy must follow.
    Accepted,
    /// Run the upload or the send that was refused once more.
    Resubmit,
}

/// The transition function. Anything not listed leaves the message where it is.
pub fn step(track: Track, input: Input, now: Instant) -> (Option<Track>, Effect) {
    use {Effect as E, Input as I, Track as T};
    match (track, input) {
        (T::Held, I::Refuse) => (Some(T::Refused), E::None),
        (T::Refused, I::Retry) => (Some(T::Held), E::Resubmit),
        (T::Queued { kind, due }, I::Dispatch) if due <= now => {
            (Some(T::InFlight { kind }), E::Publish)
        }
        (T::InFlight { .. }, I::Answer { accepted: true }) => (None, E::Accepted),
        (
            T::InFlight {
                kind: Kind::System { attempts },
            },
            I::Answer { accepted: false },
        ) if attempts < SYSTEM_RETRIES => {
            let attempts = attempts + 1;
            let due = now + Duration::from_secs(15 << attempts);
            let kind = Kind::System { attempts };
            (Some(T::Queued { kind, due }), E::None)
        }
        (T::InFlight { kind }, I::Answer { accepted: false }) => {
            (Some(T::Failed { kind }), E::None)
        }
        (T::Failed { kind }, I::Retry) => (
            Some(T::Queued {
                kind,
                due: now + RETRY_DELAY,
            }),
            E::None,
        ),
        (
            T::Failed {
                kind: Kind::System { .. },
            }
            | T::Queued {
                kind: Kind::System { attempts: 1.. },
                ..
            },
            I::Reconnected,
        ) => (
            Some(T::Queued {
                kind: Kind::System { attempts: 0 },
                due: now + RETRY_DELAY,
            }),
            E::None,
        ),
        (track, _) => (Some(track), E::None),
    }
}

/// The mark a message's row shows.
fn mark(track: Track) -> Option<Delivery> {
    match track {
        Track::Queued {
            kind: Kind::System { .. },
            ..
        }
        | Track::InFlight {
            kind: Kind::System { .. },
        }
        | Track::Failed {
            kind: Kind::System { .. },
        } => None,
        Track::Held | Track::Queued { .. } | Track::InFlight { .. } => Some(Delivery::Pending),
        Track::Refused | Track::Failed { .. } => Some(Delivery::Failed),
    }
}

// Which body goes with which track is fixed at insertion: `step` never moves a
// held message to a wrapped stage or back.
enum Body {
    /// `key` is the held row's own id, which `NostrCmd::Send` names in `replaces`.
    Draft {
        key: String,
        draft: Draft,
    },
    Upload {
        peer: String,
    },
    Wrapped {
        wraps: Wraps,
        waits: bool,
    },
}

struct Slot {
    body: Body,
    track: Track,
}

/// What `Outbox::apply` did, for the service to carry out.
#[derive(Debug, PartialEq, Eq)]
pub struct Change {
    pub effect: Effect,
    pub mark: Option<Delivery>,
}

#[derive(Default)]
pub struct Outbox {
    slots: HashMap<Id, Slot>,
}

impl Outbox {
    pub fn hold(&mut self, id: Id, key: String, draft: Draft) {
        self.slots.insert(
            id,
            Slot {
                body: Body::Draft { key, draft },
                track: Track::Held,
            },
        );
    }

    /// An encrypted image's row while it uploads; its wraps get ids of their own.
    pub fn hold_upload(&mut self, id: Id, peer: String) {
        self.slots.insert(
            id,
            Slot {
                body: Body::Upload { peer },
                track: Track::Held,
            },
        );
    }

    /// `waits`: held back while an image to the same peer uploads, so it cannot overtake it.
    /// False when the id is already here, as recovery may meet wraps it saved.
    pub fn queue(&mut self, id: Id, wraps: Wraps, kind: Kind, due: Instant, waits: bool) -> bool {
        if self.slots.contains_key(&id) {
            return false;
        }
        self.slots.insert(
            id,
            Slot {
                body: Body::Wrapped { wraps, waits },
                track: Track::Queued { kind, due },
            },
        );
        true
    }

    /// A saved message at launch: the person's wait for their Retry, system ones go at once.
    pub fn recover(&mut self, id: Id, wraps: Wraps, system: bool, waits: bool, now: Instant) {
        let track = if system {
            Track::Queued {
                kind: Kind::System { attempts: 0 },
                due: now,
            }
        } else {
            Track::Failed { kind: Kind::Chat }
        };
        self.slots.insert(
            id,
            Slot {
                body: Body::Wrapped { wraps, waits },
                track,
            },
        );
    }

    pub fn withdraw(&mut self, id: Id) -> bool {
        self.slots.remove(&id).is_some()
    }

    /// None when the message is not here: an answer can outlive its deletion.
    pub fn apply(&mut self, id: Id, input: Input, now: Instant) -> Option<Change> {
        let slot = self.slots.get_mut(&id)?;
        let chat = matches!(
            slot.track,
            Track::Held
                | Track::Refused
                | Track::Queued {
                    kind: Kind::Chat,
                    ..
                }
                | Track::InFlight { kind: Kind::Chat }
                | Track::Failed { kind: Kind::Chat }
        );
        let (next, effect) = step(slot.track, input, now);
        let mark = match next {
            Some(track) => {
                slot.track = track;
                mark(track)
            }
            None => {
                self.slots.remove(&id);
                chat.then_some(Delivery::Accepted)
            }
        };
        Some(Change { effect, mark })
    }

    pub fn reconnected(&mut self, now: Instant) {
        for slot in self.slots.values_mut() {
            slot.track = step(slot.track, Input::Reconnected, now)
                .0
                .unwrap_or(slot.track);
        }
    }

    /// Moves what is due to `InFlight`, at most `PARALLEL` in flight, and returns what to publish.
    pub fn dispatch(&mut self, now: Instant) -> Vec<(Id, Wraps)> {
        let in_flight = self
            .slots
            .values()
            .filter(|slot| matches!(slot.track, Track::InFlight { .. }))
            .count();
        let uploading: HashSet<&str> = self
            .slots
            .values()
            .filter_map(|slot| match &slot.body {
                Body::Upload { peer } => Some(peer.as_str()),
                _ => None,
            })
            .collect();
        let mut ready: Vec<(Instant, Id)> = self
            .slots
            .iter()
            .filter_map(|(id, slot)| match (&slot.body, slot.track) {
                (Body::Wrapped { wraps, waits }, Track::Queued { due, .. })
                    if due <= now && !(*waits && uploading.contains(wraps.peer.as_str())) =>
                {
                    Some((due, *id))
                }
                _ => None,
            })
            .collect();
        ready.sort();
        ready.truncate(PARALLEL.saturating_sub(in_flight));
        let mut out = Vec::with_capacity(ready.len());
        for (_, id) in ready {
            if self
                .apply(id, Input::Dispatch, now)
                .map(|change| change.effect)
                == Some(Effect::Publish)
                && let Some(Body::Wrapped { wraps, .. }) = self.slots.get(&id).map(|s| &s.body)
            {
                out.push((id, wraps.clone()));
            }
        }
        out
    }

    pub fn shown(&self, id: Id) -> Option<Delivery> {
        self.slots.get(&id).and_then(|slot| mark(slot.track))
    }

    #[cfg(test)]
    pub fn track(&self, id: Id) -> Option<Track> {
        self.slots.get(&id).map(|slot| slot.track)
    }

    /// A message not yet wrapped, with its row's key: waiting on its emoji, or refused.
    pub fn held(&self, id: Id) -> Option<(&str, &Draft)> {
        match &self.slots.get(&id)?.body {
            Body::Draft { key, draft } => Some((key, draft)),
            _ => None,
        }
    }

    /// The one count every cap checks: an upload holds room for its picture and its caption.
    pub fn pending(&self) -> usize {
        self.slots
            .values()
            .map(|slot| match slot.body {
                Body::Upload { .. } => 2,
                _ => 1,
            })
            .sum()
    }

    /// Whether `adding` fit once `freeing` of what is counted now has left.
    pub fn fits(&self, adding: usize, freeing: usize) -> bool {
        has_room(self.pending().saturating_sub(freeing), adding)
    }

    pub fn queued(&self) -> bool {
        self.slots
            .values()
            .any(|slot| matches!(slot.track, Track::Queued { .. }))
    }

    /// What `delivery::persist` saves: everything already wrapped.
    pub fn wrapped(&self) -> impl Iterator<Item = &Wraps> {
        self.slots.values().filter_map(|slot| match &slot.body {
            Body::Wrapped { wraps, .. } => Some(wraps),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT: Kind = Kind::Chat;
    const fn system(attempts: u8) -> Kind {
        Kind::System { attempts }
    }

    #[test]
    fn every_transition() {
        use {Effect as E, Input as I, Track as T};
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        let retry = now + RETRY_DELAY;
        let table: Vec<(T, I, Option<T>, E)> = vec![
            (T::Held, I::Refuse, Some(T::Refused), E::None),
            (T::Held, I::Retry, Some(T::Held), E::None),
            (T::Held, I::Dispatch, Some(T::Held), E::None),
            (T::Refused, I::Retry, Some(T::Held), E::Resubmit),
            (T::Refused, I::Reconnected, Some(T::Refused), E::None),
            (
                T::Queued {
                    kind: CHAT,
                    due: now,
                },
                I::Dispatch,
                Some(T::InFlight { kind: CHAT }),
                E::Publish,
            ),
            (
                T::Queued {
                    kind: CHAT,
                    due: later,
                },
                I::Dispatch,
                Some(T::Queued {
                    kind: CHAT,
                    due: later,
                }),
                E::None,
            ),
            (
                T::InFlight { kind: CHAT },
                I::Answer { accepted: true },
                None,
                E::Accepted,
            ),
            (
                T::InFlight { kind: system(3) },
                I::Answer { accepted: true },
                None,
                E::Accepted,
            ),
            (
                T::InFlight { kind: CHAT },
                I::Answer { accepted: false },
                Some(T::Failed { kind: CHAT }),
                E::None,
            ),
            (
                T::InFlight { kind: system(0) },
                I::Answer { accepted: false },
                Some(T::Queued {
                    kind: system(1),
                    due: now + Duration::from_secs(30),
                }),
                E::None,
            ),
            (
                T::InFlight { kind: system(4) },
                I::Answer { accepted: false },
                Some(T::Queued {
                    kind: system(5),
                    due: now + Duration::from_secs(480),
                }),
                E::None,
            ),
            (
                T::InFlight { kind: system(5) },
                I::Answer { accepted: false },
                Some(T::Failed { kind: system(5) }),
                E::None,
            ),
            (
                T::InFlight { kind: CHAT },
                I::Retry,
                Some(T::InFlight { kind: CHAT }),
                E::None,
            ),
            (
                T::Failed { kind: CHAT },
                I::Retry,
                Some(T::Queued {
                    kind: CHAT,
                    due: retry,
                }),
                E::None,
            ),
            (
                T::Failed { kind: CHAT },
                I::Reconnected,
                Some(T::Failed { kind: CHAT }),
                E::None,
            ),
            (
                T::Failed { kind: system(5) },
                I::Reconnected,
                Some(T::Queued {
                    kind: system(0),
                    due: retry,
                }),
                E::None,
            ),
            (
                T::Queued {
                    kind: system(2),
                    due: later + Duration::from_secs(60),
                },
                I::Reconnected,
                Some(T::Queued {
                    kind: system(0),
                    due: retry,
                }),
                E::None,
            ),
            (
                T::Queued {
                    kind: system(0),
                    due: now,
                },
                I::Reconnected,
                Some(T::Queued {
                    kind: system(0),
                    due: now,
                }),
                E::None,
            ),
            (
                T::Failed { kind: CHAT },
                I::Answer { accepted: true },
                Some(T::Failed { kind: CHAT }),
                E::None,
            ),
        ];
        for (from, input, to, effect) in table {
            assert_eq!(step(from, input, now), (to, effect), "{from:?} + {input:?}");
        }
    }

    #[test]
    fn marks_follow_the_track() {
        let now = Instant::now();
        for (track, shown) in [
            (Track::Held, Some(Delivery::Pending)),
            (Track::Refused, Some(Delivery::Failed)),
            (
                Track::Queued {
                    kind: CHAT,
                    due: now,
                },
                Some(Delivery::Pending),
            ),
            (Track::InFlight { kind: CHAT }, Some(Delivery::Pending)),
            (Track::Failed { kind: CHAT }, Some(Delivery::Failed)),
            (
                Track::Queued {
                    kind: system(1),
                    due: now,
                },
                None,
            ),
            (Track::InFlight { kind: system(0) }, None),
            (Track::Failed { kind: system(5) }, None),
        ] {
            assert_eq!(mark(track), shown, "{track:?}");
        }
    }

    fn wraps(peer: &str, n: u64) -> Wraps {
        let secret = secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
        let event = super::super::event::sign_with(&secret, n as i64, 1059, vec![], "x".into());
        Wraps {
            peer: peer.into(),
            theirs: event.clone(),
            ours: event,
        }
    }

    fn id(n: u128) -> Id {
        Id::from_u128(n)
    }

    #[test]
    fn an_accepted_message_leaves_and_only_a_chat_keeps_its_mark() {
        let now = Instant::now();
        let mut outbox = Outbox::default();
        outbox.queue(id(1), wraps("p", 1), CHAT, now, true);
        outbox.queue(id(2), wraps("p", 2), system(0), now, true);
        assert_eq!(outbox.dispatch(now).len(), 2);
        let accepted = Input::Answer { accepted: true };
        assert_eq!(
            outbox.apply(id(1), accepted, now),
            Some(Change {
                effect: Effect::Accepted,
                mark: Some(Delivery::Accepted)
            })
        );
        assert_eq!(
            outbox.apply(id(2), accepted, now),
            Some(Change {
                effect: Effect::Accepted,
                mark: None
            })
        );
        assert_eq!(outbox.pending(), 0);
        assert_eq!(outbox.wrapped().count(), 0);
        assert_eq!(outbox.apply(id(1), accepted, now), None);
    }

    #[test]
    fn dispatch_keeps_eight_in_flight_and_waits_behind_an_upload_to_the_same_peer() {
        let now = Instant::now();
        let mut outbox = Outbox::default();
        for n in 0..10 {
            outbox.queue(id(n), wraps("a", n as u64), CHAT, now, true);
        }
        assert_eq!(outbox.dispatch(now).len(), PARALLEL);
        assert!(outbox.dispatch(now).is_empty());
        outbox.apply(id(0), Input::Answer { accepted: false }, now);
        assert_eq!(outbox.dispatch(now).len(), 1);

        let mut outbox = Outbox::default();
        outbox.hold_upload(id(100), "b".into());
        outbox.queue(id(1), wraps("b", 1), CHAT, now, true);
        outbox.queue(id(2), wraps("b", 2), CHAT, now, false);
        outbox.queue(id(3), wraps("c", 3), CHAT, now, true);
        outbox.queue(id(4), wraps("c", 4), CHAT, now + RETRY_DELAY, true);
        let sent: HashSet<Id> = outbox.dispatch(now).into_iter().map(|(id, _)| id).collect();
        assert_eq!(sent, HashSet::from([id(2), id(3)]));
        outbox.withdraw(id(100));
        let sent: Vec<Id> = outbox.dispatch(now).into_iter().map(|(id, _)| id).collect();
        assert_eq!(sent, [id(1)]);
    }

    #[test]
    fn one_count_for_every_cap() {
        let now = Instant::now();
        let mut outbox = Outbox::default();
        outbox.hold(
            id(1),
            "k".into(),
            Draft {
                peer: "a".into(),
                text: "hi".into(),
                reply_to: None,
                emoji: vec![],
            },
        );
        outbox.hold_upload(id(2), "a".into());
        outbox.queue(id(3), wraps("a", 3), CHAT, now, true);
        outbox.queue(id(4), wraps("a", 4), system(0), now, true);
        assert_eq!(outbox.pending(), 5);
        assert_eq!(outbox.wrapped().count(), 2);
        for n in 5..128 {
            outbox.queue(id(n), wraps("a", n as u64), CHAT, now, true);
        }
        assert_eq!(outbox.pending(), CAP);
        assert!(!outbox.fits(1, 0));
        assert!(outbox.fits(2, 2));
        assert!(!has_room(CAP, 1) && has_room(CAP - 1, 1));
    }

    #[test]
    fn a_refused_hold_keeps_its_row_until_retried() {
        let now = Instant::now();
        let mut outbox = Outbox::default();
        let draft = Draft {
            peer: "a".into(),
            text: "hi".into(),
            reply_to: None,
            emoji: vec![],
        };
        outbox.hold(id(1), "k".into(), draft.clone());
        assert_eq!(outbox.shown(id(1)), Some(Delivery::Pending));
        assert_eq!(
            outbox.apply(id(1), Input::Refuse, now).unwrap().mark,
            Some(Delivery::Failed)
        );
        assert_eq!(outbox.held(id(1)), Some(("k", &draft)));
        assert_eq!(
            outbox.apply(id(1), Input::Retry, now),
            Some(Change {
                effect: Effect::Resubmit,
                mark: Some(Delivery::Pending)
            })
        );
        assert!(!outbox.queued());
        assert_eq!(outbox.wrapped().count(), 0);
    }

    #[test]
    fn recovered_messages_wait_for_retry_and_system_ones_go_at_once() {
        let now = Instant::now();
        let mut outbox = Outbox::default();
        outbox.recover(id(1), wraps("a", 1), false, true, now);
        outbox.recover(id(2), wraps("a", 2), true, true, now);
        assert_eq!(outbox.shown(id(1)), Some(Delivery::Failed));
        assert_eq!(outbox.shown(id(2)), None);
        let sent: Vec<Id> = outbox.dispatch(now).into_iter().map(|(id, _)| id).collect();
        assert_eq!(sent, [id(2)]);
        assert!(!outbox.queue(id(1), wraps("a", 1), CHAT, now, true));
    }

    #[test]
    fn a_reconnect_restarts_only_system_deliveries_that_gave_up_or_wait() {
        let now = Instant::now();
        let mut outbox = Outbox::default();
        outbox.queue(id(1), wraps("a", 1), system(5), now, true);
        outbox.queue(id(2), wraps("a", 2), CHAT, now, true);
        outbox.dispatch(now);
        let refused = Input::Answer { accepted: false };
        outbox.apply(id(1), refused, now);
        outbox.apply(id(2), refused, now);
        assert_eq!(outbox.track(id(1)), Some(Track::Failed { kind: system(5) }));
        outbox.reconnected(now);
        assert_eq!(
            outbox.track(id(1)),
            Some(Track::Queued {
                kind: system(0),
                due: now + RETRY_DELAY
            })
        );
        assert_eq!(outbox.track(id(2)), Some(Track::Failed { kind: CHAT }));
    }
}
