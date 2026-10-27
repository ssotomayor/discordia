//! The seam between a server's count and the number published to relays.
//!
//! A server is the authority on what happened on it; the ledger on this disk is
//! the only place those authorities are ever added together, and the relay copy
//! is a claim its owner signs. Nothing downstream may treat it as more.

use dioxus::prelude::*;

use crate::nostr::service::{NostrCmd, NostrTx};
use crate::state::use_app_state;

/// Mounted inside a session. Watches what this server says we have earned,
/// files it under this server, and asks for a republish when the sum moves.
#[component]
pub fn XpLedgerService() -> Element {
    let state = use_app_state();
    let nostr = use_context::<NostrTx>();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();

    let mut ledger = use_signal(crate::xp_ledger::load);

    use_effect(move || {
        let publish = settings.read().publish_global_level;
        let (origin, earned) = {
            let s = state.read();
            (s.server_origin.clone(), s.my_server_xp())
        };
        let Some(origin) = origin else { return };

        // The ledger is kept either way: turning publishing back on should not
        // start the count over, and the number is ours before it is anyone's.
        let moved = ledger.write().record(&origin, earned);
        if moved && let Err(e) = crate::xp_ledger::save(&ledger.read()) {
            crate::dlog!("[xp] ledger not saved: {e}");
        }
        if publish {
            nostr.send(NostrCmd::PublishXp(ledger.read().total()));
        }
    });

    rsx! { Fragment {} }
}

/// `Lv 12 · 3 servers`, or `Lv 12` when it all came from one. The caller is
/// responsible for saying who is claiming it.
pub fn global_label(total: crate::nostr::xp::GlobalXp) -> String {
    let level = crate::protocol::level_progress(total.xp).0;
    match total.servers {
        0 | 1 => crate::i18n::text("Lv {0}", &[level.to_string()]),
        n => crate::i18n::text("Lv {0} · {1} servers", &[level.to_string(), n.to_string()]),
    }
}

/// Where a member stands on this guild's bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub label: String,
    pub color: Option<String>,
    pub xp: u64,
    /// The total at which the next step begins; `None` past the last rank.
    pub target: Option<u64>,
    pub next: Option<String>,
    pub pct: u32,
}

/// Measured toward the next rank when the guild names them: a bar on the
/// built-in levels beside a rank's name reads as progress toward a rank it isn't.
pub fn progress(rules: &crate::protocol::Leveling, xp: u64) -> Progress {
    let (level, into, span) = crate::protocol::level_progress(xp);
    let lv = |n: u32| crate::i18n::text("Lv {0}", &[n.to_string()]);
    let pct = |done: u64, of: u64| (done as f64 / of.max(1) as f64 * 100.0) as u32;
    if !rules.enabled || rules.tiers.is_empty() {
        return Progress {
            label: lv(level),
            color: None,
            xp,
            target: Some(xp - into + span),
            next: Some(lv(level + 1)),
            pct: pct(into, span),
        };
    }
    let current = rules.tier_at(xp);
    let next = rules.tiers.iter().find(|t| t.xp > xp);
    let base = current.map_or(0, |t| t.xp);
    Progress {
        label: current.map_or_else(|| lv(level), |t| t.name.clone()),
        color: current.and_then(|t| t.color.clone()),
        xp,
        target: next.map(|t| t.xp),
        next: next.map(|t| t.name.clone()),
        pct: next.map_or(100, |t| pct(xp - base, t.xp - base)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nostr::xp::GlobalXp;

    #[test]
    fn one_server_is_not_worth_saying() {
        assert_eq!(global_label(GlobalXp { xp: 30, servers: 1 }), "Lv 3");
        assert_eq!(global_label(GlobalXp { xp: 0, servers: 0 }), "Lv 1");
    }

    #[test]
    fn a_spread_total_says_how_far_it_spread() {
        assert_eq!(
            global_label(GlobalXp {
                xp: 450,
                servers: 3
            }),
            "Lv 10 · 3 servers"
        );
    }

    fn ranks(enabled: bool) -> crate::protocol::Leveling {
        let tier = |xp, name: &str| crate::protocol::LevelTier {
            xp,
            name: name.into(),
            color: None,
            emoji: crate::protocol::DEFAULT_TIER_EMOJI.into(),
        };
        crate::protocol::Leveling {
            enabled,
            tiers: vec![tier(0, "Novato"), tier(50, "Soldado"), tier(100, "Cabo")],
            ..Default::default()
        }
    }

    #[test]
    fn a_ranked_bar_runs_to_the_next_rank() {
        let p = progress(&ranks(true), 34);
        assert_eq!(p.label, "Novato");
        assert_eq!((p.xp, p.target), (34, Some(50)));
        assert_eq!(p.next.as_deref(), Some("Soldado"));
        assert_eq!(p.pct, 68);

        let p = progress(&ranks(true), 75);
        assert_eq!(
            (p.label.as_str(), p.target, p.pct),
            ("Soldado", Some(100), 50)
        );
    }

    #[test]
    fn the_last_rank_has_nowhere_to_go() {
        let p = progress(&ranks(true), 400);
        assert_eq!(p.label, "Cabo");
        assert_eq!((p.target, p.next, p.pct), (None, None, 100));
    }

    #[test]
    fn without_ranks_the_bar_is_the_built_in_level_in_total_xp() {
        let p = progress(&ranks(false), 34);
        assert_eq!(p.label, "Lv 3");
        assert_eq!((p.xp, p.target), (34, Some(60)));
        assert_eq!(p.next.as_deref(), Some("Lv 4"));
        assert_eq!(p.pct, 13);
    }
}
