//! `push-peers.json`: who may alert from a cold process, and as what name.
//! A notification cache only; nothing in the UI reads it (trap 8).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::state::AppState;

pub const PEERS_FILE: &str = "push-peers.json";
/// Push wakes only Android; elsewhere the file would be names on disk for nothing.
const WRITES: bool = cfg!(target_os = "android");
const DEBOUNCE: Duration = Duration::from_secs(2);

/// Contacts and guild-mates are Inbox only in memory (`AppState::dm_standing`),
/// so a woken process learns them from here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushPeers {
    pub account: String,
    pub peers: Vec<Peer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Peer {
    pub pubkey: String,
    pub name: String,
}

impl PushPeers {
    pub fn name(&self, pubkey: &str) -> Option<&str> {
        self.peers
            .binary_search_by(|peer| peer.pubkey.as_str().cmp(pubkey))
            .ok()
            .and_then(|at| self.peers.get(at))
            .map(|peer| peer.name.as_str())
    }
}

/// Everyone `dm_standing` puts in the Inbox, named as the app would draw them.
/// One pass over the roster: `display_name` per key is quadratic in a big guild.
pub(crate) fn build(state: &AppState, account: &str) -> PushPeers {
    let mut roster: HashMap<&str, (&str, bool)> = HashMap::new();
    let mut mates = BTreeSet::new();
    for member in &state.members {
        let key = member.user.pubkey.as_str();
        let here = Some(member.guild_id) == state.selected_guild;
        let name = member.user.username.as_str();
        roster
            .entry(key)
            .and_modify(|held| {
                if here && !held.1 {
                    *held = (name, true);
                }
            })
            .or_insert((name, here));
        if !member.bot {
            mates.insert(key);
        }
    }
    let candidates: BTreeSet<&str> = state
        .accepted_dms()
        .chain(state.contacts.contacts.iter().map(|c| c.pubkey.as_str()))
        .chain(mates)
        .collect();
    let peers = candidates
        .into_iter()
        .filter(|key| {
            *key != account
                && crate::protocol::is_canonical_pubkey(key)
                && state.dm_standing(key) == crate::state::DmStanding::Inbox
        })
        .map(|key| {
            let name = roster
                .get(key)
                .map(|(name, _)| *name)
                .or_else(|| state.contacts.petname(key))
                .or_else(|| state.nostr_names.get(key).map(|(name, _)| name.as_str()))
                .map(crate::push::clean_name)
                .unwrap_or_else(|| crate::identity::truncate_pubkey(key));
            Peer {
                pubkey: key.to_owned(),
                name,
            }
        })
        .collect();
    PushPeers {
        account: account.to_owned(),
        peers,
    }
}

/// Another account's cache, or none, reads as empty.
pub fn read(dir: &Path, account: &str) -> PushPeers {
    let mut file: PushPeers = std::fs::read_to_string(dir.join(PEERS_FILE))
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default();
    if file.account != account {
        return PushPeers::default();
    }
    file.peers.sort_by(|a, b| a.pubkey.cmp(&b.pubkey));
    file
}

pub fn write(dir: &Path, peers: &PushPeers) -> Result<(), String> {
    let content = serde_json::to_string(peers).map_err(|e| e.to_string())?;
    crate::identity::write_private(&dir.join(PEERS_FILE), &content)
}

