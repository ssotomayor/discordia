//! What this machine has earned, per server, so a total can span them.
//!
//! A server knows only its own guilds, so nothing on the network can add up a
//! figure that crosses servers — only the person who visits them all can, and
//! this is where they keep the running tally between visits.
//!
//! Keyed by dial origin (`host:port`, or `quic:<key>`), which is the same
//! string the login signature covers. Two spellings of one server therefore
//! count once, and a self-host on this machine counts as the local address it
//! answers on.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::identity::config_dir;

const FILE_VERSION: u32 = 1;

/// A cap on the file, not on anyone's play: a client that visited thousands of
/// servers should not carry all of them forever, and the smallest totals are
/// the ones a sum misses least.
const MAX_SERVERS: usize = 256;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ledger {
    /// Ordered so the file is stable across writes and a diff means something.
    #[serde(default)]
    pub by_server: BTreeMap<String, u64>,
}

impl Ledger {
    /// The sum every server contributed to, and how many did. Servers with
    /// nothing earned are not counted: joining a place is not playing in it.
    pub fn total(&self) -> crate::nostr::xp::GlobalXp {
        let earning = self.by_server.values().filter(|xp| **xp > 0);
        crate::nostr::xp::GlobalXp {
            xp: earning.clone().copied().fold(0, u64::saturating_add),
            servers: earning.count() as u32,
        }
    }

    /// `true` when the number moved, so a caller knows whether to write and to
    /// republish. A server's figure is replaced, never added to: it is the
    /// authority on its own total and we are only caching it.
    pub fn record(&mut self, origin: &str, xp: u64) -> bool {
        if self.by_server.get(origin) == Some(&xp) {
            return false;
        }
        self.by_server.insert(origin.to_string(), xp);
        if self.by_server.len() > MAX_SERVERS
            && let Some(smallest) = self
                .by_server
                .iter()
                .filter(|(k, _)| k.as_str() != origin)
                .min_by_key(|(k, v)| (**v, (*k).clone()))
                .map(|(k, _)| k.clone())
        {
            self.by_server.remove(&smallest);
        }
        true
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Stored {
    version: u32,
    ledger: Ledger,
}

fn ledger_path(owner: &str) -> Result<std::path::PathBuf, String> {
    if !crate::protocol::is_canonical_pubkey(owner) {
        return Err("invalid ledger owner".into());
    }
    // The legacy xp.json has no owner, so assigning it would mix accounts again.
    Ok(config_dir().join("xp").join(format!("{owner}.json")))
}

pub fn save(owner: &str, ledger: &Ledger) -> Result<(), String> {
    let path = ledger_path(owner)?;
    let stored = Stored {
        version: FILE_VERSION,
        ledger: ledger.clone(),
    };
    let content =
        serde_json::to_string_pretty(&stored).map_err(|e| format!("serialize ledger: {e}"))?;
    crate::identity::write_private(&path, &content)
}

pub fn load(owner: &str) -> Ledger {
    let Ok(path) = ledger_path(owner) else {
        return Ledger::default();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Ledger::default();
    };
    match serde_json::from_str::<Stored>(&content) {
        Ok(stored) if stored.version == FILE_VERSION => stored.ledger,
        _ => Ledger::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_ledgers_are_isolated_and_legacy_totals_are_not_assigned() {
        let root = crate::identity::test_config_dir();
        let alice = "11".repeat(32);
        let bob = "22".repeat(32);
        let legacy = r#"{"version":1,"ledger":{"by_server":{"old.example:443":900}}}"#;
        std::fs::write(root.join("xp.json"), legacy).unwrap();
        assert_eq!(load(&alice), Ledger::default());
        let mut first = Ledger::default();
        first.record("first.example:443", 100);
        save(&alice, &first).unwrap();
        assert_eq!(load(&bob), Ledger::default());
        let mut second = Ledger::default();
        second.record("second.example:443", 30);
        save(&bob, &second).unwrap();
        assert_eq!(load(&alice), first);
        assert_eq!(load(&bob), second);
        assert_eq!(
            std::fs::read_to_string(root.join("xp.json")).unwrap(),
            legacy
        );
        assert!(save("../other", &first).is_err());
        crate::identity::TEST_CONFIG_DIR.with(|dir| *dir.borrow_mut() = None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn large_server_totals_saturate_without_losing_the_server_count() {
        let mut ledger = Ledger::default();
        ledger.record("first.example:443", u64::MAX);
        ledger.record("second.example:443", 1);
        assert_eq!(ledger.total().xp, u64::MAX);
        assert_eq!(ledger.total().servers, 2);
    }

    #[test]
    fn a_total_spans_servers_and_counts_only_the_earning_ones() {
        let mut l = Ledger::default();
        l.record("a.example:443", 100);
        l.record("quic:abc", 50);
        l.record("visited.example:443", 0);

        let total = l.total();
        assert_eq!(total.xp, 150);
        assert_eq!(total.servers, 2, "a server with nothing earned is not one");
    }

    #[test]
    fn a_servers_figure_is_replaced_rather_than_accumulated() {
        let mut l = Ledger::default();
        assert!(l.record("a.example:443", 10));
        assert!(l.record("a.example:443", 40));
        assert_eq!(l.total().xp, 40, "40 is the server's total, not 10 plus 40");
    }

    #[test]
    fn recording_what_is_already_held_reports_no_change() {
        let mut l = Ledger::default();
        assert!(l.record("a.example:443", 10));
        assert!(!l.record("a.example:443", 10));
    }

    #[test]
    fn the_file_stays_bounded_by_dropping_the_least_earned() {
        let mut l = Ledger::default();
        for n in 0..MAX_SERVERS {
            l.record(&format!("s{n:04}.example:443"), (n as u64) + 1);
        }
        assert_eq!(l.by_server.len(), MAX_SERVERS);

        l.record("newcomer.example:443", 5_000);
        assert_eq!(l.by_server.len(), MAX_SERVERS);
        assert!(l.by_server.contains_key("newcomer.example:443"));
        assert!(
            !l.by_server.contains_key("s0000.example:443"),
            "the smallest total is what makes room"
        );
    }

    #[test]
    fn the_server_just_recorded_is_never_the_one_evicted() {
        let mut l = Ledger::default();
        for n in 0..MAX_SERVERS {
            l.record(&format!("s{n:04}.example:443"), (n as u64) + 10);
        }
        l.record("tiny.example:443", 1);
        assert!(l.by_server.contains_key("tiny.example:443"));
        assert_eq!(l.by_server.len(), MAX_SERVERS);
    }
}
