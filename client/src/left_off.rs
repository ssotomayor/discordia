//! Where the person was when Android ended the app: the guild open, the words not yet
//! sent and, when the app restarts itself on purpose, the call it was in (#507).

use std::path::PathBuf;

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::identity::{Identity, config_dir};
use crate::nostr::nip44;
use crate::protocol::Id;
use crate::state::{AppState, ServerSession, VoicePhase};

const FILE_VERSION: u32 = 1;
/// A restart takes seconds; a call joined long after it opens a microphone nobody expects.
pub const REJOIN_WITHIN_MS: i64 = 30_000;
/// For all drafts together: written on the UI thread, and NIP-44 seals 64 KiB at most.
const MAX_DRAFT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub channel: Id,
    pub muted: bool,
    pub deafened: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejoin {
    pub call: Call,
    pub left_at_ms: i64,
}

impl Rejoin {
    pub fn fresh(&self, now_ms: i64) -> bool {
        (0..REJOIN_WITHIN_MS).contains(&(now_ms - self.left_at_ms))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Place {
    guild: Option<Id>,
    call: Option<Call>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LeftOff {
    account: String,
    at_ms: i64,
    #[serde(default)]
    guild: Option<Id>,
    #[serde(default)]
    call: Option<Call>,
    #[serde(default)]
    drafts: Vec<(Id, String)>,
}

/// Sealed to the account's own key: a DM's words are nowhere else on disk in the clear.
#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    sealed: String,
}

/// Read from the Activity's thread while the page is dead, so it cannot live in a signal.
struct Held {
    account: String,
    key: Option<[u8; 32]>,
    place: Place,
    drafts: Vec<(Id, String)>,
    /// Drafts of the last run whose composer has not mounted yet.
    restored: Vec<(Id, String)>,
}

static HELD: parking_lot::Mutex<Held> = parking_lot::Mutex::new(Held::new());

impl Held {
    const fn new() -> Self {
        Self {
            account: String::new(),
            key: None,
            place: Place {
                guild: None,
                call: None,
            },
            drafts: Vec::new(),
            restored: Vec::new(),
        }
    }

    fn note_draft(&mut self, channel: Id, text: &str) {
        self.drafts.retain(|(id, _)| *id != channel);
        if !text.trim().is_empty() {
            self.drafts.push((channel, text.to_string()));
        }
    }

    fn take_draft(&mut self, channel: Id) -> Option<String> {
        let at = self.restored.iter().position(|(id, _)| *id == channel)?;
        Some(self.restored.swap_remove(at).1)
    }

    /// `None` leaves the disk alone: before a view mounts, asking Android for storage can exit.
    fn snapshot(&self, with_call: bool, at_ms: i64) -> Option<Option<LeftOff>> {
        if self.account.is_empty() {
            return None;
        }
        let mut room = MAX_DRAFT_BYTES;
        let mut drafts = Vec::new();
        // Newest first, so the budget is spent on what was typed last.
        for (channel, text) in self.drafts.iter().rev().chain(&self.restored) {
            if text.len() <= room && !drafts.iter().any(|(id, _)| id == channel) {
                room -= text.len();
                drafts.push((*channel, text.clone()));
            }
        }
        let call = self.place.call.filter(|_| with_call);
        let nothing = self.place.guild.is_none() && call.is_none() && drafts.is_empty();
        Some((!nothing).then(|| LeftOff {
            account: self.account.clone(),
            at_ms,
            guild: self.place.guild,
            call,
            drafts,
        }))
    }
}

fn place_of(state: &AppState) -> Place {
    if state.server_session() == ServerSession::Absent {
        return Place::default();
    }
    let in_call = state.dm_call.is_none()
        && matches!(
            state.voice.phase(),
            VoicePhase::Connecting | VoicePhase::Connected
        );
    Place {
        guild: state.selected_guild,
        call: state
            .voice
            .channel_id()
            .filter(|_| in_call)
            .map(|channel| Call {
                channel,
                muted: state.voice.muted(),
                deafened: state.voice.deafened(),
            }),
    }
}

pub(crate) fn note_draft(channel: Id, text: &str) {
    HELD.lock().note_draft(channel, text);
}

pub(crate) fn take_draft(channel: Id) -> Option<String> {
    HELD.lock().take_draft(channel)
}

pub(crate) fn use_left_off(mut state: Signal<AppState>, identity: &Identity) {
    let account = identity.pubkey.clone();
    let (owner, secret) = (account.clone(), identity.secret_key());
    use_hook(move || {
        let key = nip44::conversation_key(&secret, &owner).ok();
        let left = key
            .filter(|_| cfg!(target_os = "android"))
            .and_then(|key| take(&owner, &key));
        let now = chrono::Utc::now().timestamp_millis();
        let drafts = restore(&mut state.write(), left, now);
        let mut held = HELD.lock();
        held.account = owner;
        held.key = key;
        held.place = Place::default();
        held.restored = drafts;
    });
    let place = use_memo(move || place_of(&state.read()));
    use_effect(move || HELD.lock().place = place());
    // The next account's view can mount before this one is dropped.
    use_drop(move || {
        let mut held = HELD.lock();
        if held.account == account {
            *held = Held::new();
        }
    });
}

fn restore(state: &mut AppState, left: Option<LeftOff>, now_ms: i64) -> Vec<(Id, String)> {
    let Some(left) = left else {
        return Vec::new();
    };
    if left.guild.is_some() {
        state.open_guild_on_ready = left.guild;
    }
    state.rejoin_voice_on_ready = left
        .call
        .map(|call| Rejoin {
            call,
            left_at_ms: left.at_ms,
        })
        .filter(|rejoin| rejoin.fresh(now_ms));
    left.drafts
}

fn path() -> PathBuf {
    config_dir().join("left-off.json")
}

fn write(left: &LeftOff, key: &[u8; 32]) -> Result<(), String> {
    let plain = serde_json::to_string(left).map_err(|e| format!("serialize: {e}"))?;
    let stored = Stored {
        version: FILE_VERSION,
        sealed: nip44::encrypt(key, &plain)?,
    };
    let content = serde_json::to_string(&stored).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(path(), content).map_err(|e| format!("write: {e}"))
}

pub(crate) fn clear() {
    let path = path();
    if path.exists()
        && let Err(error) = std::fs::remove_file(&path)
    {
        tracing::warn!(%error, "could not remove what the last run left off");
    }
}

/// Consumed by the start that reads it: a draft sent since must not come back later.
fn take(account: &str, key: &[u8; 32]) -> Option<LeftOff> {
    let content = std::fs::read_to_string(path()).ok()?;
    clear();
    let stored: Stored = serde_json::from_str(&content).ok()?;
    if stored.version != FILE_VERSION {
        return None;
    }
    let plain = nip44::decrypt(key, &stored.sealed).ok()?;
    let left: LeftOff = serde_json::from_str(&plain).ok()?;
    (left.account == account).then_some(left)
}

/// Called off the Dioxus thread, as the Activity pauses and as the process is ended.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) fn keep(with_call: bool) {
    let (left, key) = {
        let held = HELD.lock();
        let now = chrono::Utc::now().timestamp_millis();
        (held.snapshot(with_call, now), held.key)
    };
    match (left, key) {
        (None, _) => {}
        (Some(Some(left)), Some(key)) => {
            if let Err(error) = write(&left, &key) {
                tracing::warn!(%error, "could not keep where the app left off");
                clear();
            }
        }
        (Some(_), _) => clear(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Channel, ChannelKind, ClientMessage};
    use crate::state::{ConnectionStatus, VoiceEvent};

    fn call(channel: Id) -> Call {
        Call {
            channel,
            muted: false,
            deafened: false,
        }
    }

    fn held(account: &str) -> Held {
        let mut held = Held::new();
        held.account = account.into();
        held
    }

    fn voice_channel(state: &mut AppState) -> Id {
        let channel = Channel {
            id: Id::new_v4(),
            guild_id: Id::new_v4(),
            name: "voice".into(),
            kind: ChannelKind::Voice,
            topic: None,
            read_only: false,
            slowmode_secs: 0,
            position: 0,
            access: None,
            divider: Default::default(),
            markdown: false,
        };
        state.channels.push(channel.clone());
        channel.id
    }

    #[test]
    fn a_draft_is_held_until_it_is_sent_or_emptied() {
        let (a, b) = (Id::new_v4(), Id::new_v4());
        let mut held = held("me");
        held.note_draft(a, "half a thought");
        held.note_draft(b, "another");
        held.note_draft(a, "half a thought, finished");
        held.note_draft(b, "  \n");
        assert_eq!(
            held.snapshot(false, 1).flatten().unwrap().drafts,
            vec![(a, "half a thought, finished".to_string())]
        );
        held.note_draft(a, "");
        assert_eq!(held.snapshot(false, 1), Some(None), "nothing left to carry");
    }

    #[test]
    fn the_call_is_carried_only_by_a_restart_made_on_purpose() {
        let mut held = held("me");
        let channel = Id::new_v4();
        held.place.call = Some(call(channel));
        assert_eq!(held.snapshot(false, 1), Some(None));
        assert_eq!(
            held.snapshot(true, 1).flatten().unwrap().call,
            Some(call(channel))
        );
    }

    #[test]
    fn nothing_is_kept_without_an_account() {
        let mut held = Held::new();
        held.note_draft(Id::new_v4(), "typed while signing out");
        assert_eq!(held.snapshot(true, 1), None, "the disk is left alone");
    }

    #[test]
    fn a_restored_draft_nobody_opened_is_kept_again() {
        let (opened, unopened) = (Id::new_v4(), Id::new_v4());
        let mut held = held("me");
        held.restored = vec![(opened, "old".into()), (unopened, "waiting".into())];
        assert_eq!(held.take_draft(opened).as_deref(), Some("old"));
        assert_eq!(held.take_draft(opened), None, "handed over once");
        held.note_draft(opened, "old, edited");
        let mut drafts = held.snapshot(false, 1).flatten().unwrap().drafts;
        drafts.sort();
        let mut expected = vec![
            (opened, "old, edited".to_string()),
            (unopened, "waiting".to_string()),
        ];
        expected.sort();
        assert_eq!(drafts, expected);
    }

    #[test]
    fn drafts_past_the_budget_are_left_out_oldest_first() {
        let (old, new) = (Id::new_v4(), Id::new_v4());
        let mut held = held("me");
        held.note_draft(Id::new_v4(), &"x".repeat(MAX_DRAFT_BYTES + 1));
        assert_eq!(held.snapshot(false, 1), Some(None));

        held.note_draft(old, &"o".repeat(MAX_DRAFT_BYTES / 2));
        held.note_draft(new, &"n".repeat(MAX_DRAFT_BYTES / 2 + 1));
        let kept = held.snapshot(false, 1).flatten().unwrap().drafts;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].0, new);
    }