/// Rewrites the cache whole, once a burst of state changes has settled.
pub(crate) fn use_push_peers(state: dioxus::prelude::Signal<AppState>, account: String) {
    use dioxus::prelude::*;
    let peers = use_memo(move || WRITES.then(|| build(&state.read(), &account)));
    let mut pending = use_signal(|| None::<dioxus::core::Task>);
    use_effect(move || {
        let Some(peers) = peers() else {
            return;
        };
        if let Some(task) = pending.write().take() {
            task.cancel();
        }
        pending.set(Some(spawn(async move {
            tokio::time::sleep(DEBOUNCE).await;
            if let Err(error) = write(&crate::identity::config_dir(), &peers) {
                tracing::warn!(%error, "Could not save the push peer cache");
            }
        })));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Member;

    fn key(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    fn member(pubkey: &str, name: &str, guild: crate::protocol::Id, bot: bool) -> Member {
        serde_json::from_value(serde_json::json!({
            "user": {"pubkey": pubkey, "username": name},
            "guild_id": guild,
            "online": true,
            "bot": bot,
        }))
        .unwrap()
    }

    #[test]
    fn the_cache_holds_the_inbox_and_its_names() {
        let me = key(1);
        let mut state = AppState::empty();
        let (here, there) = (crate::protocol::Id::new_v4(), crate::protocol::Id::new_v4());
        state.selected_guild = Some(here);
        state.choose_dm(&key(2), Some(crate::state::DmChoice::Accepted));
        state.choose_dm(&me, Some(crate::state::DmChoice::Accepted));
        state.choose_dm("not a key", Some(crate::state::DmChoice::Accepted));
        state.choose_dm(&key(7), Some(crate::state::DmChoice::Ignored));
        state.contacts = state.contacts.clone().with(crate::nostr::nip02::Contact {
            pubkey: key(3),
            relay: None,
            petname: Some("pet\u{7}name".into()),
        });
        state.members = vec![
            member(&key(4), "elsewhere", there, false),
            member(&key(4), "here", here, false),
            member(&key(5), "robot", here, true),
            member(&key(6), "blocked", here, false),
        ];
        state.mutes = crate::nostr::nip51::MuteList::from_pubkeys([key(6)]);
        state
            .nostr_names
            .insert(key(2), ("published".into(), 1_700_000_000));

        let built = build(&state, &me);
        let keys: Vec<_> = built.peers.iter().map(|p| p.pubkey.clone()).collect();
        assert_eq!(keys, vec![key(2), key(3), key(4)]);
        assert_eq!(built.name(&key(2)), Some("published"));
        assert_eq!(built.name(&key(3)), Some("petname"));
        assert_eq!(
            built.name(&key(4)),
            Some("here"),
            "the open guild's name wins"
        );
        assert_eq!(built.name(&key(5)), None, "a bot introduces nobody");
        assert_eq!(built.name(&key(6)), None);
        assert_eq!(built.name(&key(7)), None);
        for peer in &built.peers {
            assert_eq!(
                state.dm_standing(&peer.pubkey),
                crate::state::DmStanding::Inbox
            );
        }

        // What a woken process decides from the files must be what the app decides.
        let (accepted, ignored) = state.dm_choice_lists();
        let blocked: Vec<String> = state.mutes.pubkeys().map(str::to_string).collect();
        for peer in (2..=8).map(key) {
            let cold = crate::dm_standing::standing(
                blocked.contains(&peer),
                crate::dm_standing::choice_in(&accepted, &ignored, &peer),
                built.name(&peer).is_some(),
            );
            assert_eq!(
                cold == crate::state::DmStanding::Inbox,
                state.dm_standing(&peer) == crate::state::DmStanding::Inbox,
                "{peer}"
            );
        }

        state.nostr_names.clear();
        assert_eq!(
            build(&state, &me).name(&key(2)),
            Some(crate::identity::truncate_pubkey(&key(2)).as_str())
        );
    }

    #[test]
    fn the_file_round_trips_and_belongs_to_one_account() {
        let dir = std::env::temp_dir().join(format!("discordia-peers-{}", uuid::Uuid::new_v4()));
        let peers = PushPeers {
            account: key(1),
            peers: vec![Peer {
                pubkey: key(2),
                name: "ana".into(),
            }],
        };
        assert_eq!(read(&dir, &key(1)), PushPeers::default());
        write(&dir, &peers).unwrap();
        assert_eq!(read(&dir, &key(1)), peers);
        assert_eq!(read(&dir, &key(9)), PushPeers::default());
        std::fs::write(dir.join(PEERS_FILE), "{garbage").unwrap();
        assert_eq!(read(&dir, &key(1)), PushPeers::default());
    }
}
