use std::collections::{HashMap, HashSet, VecDeque};
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
pub struct Ledger {
    limits: Limits,
    asked: HashMap<String, VecDeque<Instant>>,
    noticed: HashSet<(String, Refusal)>,
    day: Option<NaiveDate>,
    spent: u64,
    guild_spent: HashMap<Id, u64>,
    in_flight: usize,
}

impl Ledger {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            asked: HashMap::new(),
            noticed: HashSet::new(),
            day: None,
            spent: 0,
            guild_spent: HashMap::new(),
            in_flight: 0,
        }
    }

    fn roll(&mut self, today: NaiveDate) {
        if self.day != Some(today) {
            self.day = Some(today);
            self.spent = 0;
            self.guild_spent.clear();
            self.noticed
                .retain(|(_, r)| !matches!(r, Refusal::DailyBudget | Refusal::GuildBudget));
        }
    }

    pub fn admit(
        &mut self,
        member: &str,
        guild: Option<Id>,
        now: Instant,
        today: NaiveDate,
    ) -> Result<(), (Refusal, bool)> {
        self.roll(today);
        let times = self.asked.entry(member.to_string()).or_default();
        while times
            .front()
            .is_some_and(|t| now.duration_since(*t) >= HOUR)
        {
            times.pop_front();
        }
        let guild_spent = guild
            .and_then(|g| self.guild_spent.get(&g))
            .copied()
            .unwrap_or(0);
        let refusal = if self.spent >= self.limits.daily_tokens {
            Some(Refusal::DailyBudget)
        } else if guild.is_some()
            && self
                .limits
                .guild_daily_tokens
                .is_some_and(|cap| guild_spent >= cap)
        {
            Some(Refusal::GuildBudget)
        } else if times.len() >= self.limits.per_member_per_hour as usize {
            Some(Refusal::MemberRate)
        } else if self.in_flight >= self.limits.in_flight {
            Some(Refusal::Busy)
        } else {
            None
        };
        match refusal {
            Some(r) => Err((r, self.noticed.insert((member.to_string(), r)))),
            None => {
                times.push_back(now);
                self.in_flight += 1;
                self.noticed.retain(|(m, _)| m != member);
                Ok(())
            }
        }
    }

    pub fn finish(&mut self, tokens: u64, guild: Option<Id>, today: NaiveDate) {
        self.in_flight = self.in_flight.saturating_sub(1);
        self.roll(today);
        self.spent = self.spent.saturating_add(tokens);
        if let Some(g) = guild {
            let spent = self.guild_spent.entry(g).or_default();
            *spent = spent.saturating_add(tokens);
        }
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
        let mut l = Ledger::new(limits());
        let t = Instant::now();
        assert!(l.admit("a", None, t, day(1)).is_ok());
        l.finish(1, None, day(1));
        assert!(l.admit("a", None, t, day(1)).is_ok());
        l.finish(1, None, day(1));
        assert_eq!(
            l.admit("a", None, t, day(1)),
            Err((Refusal::MemberRate, true))
        );
        assert_eq!(
            l.admit("a", None, t, day(1)),
            Err((Refusal::MemberRate, false))
        );
        assert!(
            l.admit("b", None, t, day(1)).is_ok(),
            "others are unaffected"
        );
        l.finish(1, None, day(1));
        assert!(
            l.admit("a", None, t + HOUR, day(1)).is_ok(),
            "an hour later"
        );
    }

    #[test]
    fn the_daily_budget_resets_with_the_date() {
        let mut l = Ledger::new(limits());
        let t = Instant::now();
        l.admit("a", None, t, day(1)).unwrap();
        l.finish(150, None, day(1));
        assert_eq!(
            l.admit("b", None, t, day(1)),
            Err((Refusal::DailyBudget, true))
        );
        assert!(l.admit("b", None, t, day(2)).is_ok());
    }

    #[test]
    fn one_guild_spends_only_its_share() {
        let mut l = Ledger::new(Limits {
            guild_daily_tokens: Some(50),
            ..limits()
        });
        let t = Instant::now();
        let (a, b) = (Id::new_v4(), Id::new_v4());
        l.admit("x", Some(a), t, day(1)).unwrap();
        l.finish(60, Some(a), day(1));
        assert_eq!(
            l.admit("y", Some(a), t, day(1)),
            Err((Refusal::GuildBudget, true))
        );
        assert!(
            l.admit("y", Some(b), t, day(1)).is_ok(),
            "another guild is unaffected"
        );
        l.finish(0, Some(b), day(1));
        assert!(
            l.admit("y", Some(a), t, day(2)).is_ok(),
            "the next day starts over"
        );
    }

    #[test]
    fn too_many_at_once_is_busy() {
        let mut l = Ledger::new(limits());
        let t = Instant::now();
        l.admit("a", None, t, day(1)).unwrap();
        l.admit("b", None, t, day(1)).unwrap();
        assert_eq!(l.admit("c", None, t, day(1)), Err((Refusal::Busy, true)));
        l.finish(0, None, day(1));
        assert!(l.admit("c", None, t, day(1)).is_ok());
    }
}
