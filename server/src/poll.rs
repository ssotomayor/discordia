use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::protocol::Poll;

/// A poll as its message row holds it. Only `view` may leave the server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredPoll {
    pub options: Vec<String>,
    #[serde(default)]
    pub anonymous: bool,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub changeable: bool,
    #[serde(default)]
    pub closes_at_ms: Option<i64>,
    #[serde(default)]
    pub closed_at_ms: Option<i64>,
    /// Kept for an anonymous poll too: it is what refuses a second vote.
    #[serde(default)]
    pub votes: BTreeMap<String, Vec<u8>>,
}

pub struct NewPoll {
    pub options: Vec<String>,
    pub anonymous: bool,
    pub multiple: bool,
    pub changeable: bool,
    pub duration_secs: Option<u32>,
}

const MIN_DURATION_SECS: u32 = 60;

impl StoredPoll {
    pub fn new(spec: NewPoll, now_ms: i64) -> Result<Self, String> {
        let closes_at_ms = match spec.duration_secs {
            None => None,
            Some(secs) if (MIN_DURATION_SECS..=crate::protocol::POLL_MAX_SECS).contains(&secs) => {
                Some(now_ms + i64::from(secs) * 1000)
            }
            Some(_) => {
                return Err(format!(
                    "a poll runs from a minute to {} days",
                    crate::protocol::POLL_MAX_SECS / 86_400
                ));
            }
        };
        Ok(Self {
            options: spec.options,
            anonymous: spec.anonymous,
            multiple: spec.multiple,
            changeable: spec.changeable,
            closes_at_ms,
            closed_at_ms: None,
            votes: BTreeMap::new(),
        })
    }

    pub fn is_open(&self, now_ms: i64) -> bool {
        self.closed_at_ms.is_none() && self.closes_at_ms.is_none_or(|at| now_ms < at)
    }

    pub fn counts(&self) -> Vec<u32> {
        let mut counts = vec![0; self.options.len()];
        for choice in self.votes.values().flatten() {
            if let Some(count) = counts.get_mut(usize::from(*choice)) {
                *count += 1;
            }
        }
        counts
    }

    pub fn voters(&self) -> Vec<Vec<String>> {
        if self.anonymous {
            return Vec::new();
        }
        let mut voters = vec![Vec::new(); self.options.len()];
        for (voter, choices) in &self.votes {
            for choice in choices {
                if let Some(list) = voters.get_mut(usize::from(*choice)) {
                    list.push(voter.clone());
                }
            }
        }
        voters
    }

    pub fn choices_of(&self, voter: &str) -> Vec<u8> {
        self.votes.get(voter).cloned().unwrap_or_default()
    }

    pub fn closed_at(&self) -> Option<DateTime<Utc>> {
        self.closed_at_ms.and_then(DateTime::from_timestamp_millis)
    }

    pub fn view(&self, viewer: Option<&str>) -> Poll {
        Poll {
            options: self.options.clone(),
            anonymous: self.anonymous,
            multiple: self.multiple,
            changeable: self.changeable,
            closes_at: self.closes_at_ms.and_then(DateTime::from_timestamp_millis),
            closed_at: self.closed_at(),
            counts: self.counts(),
            voters: self.voters(),
            mine: viewer.map(|v| self.choices_of(v)).unwrap_or_default(),
        }
    }

    /// The voter's choices as now held; empty when withdrawn.
    pub fn vote(&mut self, voter: &str, choices: &[u8], now_ms: i64) -> Result<Vec<u8>, String> {
        if !self.is_open(now_ms) {
            return Err("this poll has closed".into());
        }
        let choices =
            crate::protocol::normalize_choices(choices, self.options.len(), self.multiple)?;
        let previous = self.votes.get(voter);
        if previous.is_some_and(|p| *p == choices) {
            return Ok(choices);
        }
        if previous.is_some() && !self.changeable {
            return Err("this poll doesn't allow changing a vote".into());
        }
        if choices.is_empty() {
            if previous.is_none() {
                return Ok(choices);
            }
            self.votes.remove(voter);
        } else {
            self.votes.insert(voter.to_string(), choices.clone());
        }
        Ok(choices)
    }