    #[test]
    fn the_place_is_the_server_sessions_and_the_call_is_a_guild_call() {
        let mut state = AppState::empty();
        let guild = Id::new_v4();
        let channel = voice_channel(&mut state);
        state.selected_guild = Some(guild);
        state.voice.on(VoiceEvent::Granted(channel));
        assert_eq!(place_of(&state), Place::default(), "Home has no place");

        state.status = ConnectionStatus::Ready;
        assert_eq!(
            place_of(&state),
            Place {
                guild: Some(guild),
                call: Some(call(channel)),
            }
        );

        state.status = ConnectionStatus::Reconnecting;
        assert_eq!(
            place_of(&state).call,
            Some(call(channel)),
            "a redial is still the same call"
        );

        state.voice.on(VoiceEvent::Ended);
        assert_eq!(place_of(&state).call, None);
    }

    #[test]
    fn what_was_kept_is_read_once_and_only_by_its_account() {
        let _dir = crate::identity::test_config_dir();
        let channel = Id::new_v4();
        let left = LeftOff {
            account: "me".into(),
            at_ms: 5,
            guild: Some(Id::new_v4()),
            call: Some(call(channel)),
            drafts: vec![(channel, "line one\nline two".into())],
        };
        let (mine, theirs) = ([7u8; 32], [8u8; 32]);
        write(&left, &mine).unwrap();
        let on_disk = std::fs::read_to_string(path()).unwrap();
        assert!(!on_disk.contains("line one"), "sealed: {on_disk}");
        assert_eq!(
            take("me", &theirs),
            None,
            "another account's key reads nothing"
        );
        assert_eq!(take("me", &mine), None, "and that start consumed it");

        write(&left, &mine).unwrap();
        assert_eq!(take("someone-else", &mine), None);

        write(&left, &mine).unwrap();
        assert_eq!(take("me", &mine), Some(left));
        assert_eq!(take("me", &mine), None);
    }

