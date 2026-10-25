use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    event::{self, Rumor},
    nip17,
};

const TAG: &str = "discordia-call";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Started,
    Missed,
    Failed,
    Busy,
    Declined,
    Answered,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub version: u8,
    pub call_id: Uuid,
    pub caller: String,
    pub callee: String,
    pub started_at: i64,
    pub outcome: Outcome,
}

impl Record {
    pub fn peer<'a>(&'a self, account: &str) -> Option<&'a str> {
        if self.caller == account {
            Some(&self.callee)
        } else if self.callee == account {
            Some(&self.caller)
        } else {
            None
        }
    }

    pub fn id(&self) -> crate::protocol::Id {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"discordia/dm-call-history/1");
        hash.update(self.caller.as_bytes());
        hash.update(self.call_id.as_bytes());
        let digest = hash.finalize();
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&digest[..16]);
        crate::protocol::Id::from_bytes(bytes)
    }

    pub fn label(&self, account: &str) -> &'static str {
        match (self.outcome, self.caller == account) {
            (Outcome::Started, _) => "Voice call",
            (Outcome::Missed, true) => "Voice call not answered",
            (Outcome::Missed, false) => "Missed voice call",
            (Outcome::Failed, _) => "Voice call failed",
            (Outcome::Busy, _) => "Voice call: busy",
            (Outcome::Declined, _) => "Voice call declined",
            (Outcome::Answered, _) => "Voice call answered",
        }
    }

    pub fn replaces(&self, previous: &Self) -> bool {
        self.call_id == previous.call_id
            && self.caller == previous.caller
            && self.callee == previous.callee
            && self.started_at == previous.started_at
            && self.outcome > previous.outcome
    }

    fn validate(&self, author: &str, at: i64) -> Result<(), String> {
        if self.version != 1
            || self.call_id.is_nil()
            || self.caller == self.callee
            || self.caller.parse::<secp256k1::XOnlyPublicKey>().is_err()
            || self.callee.parse::<secp256k1::XOnlyPublicKey>().is_err()
            || self.started_at <= 0
            || self.started_at
                > at.saturating_add(super::calls::MAX_CLOCK_SKEW)
                    .saturating_add(super::calls::TTL)
            || chrono::DateTime::from_timestamp(self.started_at, 0).is_none()
            || (author != self.caller && author != self.callee)
            || (matches!(self.outcome, Outcome::Started | Outcome::Missed) && author != self.caller)
            || (!matches!(
                self.outcome,
                Outcome::Started | Outcome::Missed | Outcome::Failed
            ) && author != self.callee)
        {
            return Err("Invalid voice call history record".into());
        }
        Ok(())
    }
}

pub fn rumor(account: &str, record: &Record, at: i64) -> Result<Rumor, String> {
    record.validate(account, at)?;
    let peer = record
        .peer(account)
        .ok_or("Call does not belong to this account")?;
    let metadata = serde_json::to_string(record).map_err(|error| error.to_string())?;
    Ok(event::rumor(
        account,
        at,
        nip17::KIND_CHAT,
        vec![vec!["p".into(), peer.into()], vec![TAG.into(), metadata]],
        format!("[{}]", record.label(&record.caller)),
    ))
}

pub fn open(account: &str, rumor: &Rumor) -> Result<Option<Record>, String> {
    let mut tags = rumor
        .tags
        .iter()
        .filter(|tag| tag.first().is_some_and(|name| name == TAG));
    let Some(tag) = tags.next() else {
        return Ok(None);
    };
    let data = tag
        .get(1)
        .filter(|data| data.len() <= 1024)
        .ok_or("Invalid call history metadata")?;
    if tag.len() != 2 || tags.next().is_some() {
        return Err("Duplicate call history metadata".into());
    }
    let record: Record = serde_json::from_str(data).map_err(|_| "Invalid call history metadata")?;
    record.validate(&rumor.pubkey, rumor.created_at)?;
    let message = nip17::open_rumor(account, rumor)?;
    if record.peer(account) != Some(message.peer.as_str()) {
        return Err("Call history participants do not match".into());
    }
    Ok(Some(record))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub record: Record,
    pub confirmed: bool,
    pub missed_at: Option<i64>,
    expired: bool,
    recorded_at: i64,
}