    /// Whether this closed it; an ended poll is closed already.
    pub fn close(&mut self, now_ms: i64) -> bool {
        if !self.is_open(now_ms) {
            return false;
        }
        self.closed_at_ms = Some(now_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll(anonymous: bool, multiple: bool, changeable: bool) -> StoredPoll {
        StoredPoll::new(
            NewPoll {
                options: vec!["a".into(), "b".into(), "c".into()],
                anonymous,
                multiple,
                changeable,
                duration_secs: Some(3600),
            },
            1_000,
        )
        .unwrap()
    }

    #[test]
    fn an_anonymous_poll_counts_votes_but_names_no_voter_except_to_themselves() {
        let mut p = poll(true, false, false);
        p.vote("alice", &[1], 2_000).unwrap();
        p.vote("bob", &[1], 2_000).unwrap();
        let seen_by_carol = p.view(Some("carol"));
        assert_eq!(seen_by_carol.counts, vec![0, 2, 0]);
        assert!(seen_by_carol.voters.is_empty());
        assert!(seen_by_carol.mine.is_empty());
        assert_eq!(p.view(Some("alice")).mine, vec![1]);
        let json = serde_json::to_string(&p.view(None)).unwrap();
        assert!(!json.contains("alice") && !json.contains("bob"), "{json}");
    }

    #[test]
    fn a_public_poll_lists_who_chose_each_answer() {
        let mut p = poll(false, true, true);
        p.vote("alice", &[0, 2], 2_000).unwrap();
        p.vote("bob", &[2], 2_000).unwrap();
        assert_eq!(
            p.view(None).voters,
            vec![
                vec!["alice".to_string()],
                vec![],
                vec!["alice".into(), "bob".into()]
            ]
        );
        assert_eq!(p.counts(), vec![1, 0, 2]);
    }

    #[test]
    fn a_vote_is_final_unless_the_poll_allows_changing_it() {
        let mut fixed = poll(false, false, false);
        fixed.vote("alice", &[0], 2_000).unwrap();
        assert_eq!(
            fixed.vote("alice", &[0], 2_000),
            Ok(vec![0]),
            "repeating it is not a change"
        );
        assert!(fixed.vote("alice", &[1], 2_000).is_err());
        assert!(fixed.vote("alice", &[], 2_000).is_err());

        let mut open = poll(false, false, true);
        open.vote("alice", &[0], 2_000).unwrap();
        open.vote("alice", &[1], 2_000).unwrap();
        assert_eq!(open.counts(), vec![0, 1, 0]);
        open.vote("alice", &[], 2_000).unwrap();
        assert_eq!(open.counts(), vec![0, 0, 0]);
        assert!(open.votes.is_empty());
    }

    #[test]
    fn single_choice_polls_take_one_answer_and_ended_polls_take_none() {
        let mut p = poll(false, false, true);
        assert!(p.vote("alice", &[0, 1], 2_000).is_err());
        assert!(p.vote("alice", &[7], 2_000).is_err());
        assert!(
            p.vote("alice", &[0], 1_000 + 3_600_000).is_err(),
            "past its end time"
        );
        let mut closing = poll(false, false, true);
        assert!(closing.close(2_000));
        assert!(!closing.close(3_000), "closing twice changes nothing");
        assert!(closing.vote("alice", &[0], 2_500).is_err());
        assert_eq!(
            closing.view(None).closed_at.map(|t| t.timestamp_millis()),
            Some(2_000)
        );
    }

    #[test]
    fn a_duration_is_a_minute_to_the_cap_or_none() {
        let spec = |duration_secs| NewPoll {
            options: vec!["a".into(), "b".into()],
            anonymous: false,
            multiple: false,
            changeable: false,
            duration_secs,
        };
        assert_eq!(StoredPoll::new(spec(None), 0).unwrap().closes_at_ms, None);
        assert_eq!(
            StoredPoll::new(spec(Some(60)), 0).unwrap().closes_at_ms,
            Some(60_000)
        );
        assert!(StoredPoll::new(spec(Some(59)), 0).is_err());
        assert!(StoredPoll::new(spec(Some(crate::protocol::POLL_MAX_SECS + 1)), 0).is_err());
    }
}