    #[test]
    fn a_fresh_restart_rejoins_its_call_muted_as_it_was() {
        let mut state = AppState::empty();
        let channel = voice_channel(&mut state);
        let guild = Id::new_v4();
        let left = LeftOff {
            account: "me".into(),
            at_ms: 1_000,
            guild: Some(guild),
            call: Some(Call {
                channel,
                muted: true,
                deafened: false,
            }),
            drafts: vec![(channel, "kept".into())],
        };
        let drafts = restore(&mut state, Some(left), 4_000);
        assert_eq!(drafts, vec![(channel, "kept".to_string())]);
        assert_eq!(state.open_guild_on_ready, Some(guild));
        assert!(
            !state.voice.muted(),
            "nothing is muted until the call is joined"
        );

        state.status = ConnectionStatus::Ready;
        let join = state.rejoin_left_call(5_000);
        assert!(
            matches!(join, Some(ClientMessage::JoinVoice { channel_id, preferences: Some(p) }) if channel_id == channel && p.muted && !p.deafened)
        );
        assert!(state.voice.muted());
        assert!(state.rejoin_left_call(5_000).is_none(), "asked once");
    }

    #[test]
    fn a_call_is_not_rejoined_late_into_another_call_or_into_a_missing_channel() {
        let rejoin = |channel, left_at_ms| Rejoin {
            call: call(channel),
            left_at_ms,
        };

        let mut state = AppState::empty();
        let channel = voice_channel(&mut state);
        let left = LeftOff {
            account: "me".into(),
            at_ms: 0,
            guild: None,
            call: Some(call(channel)),
            drafts: Vec::new(),
        };
        restore(&mut state, Some(left), REJOIN_WITHIN_MS);
        assert_eq!(state.rejoin_voice_on_ready, None, "stale at the start");

        state.rejoin_voice_on_ready = Some(rejoin(channel, 0));
        assert!(
            state.rejoin_left_call(REJOIN_WITHIN_MS).is_none(),
            "stale at Ready"
        );

        state.rejoin_voice_on_ready = Some(rejoin(channel, 10));
        assert!(state.rejoin_left_call(5).is_none(), "the clock went back");

        state.rejoin_voice_on_ready = Some(rejoin(Id::new_v4(), 0));
        assert!(state.rejoin_left_call(1).is_none(), "the channel is gone");

        let other = voice_channel(&mut state);
        state.voice.on(VoiceEvent::Granted(other));
        state.rejoin_voice_on_ready = Some(rejoin(channel, 0));
        assert!(state.rejoin_left_call(1).is_none(), "already in a call");
        assert_eq!(state.voice.channel_id(), Some(other));
    }
}
