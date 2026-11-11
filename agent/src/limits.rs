use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use discordia_protocol::Id;

use crate::config::Limits;

const HOUR: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Refusal {
    MemberRate,
    DailyBudget,
    GuildBudget,
    Busy,
}

impl Refusal {
    pub fn notice(self) -> &'static str {
        match self {
            Self::MemberRate => "You've asked me a lot this hour. Try again later.",
            Self::DailyBudget => "I've used up today's budget. I'll be back tomorrow (UTC).",
            Self::GuildBudget => {
                "This guild has used today's share of my budget. I'll be back tomorrow (UTC)."
            }
            Self::Busy => "I'm answering a few others right now. Ask again in a moment.",
        }
    }
}

/// Decides before any provider call. `notify` is true once per member and
/// reason until they are let through again, so refusals cannot flood a channel.
/// A clone is the same ledger: it outlives a connection, so a reconnect spends
/// from what is left (trap 39).
#[derive(Clone)]
pub struct Ledger(Arc<Mutex<Book>>);

struct Book {
    limits: Limits,
    asked: HashMap<String, VecDeque<Instant>>,
    noticed: HashSet<(String, Refusal)>,
    day: Option<NaiveDate>,
    spent: u64,
    guild_spent: HashMap<Id, u64>,
    in_flight: usize,
}

/// One admitted reply, holding its place under `Limits::in_flight` until it is
/// spent or dropped, so no path can forget to give it back or give it back twice.
#[must_use = "dropping a slot frees it at once"]
pub struct Slot(Arc<Mutex<Book>>);

fn lock(book: &Mutex<Book>) -> MutexGuard<'_, Book> {
    book.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Book {
    fn roll(&mut self, today: NaiveDate) {
        if self.day != Some(today) {
            self.day = Some(today);
            self.spent = 0;
            self.guild_spent.clear();
            self.noticed
                .retain(|(_, r)| !matches!(r, Refusal::DailyBudget | Refusal::GuildBudget));
        }
    }
}

impl Ledger {
    pub fn new(limits: Limits) -> Self {
        Self(Arc::new(Mutex::new(Book {
            limits,
            asked: HashMap::new(),
            noticed: HashSet::new(),
            day: None,
            spent: 0,
            guild_spent: HashMap::new(),
            in_flight: 0,
        })))
    }

    pub fn admit(
        &self,
        member: &str,
        guild: Option<Id>,
        now: Instant,
        today: NaiveDate,
    ) -> Result<Slot, (Refusal, bool)> {
        let mut book = lock(&self.0);
        let book = &mut *book;
        book.roll(today);
        let times = book.asked.entry(member.to_string()).or_default();
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t) >= HOUR)
        {
            times.pop_front();
        }
        let guild_spent = guild
            .and_then(|g| book.guild_spent.get(&g))
            .copied()
            .unwrap_or(0);
        let refusal = if book.spent >= book.limits.daily_tokens {
            Some(Refusal::DailyBudget)
        } else if guild.is_some()
            && book
                .limits
                .guild_daily_tokens
                .is_some_and(|cap| guild_spent >= cap)
        {
            Some(Refusal::GuildBudget)
        } else if times.len() >= book.limits.per_member_per_hour as usize {
            Some(Refusal::MemberRate)
        } else if book.in_flight >= book.limits.in_flight {
            Some(Refusal::Busy)
        } else {
            None
        };
        match refusal {
            Some(r) => Err((r, book.noticed.insert((member.to_string(), r)))),
            None => {
                times.push_back(now);
                book.in_flight += 1;
                book.noticed.retain(|(m, _)| m != member);
                Ok(Slot(self.0.clone()))
            }
        }
    }

    #[cfg(test)]
    fn in_flight(&self) -> usize {
        lock(&self.0).in_flight
    }
}

