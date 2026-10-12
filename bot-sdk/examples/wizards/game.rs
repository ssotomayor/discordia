use rand::Rng;
use serde_json::{Value, json};

pub const MAX_HP: i32 = 100;
pub const MAX_PLAYERS: usize = 3;
const LOG_LINES: usize = 12;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Spell {
    Fireball,
    Freeze,
}

impl Spell {
    pub fn parse(s: &str) -> Option<Spell> {
        match s {
            "fireball" => Some(Spell::Fireball),
            "freeze" => Some(Spell::Freeze),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Spell::Fireball => "fireball",
            Spell::Freeze => "freeze",
        }
    }

    fn hit_chance(self) -> f64 {
        match self {
            Spell::Fireball => 0.4,
            Spell::Freeze => 0.6,
        }
    }

    fn damage(self, rng: &mut impl Rng) -> i32 {
        match self {
            Spell::Fireball => rng.gen_range(30..=40),
            Spell::Freeze => rng.gen_range(15..=22),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Lobby,
    Playing,
    Over,
}

#[derive(Clone, Debug)]
pub struct Wizard {
    pub key: String,
    pub hp: i32,
    pub initiative: u8,
}

/// One table. Every roll happens here, on the bot: a player's page that
/// rolled for itself could pick its own numbers.
#[derive(Debug)]
pub struct Game {
    pub phase: Phase,
    /// In turn order, highest initiative first.
    pub wizards: Vec<Wizard>,
    turn: usize,
    log: Vec<Value>,
    last: Option<Value>,
    winner: Option<String>,
    /// Bumped on every change, so a page animates each action exactly once.
    seq: u64,
}

impl Default for Game {
    fn default() -> Self {
        Game {
            phase: Phase::Lobby,
            wizards: Vec::new(),
            turn: 0,
            log: Vec::new(),
            last: None,
            winner: None,
            seq: 0,
        }
    }
}

impl Game {
    pub fn current(&self) -> Option<&str> {
        (self.phase == Phase::Playing).then(|| self.wizards[self.turn].key.as_str())
    }

    pub fn start(&mut self, players: &[String], rng: &mut impl Rng) -> Result<(), String> {
        if self.phase == Phase::Playing {
            return Err("a duel is already on".into());
        }
        if !(2..=MAX_PLAYERS).contains(&players.len()) {
            return Err(format!("a duel needs 2 to {MAX_PLAYERS} players"));
        }
        self.log.clear();
        self.last = None;
        self.winner = None;
        self.wizards = self.roll_initiative(players, rng);
        self.turn = 0;
        self.phase = Phase::Playing;
        self.seq += 1;
        Ok(())
    }

    /// Everyone rolls a d20; anyone sharing a number with someone else rolls
    /// again, until every number is different.
    fn roll_initiative(&mut self, players: &[String], rng: &mut impl Rng) -> Vec<Wizard> {
        let mut rolls: Vec<(String, u8)> = players
            .iter()
            .map(|k| (k.clone(), rng.gen_range(1..=20)))
            .collect();
        for (k, r) in &rolls {
            self.note(json!({ "k": "init", "who": k, "roll": r }));
        }
        loop {
            let tied: Vec<usize> = (0..rolls.len())
                .filter(|&i| rolls.iter().filter(|(_, r)| *r == rolls[i].1).count() > 1)
                .collect();
            if tied.is_empty() {
                break;
            }
            let who: Vec<&String> = tied.iter().map(|&i| &rolls[i].0).collect();
            self.note(json!({ "k": "tie", "who": who }));
            for i in tied {
                rolls[i].1 = rng.gen_range(1..=20);
                self.note(json!({ "k": "init", "who": rolls[i].0, "roll": rolls[i].1 }));
            }
        }
        rolls.sort_by_key(|r| std::cmp::Reverse(r.1));
        rolls
            .into_iter()
            .map(|(key, initiative)| Wizard {
                key,
                hp: MAX_HP,
                initiative,
            })
            .collect()
    }

    pub fn attack(
        &mut self,
        by: &str,
        spell: Spell,
        target: &str,
        rng: &mut impl Rng,
    ) -> Result<(), String> {
        if self.current() != Some(by) {
            return Err("it isn't your turn".into());
        }
        if target == by {
            return Err("a wizard can't target themself".into());
        }
        let t = self
            .wizards
            .iter()
            .position(|w| w.key == target && w.hp > 0)
            .ok_or("that wizard isn't standing")?;
        let hit = rng.gen_bool(spell.hit_chance());
        let damage = if hit { spell.damage(rng) } else { 0 };
        self.wizards[t].hp = (self.wizards[t].hp - damage).max(0);
        let action = json!({
            "k": if hit { "hit" } else { "miss" },
            "by": by,
            "target": target,
            "spell": spell.name(),
            "damage": damage,
        });
        self.last = Some(action.clone());
        self.note(action);
        if self.wizards[t].hp == 0 {
            self.note(json!({ "k": "down", "who": target }));
        }
        self.finish_turn();
        Ok(())
    }

    /// A turn nobody took in time passes to the next wizard.
    pub fn skip(&mut self) {
        if let Some(who) = self.current().map(str::to_string) {
            self.last = None;
            self.note(json!({ "k": "skip", "who": who }));
            self.finish_turn();
        }
    }

    /// Leaving mid-duel is losing it.
    pub fn forfeit(&mut self, key: &str) {
        if self.phase != Phase::Playing {
            return;
        }
        let Some(i) = self.wizards.iter().position(|w| w.key == key && w.hp > 0) else {
            return;
        };
        let theirs = self.turn == i;
        self.wizards[i].hp = 0;
        self.last = None;
        self.note(json!({ "k": "left", "who": key }));
        if theirs {
            self.finish_turn();
        } else {
            self.check_over();
        }
    }

    fn finish_turn(&mut self) {
        self.seq += 1;
        if self.check_over() {
            return;
        }
        let n = self.wizards.len();
        for step in 1..=n {
            let next = (self.turn + step) % n;
            if self.wizards[next].hp > 0 {
                self.turn = next;
                return;
            }
        }
    }

    fn check_over(&mut self) -> bool {
        let standing: Vec<&Wizard> = self.wizards.iter().filter(|w| w.hp > 0).collect();
        if standing.len() > 1 {
            return false;
        }
        self.winner = standing.first().map(|w| w.key.clone());
        self.phase = Phase::Over;
        self.seq += 1;
        if let Some(w) = self.winner.clone() {
            self.note(json!({ "k": "win", "who": w }));
        }
        true
    }

    fn note(&mut self, line: Value) {
        self.log.push(line);
        if self.log.len() > LOG_LINES {
            self.log.remove(0);
        }
    }

    pub fn state(&self) -> Value {
        json!({
            "t": "state",
            "seq": self.seq,
            "phase": match self.phase {
                Phase::Lobby => "lobby",
                Phase::Playing => "playing",
                Phase::Over => "over",
            },
            "wizards": self.wizards.iter().map(|w| json!({
                "key": w.key, "hp": w.hp, "initiative": w.initiative,
            })).collect::<Vec<_>>(),
            "turn": self.current(),
            "log": self.log,
            "last": self.last,
            "winner": self.winner,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn keys(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("w{i}")).collect()
    }

    #[test]
    fn initiative_orders_everyone_and_leaves_no_ties() {
        for seed in 0..200 {
            let mut g = Game::default();
            g.start(&keys(3), &mut StdRng::seed_from_u64(seed)).unwrap();
            let rolls: Vec<u8> = g.wizards.iter().map(|w| w.initiative).collect();
            assert!(rolls.windows(2).all(|p| p[0] > p[1]), "{rolls:?}");
            assert!(rolls.iter().all(|r| (1..=20).contains(r)));
            assert_eq!(g.current(), Some(g.wizards[0].key.as_str()));
        }
    }

    #[test]
    fn a_duel_needs_two_or_three() {
        let rng = &mut StdRng::seed_from_u64(1);
        assert!(Game::default().start(&keys(1), rng).is_err());
        assert!(Game::default().start(&keys(4), rng).is_err());
    }

    #[test]
    fn only_the_wizard_whose_turn_it_is_may_cast_and_never_at_themself() {
        let rng = &mut StdRng::seed_from_u64(7);
        let mut g = Game::default();
        g.start(&keys(2), rng).unwrap();
        let (first, second) = (g.wizards[0].key.clone(), g.wizards[1].key.clone());
        assert!(g.attack(&second, Spell::Freeze, &first, rng).is_err());
        assert!(g.attack(&first, Spell::Freeze, &first, rng).is_err());
        g.attack(&first, Spell::Freeze, &second, rng).unwrap();
        assert_eq!(g.current(), Some(second.as_str()));
    }

    #[test]
    fn hit_rates_and_damage_follow_the_spell() {
        let rng = &mut StdRng::seed_from_u64(42);
        for (spell, chance, range) in [
            (Spell::Fireball, 0.4, 30..=40),
            (Spell::Freeze, 0.6, 15..=22),
        ] {
            let mut hits = 0;
            for _ in 0..4000 {
                let mut g = Game::default();
                g.start(&keys(2), rng).unwrap();
                let (a, b) = (g.wizards[0].key.clone(), g.wizards[1].key.clone());
                g.attack(&a, spell, &b, rng).unwrap();
                let dealt = MAX_HP - g.wizards[1].hp;
                if dealt > 0 {
                    hits += 1;
                    assert!(range.contains(&dealt), "{spell:?} dealt {dealt}");
                }
            }
            let rate = hits as f64 / 4000.0;
            assert!((rate - chance).abs() < 0.03, "{spell:?} hit {rate}");
        }
    }

    #[test]
    fn the_last_wizard_standing_wins_and_the_fallen_are_skipped() {
        let rng = &mut StdRng::seed_from_u64(3);
        let mut g = Game::default();
        g.start(&keys(3), rng).unwrap();
        let order: Vec<String> = g.wizards.iter().map(|w| w.key.clone()).collect();
        g.wizards[1].hp = 0;
        g.attack(&order[0], Spell::Freeze, &order[2], rng).unwrap();
        assert_eq!(
            g.current(),
            Some(order[2].as_str()),
            "a fallen wizard's turn is skipped"
        );
        while g.phase == Phase::Playing {
            let me = g.current().unwrap().to_string();
            let foe = order
                .iter()
                .find(|k| **k != me && g.wizards.iter().any(|w| &w.key == *k && w.hp > 0))
                .unwrap()
                .clone();
            g.attack(&me, Spell::Fireball, &foe, rng).unwrap();
        }
        let standing: Vec<_> = g.wizards.iter().filter(|w| w.hp > 0).collect();
        assert_eq!(standing.len(), 1);
        assert_eq!(g.state()["winner"], json!(standing[0].key));
        assert!(
            g.start(&keys(2), rng).is_ok(),
            "a rematch starts from the end"
        );
    }

    #[test]
    fn leaving_or_stalling_moves_the_duel_on() {
        let rng = &mut StdRng::seed_from_u64(9);
        let mut g = Game::default();
        g.start(&keys(3), rng).unwrap();
        let order: Vec<String> = g.wizards.iter().map(|w| w.key.clone()).collect();
        g.skip();
        assert_eq!(g.current(), Some(order[1].as_str()));
        g.forfeit(&order[1]);
        assert_eq!(
            g.current(),
            Some(order[2].as_str()),
            "the leaver's turn passes on"
        );
        g.forfeit(&order[0]);
        assert_eq!(g.phase, Phase::Over);
        assert_eq!(g.state()["winner"], json!(order[2]));
    }
}
