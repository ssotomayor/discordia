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
        0 | 1 => format!("Lv {level}"),
        n => format!("Lv {level} · {n} servers"),
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
}