impl Slot {
    /// Charges what the reply cost, then frees the slot.
    pub fn finish(self, tokens: u64, guild: Option<Id>, today: NaiveDate) {
        let mut book = lock(&self.0);
        book.roll(today);
        book.spent = book.spent.saturating_add(tokens);
        if let Some(g) = guild {
            let spent = book.guild_spent.entry(g).or_default();
            *spent = spent.saturating_add(tokens);
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        lock(&self.0).in_flight -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
    }

    fn limits() -> Limits {
        Limits {
            per_member_per_hour: 2,
            daily_tokens: 100,
            in_flight: 2,
            ..Limits::default()
        }
    }

    #[test]
    fn a_member_is_held_to_the_hourly_count_and_told_once() {
        let l = Ledger::new(limits());
        let t = Instant::now();
        l.admit("a", None, t, day(1))
            .unwrap()
            .finish(1, None, day(1));
        l.admit("a", None, t, day(1))
            .unwrap()
            .finish(1, None, day(1));
        assert_eq!(
            l.admit("a", None, t, day(1)).err(),
            Some((Refusal::MemberRate, true))
        );
        assert_eq!(
            l.admit("a", None, t, day(1)).err(),
            Some((Refusal::MemberRate, false))
        );
        let b = l.admit("b", None, t, day(1));
        assert!(b.is_ok(), "others are unaffected");
        drop(b);
        assert!(
            l.admit("a", None, t + HOUR, day(1)).is_ok(),
            "an hour later"
        );
    }

    #[test]
    fn the_daily_budget_resets_with_the_date() {
        let l = Ledger::new(limits());
        let t = Instant::now();
        l.admit("a", None, t, day(1))
            .unwrap()
            .finish(150, None, day(1));
        assert_eq!(
            l.admit("b", None, t, day(1)).err(),
            Some((Refusal::DailyBudget, true))
        );
        assert!(l.admit("b", None, t, day(2)).is_ok());
    }

    #[test]
    fn one_guild_spends_only_its_share() {
        let l = Ledger::new(Limits {
            guild_daily_tokens: Some(50),
            ..limits()
        });
        let t = Instant::now();
        let (a, b) = (Id::new_v4(), Id::new_v4());
        l.admit("x", Some(a), t, day(1))
            .unwrap()
            .finish(60, Some(a), day(1));
        assert_eq!(
            l.admit("y", Some(a), t, day(1)).err(),
            Some((Refusal::GuildBudget, true))
        );
        let other = l.admit("y", Some(b), t, day(1));
        assert!(other.is_ok(), "another guild is unaffected");
        drop(other);
        assert!(
            l.admit("y", Some(a), t, day(2)).is_ok(),
            "the next day starts over"
        );
    }

    #[test]
    fn too_many_at_once_is_busy() {
        let l = Ledger::new(limits());
        let t = Instant::now();
        let first = l.admit("a", None, t, day(1)).unwrap();
        let _second = l.admit("b", None, t, day(1)).unwrap();
        assert_eq!(
            l.admit("c", None, t, day(1)).err(),
            Some((Refusal::Busy, true))
        );
        first.finish(0, None, day(1));
        assert!(l.admit("c", None, t, day(1)).is_ok());
    }

    #[test]
    fn a_slot_comes_back_once_however_its_reply_ends() {
        let l = Ledger::new(limits());
        let t = Instant::now();
        let spent = l.admit("a", None, t, day(1)).unwrap();
        let dropped = l.admit("b", None, t, day(1)).unwrap();
        assert_eq!(l.in_flight(), 2);
        spent.finish(10, None, day(1));
        assert_eq!(l.in_flight(), 1);
        drop(dropped);
        assert_eq!(
            l.in_flight(),
            0,
            "a reply lost with its task frees its slot"
        );
        let panicked = l.admit("c", None, t, day(1)).unwrap();
        let task = std::thread::spawn(move || {
            let _held = panicked;
            panic!("provider task failed");
        });
        assert!(task.join().is_err());
        assert_eq!(l.in_flight(), 0);
    }

    #[test]
    fn a_clone_is_the_same_ledger() {
        let l = Ledger::new(limits());
        let t = Instant::now();
        let again = l.clone();
        l.admit("a", None, t, day(1))
            .unwrap()
            .finish(1, None, day(1));
        l.admit("a", None, t, day(1))
            .unwrap()
            .finish(1, None, day(1));
        assert_eq!(
            again.admit("a", None, t, day(1)).err(),
            Some((Refusal::MemberRate, true)),
            "what one connection counted binds the next"
        );
    }
}
