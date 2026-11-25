//! Paging back through DM history. A relay can be asked for gift wraps to us
//! only by time, never by conversation, so each page goes back across all of them.

use std::collections::{BTreeSet, HashSet};
use std::time::{Duration, Instant};

use crate::protocol::Id;

/// Gift wraps per relay per page.
pub const PAGE: u32 = 200;
const PATIENCE: Duration = Duration::from_secs(20);
/// Pages fetched for one press before giving up on a conversation they never reached.
const EMPTY_PAGES: usize = 5;

/// What the conversation view draws from this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Look {
    pub loading: bool,
    /// Every relay has said it holds nothing older.
    pub exhausted: bool,
}

struct Page {
    waiting: HashSet<String>,
    due: Instant,
    channel: Id,
    held: usize,
    pages: usize,
}

/// A page ended: whether it reached the conversation it was fetched for is the caller's to say.
pub struct Done {
    channel: Id,
    held: usize,
    pages: usize,
}

impl Done {
    pub fn channel(&self) -> Id {
        self.channel
    }
}

#[derive(Default)]
pub struct History {
    relays: BTreeSet<String>,
    replayed: HashSet<String>,
    ended: HashSet<String>,
    page: Option<Page>,
}

impl History {
    pub fn new(relays: impl IntoIterator<Item = String>) -> Self {
        Self {
            relays: relays.into_iter().collect(),
            ..Default::default()
        }
    }

    /// The first end of a relay's DM replay. A reconnect resumes from the newest
    /// event, so only the first one says whether the relay holds anything.
    pub fn replayed(&mut self, relay: &str, count: usize) {
        if self.replayed.insert(relay.to_string()) && count == 0 {
            self.ended.insert(relay.to_string());
        }
    }

    /// Starts a page for `channel`, which holds `held` messages, from the relays
    /// in `up`; false when one is already running or no relay could answer.
    pub fn start(&mut self, channel: Id, held: usize, up: &HashSet<String>, now: Instant) -> bool {
        self.page.is_none() && self.open(channel, held, 1, up, now)
    }

    /// After `done`, another page while the conversation has gained nothing.
    pub fn again(&mut self, done: Done, held: usize, up: &HashSet<String>, now: Instant) -> bool {
        held == done.held
            && done.pages < EMPTY_PAGES
            && self.page.is_none()
            && self.open(done.channel, held, done.pages + 1, up, now)
    }

    fn open(
        &mut self,
        channel: Id,
        held: usize,
        pages: usize,
        up: &HashSet<String>,
        now: Instant,
    ) -> bool {
        let waiting: HashSet<String> = up
            .iter()
            .filter(|relay| self.relays.contains(*relay) && !self.ended.contains(*relay))
            .cloned()
            .collect();
        if waiting.is_empty() {
            return false;
        }
        self.page = Some(Page {
            waiting,
            due: now + PATIENCE,
            channel,
            held,
            pages,
        });
        true
    }

    /// Only an empty answer ends a relay: many cap `limit` below what was asked,
    /// so a short page is not the end of what they hold.
    pub fn finished(&mut self, relay: &str, count: usize) -> Option<Done> {
        if count == 0 {
            self.ended.insert(relay.to_string());
        }
        self.settle(relay)
    }

    pub fn disconnected(&mut self, relay: &str) -> Option<Done> {
        self.settle(relay)
    }

    fn settle(&mut self, relay: &str) -> Option<Done> {
        let page = self.page.as_mut()?;
        page.waiting.remove(relay);
        if page.waiting.is_empty() {
            self.end()
        } else {
            None
        }
    }

    pub fn due(&self) -> Option<Instant> {
        self.page.as_ref().map(|page| page.due)
    }

    pub fn overdue(&mut self, now: Instant) -> Option<Done> {
        if self.due().is_some_and(|due| due <= now) {
            self.end()
        } else {
            None
        }
    }

    fn end(&mut self) -> Option<Done> {
        self.page.take().map(|page| Done {
            channel: page.channel,
            held: page.held,
            pages: page.pages,
        })
    }

    pub fn look(&self) -> Look {
        Look {
            loading: self.page.is_some(),
            exhausted: !self.relays.is_empty()
                && self.relays.iter().all(|relay| self.ended.contains(relay)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up(relays: &[&str]) -> HashSet<String> {
        relays.iter().map(|r| r.to_string()).collect()
    }

    #[test]
    fn an_empty_answer_ends_a_relay_and_a_short_one_does_not() {
        let mut history = History::new(["a".to_string(), "b".to_string()]);
        history.replayed("a", 0);
        assert!(!history.look().exhausted);
        history.replayed("b", 500);
        history.replayed("b", 0);
        assert!(
            !history.look().exhausted,
            "a reconnect's empty resume is not the relay's whole store"
        );
        assert!(history.start(Id::nil(), 0, &up(&["a", "b"]), Instant::now()));
        assert!(history.finished("b", 7).is_some());
        assert!(
            !history.look().exhausted,
            "a relay capping its limit sent a short page"
        );
        assert!(history.start(Id::nil(), 0, &up(&["a", "b"]), Instant::now()));
        let done = history.finished("b", 0).unwrap();
        assert_eq!(done.channel(), Id::nil());
        assert!(history.look().exhausted);
        assert!(
            !history.start(Id::nil(), 0, &up(&["a", "b"]), Instant::now()),
            "nobody left to ask"
        );
    }

    #[test]
    fn a_page_waits_for_every_connected_relay_or_its_patience() {
        let now = Instant::now();
        let mut history = History::new(["a".to_string(), "b".to_string(), "c".to_string()]);
        assert!(history.start(Id::nil(), 0, &up(&["a", "b"]), now));
        assert!(
            !history.start(Id::nil(), 0, &up(&["a", "b"]), now),
            "one page at a time"
        );
        assert!(history.look().loading);
        assert!(history.finished("a", PAGE as usize).is_none());
        assert!(history.overdue(now + PATIENCE / 2).is_none());
        assert!(history.overdue(now + PATIENCE).is_some());
        assert!(!history.look().loading);
        assert!(history.start(Id::nil(), 0, &up(&["a", "b"]), now));
        assert!(history.disconnected("a").is_none());
        assert!(
            history.disconnected("b").is_some(),
            "a relay that dropped is not waited for"
        );
    }

    #[test]
    fn pages_continue_only_while_the_conversation_gains_nothing() {
        let now = Instant::now();
        let relays = up(&["a"]);
        let mut history = History::new(["a".to_string()]);
        assert!(history.start(Id::nil(), 4, &relays, now));
        let done = history.finished("a", PAGE as usize).unwrap();
        assert!(
            !history.again(done, 9, &relays, now),
            "it reached the conversation"
        );
        assert!(history.start(Id::nil(), 9, &relays, now));
        let mut pages = 1;
        while let Some(done) = history.finished("a", PAGE as usize) {
            if !history.again(done, 9, &relays, now) {
                break;
            }
            pages += 1;
        }
        assert_eq!(pages, EMPTY_PAGES);
    }
}