impl Entry {
    pub fn outcome(&self, at: i64, offset: i64) -> Outcome {
        if self.record.outcome == Outcome::Started
            && at
                >= self
                    .record
                    .started_at
                    .saturating_sub(offset)
                    .saturating_add(45)
        {
            Outcome::Missed
        } else {
            self.record.outcome
        }
    }
}

pub fn merge(
    state: &mut crate::state::AppState,
    account: &str,
    record: Record,
    author: &str,
    at: i64,
) {
    use crate::state::{DmInfo, DmStanding};
    let Some(peer) = record.peer(account).map(str::to_owned) else {
        return;
    };
    if matches!(
        state.dm_standing(&peer),
        DmStanding::Blocked | DmStanding::Ignored
    ) {
        return;
    }
    let id = record.id();
    let previous = state.dm_call_history.get(&id).cloned();
    if let Some(previous) = &previous
        && record != previous.record
        && !record.replaces(&previous.record)
        && !previous.record.replaces(&record)
    {
        return;
    }
    let mut entry = previous.unwrap_or(Entry {
        record: record.clone(),
        confirmed: false,
        missed_at: None,
        expired: false,
        recorded_at: at,
    });
    if record.replaces(&entry.record) {
        entry.record = record;
        entry.recorded_at = at;
    }
    entry.confirmed |= author == entry.record.caller;
    if !entry.confirmed
        && !state.dm_call_history.contains_key(&id)
        && state
            .dm_call_history
            .values()
            .filter(|entry| !entry.confirmed)
            .count()
            >= 128
    {
        return;
    }
    if !entry.confirmed {
        // A callee may report an outcome but cannot invent an outgoing attempt
        // for another account; its caller must first attest that call id.
        state.dm_call_history.insert(id, entry);
        return;
    }
    let cid = super::service::conversation_id(&peer);
    let offset = state.clock_offset(&entry.record.caller);
    let created_at = entry.record.started_at.saturating_sub(offset);
    let outcome = entry.outcome(at, offset);
    let text = Record {
        outcome,
        ..entry.record.clone()
    }
    .label(account);
    if !state.dms.iter().any(|dm| dm.channel_id == cid) {
        state.dms.push(DmInfo {
            channel_id: cid,
            other_pubkey: peer.clone(),
        });
    }
    if let Some(message) = state
        .messages
        .get_mut(&cid)
        .and_then(|messages| messages.iter_mut().find(|message| message.id == id))
    {
        message.content = format!("[{text}]");
    } else {
        state.insert_message(
            cid,
            crate::protocol::Message {
                id,
                channel_id: cid,
                author: crate::protocol::User {
                    pubkey: entry.record.caller.clone(),
                    username: String::new(),
                },
                content: format!("[{text}]"),
                created_at: chrono::DateTime::from_timestamp(created_at, 0)
                    .unwrap_or_else(chrono::Utc::now),
                author_bot: false,
                edited_at: None,
                image_metadata: None,
                attachment: None,
                pinned: false,
                image: None,
                reactions: Vec::new(),
                reply_to: None,
            },
        );
    }
    if outcome == Outcome::Missed {
        entry.expired = true;
    }
    if outcome == Outcome::Missed
        && entry.record.callee == account
        && state.dm_standing(&peer) == DmStanding::Inbox
    {
        let missed_at = if entry.record.outcome == Outcome::Started {
            created_at.saturating_add(45)
        } else {
            entry.recorded_at
        };
        if let Some(previous) = entry.missed_at {
            if missed_at < previous {
                if state
                    .dm_read_at
                    .get(&peer)
                    .is_some_and(|read| missed_at <= *read && *read < previous)
                    && let Some(unread) = state.dm_unread.get_mut(&cid)
                {
                    *unread = unread.saturating_sub(1);
                }
                entry.missed_at = Some(missed_at);
            }
        } else {
            entry.missed_at = Some(missed_at);
            state.note_dm_arrival(cid, &peer, missed_at);
        }
    } else if outcome > Outcome::Missed
        && let Some(missed_at) = entry.missed_at.take()
        && state
            .dm_read_at
            .get(&peer)
            .is_none_or(|read| missed_at > *read)
        && let Some(unread) = state.dm_unread.get_mut(&cid)
    {
        *unread = unread.saturating_sub(1);
    }
    state.dm_call_history.insert(id, entry);
}

pub fn next_expiry(state: &crate::state::AppState) -> Option<i64> {
    state
        .dm_call_history
        .values()
        .filter(|entry| {
            entry.confirmed
                && entry.record.outcome == Outcome::Started
                && !entry.expired
                && entry.missed_at.is_none()
        })
        .map(|entry| {
            entry
                .record
                .started_at
                .saturating_sub(state.clock_offset(&entry.record.caller))
                .saturating_add(45)
        })
        .min()
}

pub fn reconcile(state: &mut crate::state::AppState, account: &str, at: i64) {
    let records: Vec<_> = state
        .dm_call_history
        .values()
        .filter(|entry| entry.confirmed)
        .map(|entry| entry.record.clone())
        .collect();
    for record in records {
        let author = record.caller.clone();
        merge(state, account, record, &author, at);
    }
}

pub fn expire(state: &mut crate::state::AppState, account: &str, at: i64) {
    let records: Vec<_> = state
        .dm_call_history
        .values()
        .filter(|entry| {
            entry.confirmed
                && entry.record.outcome == Outcome::Started
                && !entry.expired
                && entry.missed_at.is_none()
                && entry
                    .record
                    .started_at
                    .saturating_sub(state.clock_offset(&entry.record.caller))
                    .saturating_add(45)
                    <= at
        })
        .map(|entry| entry.record.clone())
        .collect();
    for record in records {
        let id = record.id();
        let caller = record.caller.clone();
        merge(state, account, record, &caller, at);
        if let Some(entry) = state.dm_call_history.get_mut(&id) {
            entry.expired = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> secp256k1::SecretKey {
        secp256k1::SecretKey::from_slice(&[byte; 32]).unwrap()
    }
    fn record() -> Record {
        Record {
            version: 1,
            call_id: Uuid::new_v4(),
            caller: event::xonly_hex(&key(1)),
            callee: event::xonly_hex(&key(2)),
            started_at: 100,
            outcome: Outcome::Missed,
        }
    }

    #[test]
    fn record_is_encrypted_for_both_participants_and_replay_uses_one_row() {
        let record = record();
        let rumor = rumor(&record.caller, &record, 145).unwrap();
        let (theirs, ours) =
            super::super::nip17::wrap_both(&key(1), &record.callee, &rumor, 145).unwrap();
        assert!(
            !theirs
                .tags
                .iter()
                .any(|tag| tag.first().is_some_and(|name| name == "expiration"))
        );
        assert!(!theirs.content.contains(&record.call_id.to_string()));
        for (account, secret, wrapped) in [
            (&record.callee, key(2), theirs),
            (&record.caller, key(1), ours),
        ] {
            let decoded = super::super::nip59::unwrap(&secret, &wrapped).unwrap();
            let decoded = open(account, &decoded).unwrap().unwrap();
            assert_eq!(decoded.id(), record.id());
            assert_eq!(decoded, record);
        }
    }

    #[test]
    fn accepted_or_declined_on_another_device_overrides_a_late_missed_record() {
        let missed = record();
        for outcome in [Outcome::Answered, Outcome::Declined, Outcome::Busy] {
            let settled = Record {
                outcome,
                ..missed.clone()
            };
            assert!(settled.replaces(&missed));
            assert!(!missed.replaces(&settled));
            assert_eq!(settled.id(), missed.id());
        }
    }

    #[test]
    fn only_the_callee_can_claim_the_call_was_answered() {
        let record = Record {
            outcome: Outcome::Answered,
            ..record()
        };
        assert!(rumor(&record.caller, &record, 145).is_err());
        let answer = rumor(&record.callee, &record, 145).unwrap();
        assert_eq!(open(&record.caller, &answer).unwrap(), Some(record));
    }

    #[test]
    fn unrelated_accounts_and_invalid_metadata_cannot_create_call_history() {
        let record = record();
        let mut event = rumor(&record.caller, &record, 145).unwrap();
        assert!(open(&event::xonly_hex(&key(3)), &event).is_err());
        event.tags.push(vec![TAG.into(), "{}".into()]);
        assert!(open(&record.callee, &event).is_err());
    }
    fn inbox(record: &Record, account: &str) -> crate::state::AppState {
        let mut state = crate::state::AppState::empty();
        state.self_user = Some(crate::protocol::User {
            pubkey: account.into(),
            username: String::new(),
        });
        state
            .dm_accepted
            .insert(record.peer(account).unwrap().into());
        state
    }

    #[test]
    fn duplicate_missed_calls_have_one_row_and_one_unread_message() {
        let record = record();
        let mut state = inbox(&record, &record.callee);
        for _ in 0..3 {
            merge(
                &mut state,
                &record.callee,
                record.clone(),
                &record.caller,
                145,
            );
        }
        let cid = super::super::service::conversation_id(&record.caller);
        assert_eq!(state.messages[&cid].len(), 1);
        assert_eq!(state.dm_unread[&cid], 1);
        state.mark_dm_read(cid);
        assert_eq!(state.dm_read_at[&record.caller], 145);
        state.dm_call_history.clear();
        state.messages.clear();
        merge(
            &mut state,
            &record.callee,
            record.clone(),
            &record.caller,
            145,
        );
        assert!(state.dm_unread.is_empty());
    }

    #[test]
    fn authenticated_acceptance_overrides_a_missed_call_without_adding_a_row() {
        let missed = record();
        let mut state = inbox(&missed, &missed.callee);
        merge(
            &mut state,
            &missed.callee,
            missed.clone(),
            &missed.caller,
            145,
        );
        let answered = Record {
            outcome: Outcome::Answered,
            ..missed.clone()
        };
        merge(
            &mut state,
            &missed.callee,
            answered.clone(),
            &missed.callee,
            146,
        );
        merge(
            &mut state,
            &missed.callee,
            missed.clone(),
            &missed.caller,
            145,
        );
        assert_eq!(state.dm_call_history[&missed.id()].record, answered);
        let cid = super::super::service::conversation_id(&missed.caller);
        assert_eq!(state.messages[&cid].len(), 1);
        assert_eq!(state.dm_unread[&cid], 0);
    }

    #[test]
    fn a_peer_cannot_invent_an_outgoing_call_without_its_callers_confirmation() {
        let started = Record {
            outcome: Outcome::Started,
            ..record()
        };
        let answered = Record {
            outcome: Outcome::Answered,
            ..started.clone()
        };
        let mut state = inbox(&started, &started.caller);
        merge(
            &mut state,
            &started.caller,
            answered.clone(),
            &started.callee,
            110,
        );
        assert!(state.messages.is_empty());
        merge(
            &mut state,
            &started.caller,
            started.clone(),
            &started.caller,
            100,
        );
        let cid = super::super::service::conversation_id(&started.callee);
        assert_eq!(state.messages[&cid].len(), 1);
        assert_eq!(state.dm_call_history[&started.id()].record, answered);
        assert!(state.dm_call_history[&started.id()].confirmed);
    }

    #[test]
    fn an_offline_attempt_expires_once_but_a_connection_failure_is_not_missed() {
        let started = Record {
            outcome: Outcome::Started,
            ..record()
        };
        let mut state = inbox(&started, &started.callee);
        merge(
            &mut state,
            &started.callee,
            started.clone(),
            &started.caller,
            100,
        );
        assert_eq!(next_expiry(&state), Some(145));
        expire(&mut state, &started.callee, 144);
        assert!(state.dm_unread.is_empty());
        expire(&mut state, &started.callee, 145);
        expire(&mut state, &started.callee, 146);
        assert_eq!(next_expiry(&state), None);
        assert_eq!(state.dm_unread.values().sum::<u32>(), 1);
        let failed = Record {
            outcome: Outcome::Failed,
            ..started.clone()
        };
        merge(
            &mut state,
            &started.callee,
            failed.clone(),
            &started.caller,
            147,
        );
        assert_eq!(state.dm_unread.values().sum::<u32>(), 0);
        assert_eq!(
            state.dm_call_history[&started.id()].outcome(200, 0),
            Outcome::Failed
        );
    }

    #[test]
    fn a_read_offline_call_does_not_reappear_as_unread_during_replay() {
        let started = Record {
            outcome: Outcome::Started,
            ..record()
        };
        let mut state = inbox(&started, &started.callee);
        state.dm_read_at.insert(started.caller.clone(), 150);
        merge(
            &mut state,
            &started.callee,
            started.clone(),
            &started.caller,
            1000,
        );
        assert!(state.dm_unread.is_empty());
        assert_eq!(state.dm_call_history[&started.id()].missed_at, Some(145));
    }

    #[test]
    fn unrelated_reused_ids_and_blocked_peers_cannot_replace_history() {
        let record = record();
        let mut state = inbox(&record, &record.callee);
        merge(
            &mut state,
            &record.callee,
            record.clone(),
            &record.caller,
            145,
        );
        let forged = Record {
            started_at: 99,
            outcome: Outcome::Answered,
            ..record.clone()
        };
        merge(&mut state, &record.callee, forged, &record.callee, 145);
        assert_eq!(state.dm_call_history[&record.id()].record, record);
        state.dm_accepted.clear();
        state.mutes = state.mutes.with(&record.caller);
        state.dm_call_history.clear();
        state.messages.clear();
        merge(
            &mut state,
            &record.callee,
            record.clone(),
            &record.caller,
            145,
        );
        assert!(state.messages.is_empty());
    }

    #[test]
    fn only_the_caller_can_publish_the_attempt_or_missed_outcome() {
        for outcome in [Outcome::Started, Outcome::Missed] {
            let record = Record {
                outcome,
                ..record()
            };
            assert!(rumor(&record.callee, &record, 145).is_err());
            assert!(rumor(&record.caller, &record, 145).is_ok());
        }
        let ordinary =
            nip17::chat_rumor(&record().caller, &record().callee, "hello", None, &[], 145);
        assert_eq!(open(&record().callee, &ordinary).unwrap(), None);
    }
    #[test]
    fn replaying_an_early_cancellation_corrects_the_provisional_timeout_unread() {
        let started = Record {
            outcome: Outcome::Started,
            ..record()
        };
        let mut state = inbox(&started, &started.callee);
        state.dm_read_at.insert(started.caller.clone(), 120);
        merge(
            &mut state,
            &started.callee,
            started.clone(),
            &started.caller,
            1000,
        );
        assert_eq!(state.dm_unread.values().sum::<u32>(), 1);
        let missed = Record {
            outcome: Outcome::Missed,
            ..started.clone()
        };
        merge(
            &mut state,
            &started.callee,
            missed.clone(),
            &started.caller,
            110,
        );
        assert_eq!(state.dm_unread.values().sum::<u32>(), 0);
        assert_eq!(state.dm_call_history[&started.id()].missed_at, Some(110));
    }
    #[test]
    fn expiring_an_outgoing_call_does_not_consume_unread_messages_when_answered() {
        let started = Record {
            outcome: Outcome::Started,
            ..record()
        };
        let mut state = inbox(&started, &started.caller);
        let cid = super::super::service::conversation_id(&started.callee);
        state.dm_unread.insert(cid, 2);
        merge(
            &mut state,
            &started.caller,
            started.clone(),
            &started.caller,
            100,
        );
        expire(&mut state, &started.caller, 145);
        assert_eq!(next_expiry(&state), None);
        let answered = Record {
            outcome: Outcome::Answered,
            ..started.clone()
        };
        merge(&mut state, &started.caller, answered, &started.callee, 146);
        assert_eq!(state.dm_unread[&cid], 2);
    }

    #[test]
    fn unconfirmed_outcomes_have_a_fixed_memory_bound() {
        let answered = Record {
            outcome: Outcome::Answered,
            ..record()
        };
        let mut state = inbox(&answered, &answered.caller);
        for _ in 0..150 {
            merge(
                &mut state,
                &answered.caller,
                Record {
                    call_id: Uuid::new_v4(),
                    ..answered.clone()
                },
                &answered.callee,
                110,
            );
        }
        assert_eq!(state.dm_call_history.len(), 128);
        assert!(state.messages.is_empty());
    }
    #[test]
    fn history_received_before_contacts_replay_is_retained_without_ringing() {
        let record = record();
        let mut state = inbox(&record, &record.callee);
        state.dm_accepted.clear();
        merge(
            &mut state,
            &record.callee,
            record.clone(),
            &record.caller,
            145,
        );
        assert!(state.dm_unread.is_empty());
        assert_eq!(state.dm_notify_tick, 0);
        assert_eq!(next_expiry(&state), None);
        state.dm_accepted.insert(record.caller.clone());
        reconcile(&mut state, &record.callee, 145);
        assert_eq!(state.dm_unread.values().sum::<u32>(), 1);
        let cid = super::super::service::conversation_id(&record.caller);
        assert_eq!(state.messages[&cid].len(), 1);
    }
}
