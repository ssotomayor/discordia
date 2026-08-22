//! NIP-02 contact list — the friends that come with you.
//!
//! The other half of "changing servers loses your people". Direct messages
//! stopped belonging to a host when they became gift wraps; this does the same
//! for the list of who you talk to. A kind:3 event holds the whole list, signed
//! by you and stored on relays, so a fresh install on a new machine — or a
//! different Discordia server entirely — pulls it back from your key alone.
//!
//! **It is a public event, and that is not a detail.** Anyone can read who you
//! follow. That is how Nostr's social graph has always worked and it is the
//! opposite of the gift-wrapped messages beside it, so the UI must never let
//! the two be confused: adding a contact is a public act, sending them a
//! message is not.
//!
//! **The list is replaced wholesale, never appended to.** Kind 3 is a
//! *replaceable* event: relays keep only the newest per author, so publishing a
//! list with one name deletes every other name you had. Anything that edits it
//! must read the current list first, which is why `ContactList` carries the
//! whole set and there is no `add_one` here.

use secp256k1::SecretKey;

use super::event::{self, Event};

/// A contact list, per NIP-02.
pub const KIND_CONTACTS: u16 = 3;

/// One entry: a key, optionally where to find them and what you call them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub pubkey: String,
    /// Relay where this person's notes can be found, if the list says.
    pub relay: Option<String>,
    /// A local nickname. Yours, not theirs — it is what *you* chose to call
    /// them, and it travels with the list rather than with their profile.
    pub petname: Option<String>,
}

/// The whole list, which is the only unit that can be published.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactList {
    pub contacts: Vec<Contact>,
}

impl ContactList {
    /// Add or update one entry, keeping the rest.
    ///
    /// Returns the list so a caller has to publish the *result* rather than
    /// forgetting that a partial list is a deletion.
    pub fn with(mut self, contact: Contact) -> Self {
        match self
            .contacts
            .iter_mut()
            .find(|c| c.pubkey == contact.pubkey)
        {
            Some(existing) => *existing = contact,
            None => self.contacts.push(contact),
        }
        self
    }

    /// Remove one entry, keeping the rest.
    pub fn without(mut self, pubkey: &str) -> Self {
        self.contacts.retain(|c| c.pubkey != pubkey);
        self
    }

    pub fn contains(&self, pubkey: &str) -> bool {
        self.contacts.iter().any(|c| c.pubkey == pubkey)
    }

    /// Put someone on the list, keeping whatever you already knew about them.
    ///
    /// Adding is not editing. `with` replaces an entry wholesale, which is what
    /// `renamed` wants and the opposite of what this wants: re-adding a key you
    /// already have — pasting it into the add box a second time, say — must not
    /// throw away the name you gave them or the relay hint that says where to
    /// find them. Already present means nothing to do.
    pub fn added(&self, pubkey: &str) -> Self {
        if self.contains(pubkey) {
            return self.clone();
        }
        self.clone().with(Contact {
            pubkey: pubkey.to_string(),
            relay: None,
            petname: None,
        })
    }

    /// Rename someone already on the list, keeping their relay hint.
    ///
    /// Blank clears the name instead of storing one: a petname is absent or it
    /// is something, and `Some("")` would publish a tag claiming you call them
    /// nothing.
    ///
    /// A key that is not on the list comes back unchanged. Naming a stranger
    /// would otherwise add them to a list anyone can read — a rename is not a
    /// decision to publish a relationship, and must not become one.
    pub fn renamed(&self, pubkey: &str, petname: Option<String>) -> Self {
        match self.contacts.iter().find(|c| c.pubkey == pubkey) {
            Some(existing) => self.clone().with(Contact {
                petname: petname.filter(|p| !p.trim().is_empty()),
                ..existing.clone()
            }),
            None => self.clone(),
        }
    }

    /// What we call `pubkey`, if we call them anything.
    pub fn petname(&self, pubkey: &str) -> Option<&str> {
        self.contacts
            .iter()
            .find(|c| c.pubkey == pubkey)
            .and_then(|c| c.petname.as_deref())
    }
}

/// Build the signed kind:3 event for a list.
pub fn contact_list_event(secret: &SecretKey, list: &ContactList, now: i64) -> Event {
    let tags = list
        .contacts
        .iter()
        .map(|c| {
            // Positional, per NIP-02: ["p", pubkey, relay, petname]. A petname
            // with no relay still needs the relay slot present, or the petname
            // would be read as the relay.
            vec![
                "p".to_string(),
                c.pubkey.clone(),
                c.relay.clone().unwrap_or_default(),
                c.petname.clone().unwrap_or_default(),
            ]
        })
        .collect();
    event::sign_with(secret, now, KIND_CONTACTS, tags, String::new())
}

/// Read a kind:3 back into a list.
pub fn parse_contact_list(event: &Event) -> ContactList {
    if event.kind != KIND_CONTACTS {
        return ContactList::default();
    }
    let contacts = event
        .tags
        .iter()
        .filter(|t| t.first().map(String::as_str) == Some("p"))
        .filter_map(|t| {
            let pubkey = t.get(1)?.clone();
            // Reject malformed pubkeys to avoid creating uncontactable
            // entries.
            if pubkey.len() != 64 || !pubkey.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            let non_empty = |s: Option<&String>| s.filter(|v| !v.is_empty()).cloned();
            Some(Contact {
                pubkey,
                relay: non_empty(t.get(2)),
                petname: non_empty(t.get(3)),
            })
        })
        .collect();
    ContactList { contacts }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> SecretKey {
        SecretKey::from_slice(&[seed; 32]).expect("valid key")
    }

    fn pk(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    /// A list survives a publish and a read, petnames and all.
    #[test]
    fn a_contact_list_round_trips() {
        let list = ContactList::default()
            .with(Contact {
                pubkey: pk('a'),
                relay: Some("wss://r.example".into()),
                petname: Some("Ana".into()),
            })
            .with(Contact {
                pubkey: pk('b'),
                relay: None,
                petname: None,
            });
        let e = contact_list_event(&key(1), &list, 1_700_000_000);
        assert!(e.verify());
        assert_eq!(parse_contact_list(&e), list);
    }

    /// A petname with no relay must not slide into the relay slot.
    #[test]
    fn a_petname_without_a_relay_stays_a_petname() {
        let list = ContactList::default().with(Contact {
            pubkey: pk('c'),
            relay: None,
            petname: Some("Cy".into()),
        });
        let parsed = parse_contact_list(&contact_list_event(&key(1), &list, 1));
        assert_eq!(parsed.contacts[0].petname.as_deref(), Some("Cy"));
        assert_eq!(parsed.contacts[0].relay, None);
    }

    /// Editing replaces in place rather than duplicating, because a list with
    /// the same key twice is one a relay may resolve either way.
    #[test]
    fn editing_a_contact_does_not_duplicate_it() {
        let list = ContactList::default()
            .with(Contact {
                pubkey: pk('a'),
                relay: None,
                petname: Some("old".into()),
            })
            .with(Contact {
                pubkey: pk('a'),
                relay: None,
                petname: Some("new".into()),
            });
        assert_eq!(list.contacts.len(), 1);
        assert_eq!(list.petname(&pk('a')), Some("new"));
    }

    /// Removal keeps everyone else — the trap being that kind 3 is replaceable,
    /// so a list that drops someone by accident deletes them for good.
    #[test]
    fn removing_one_contact_keeps_the_others() {
        let list = ContactList::default()
            .with(Contact {
                pubkey: pk('a'),
                relay: None,
                petname: None,
            })
            .with(Contact {
                pubkey: pk('b'),
                relay: None,
                petname: None,
            })
            .without(&pk('a'));
        assert!(!list.contains(&pk('a')));
        assert!(list.contains(&pk('b')));
    }

    /// Adding is not editing. Pasting a key you already have into the add box
    /// must not throw away the name you gave them — `with` would have, because
    /// replacing wholesale is what a *rename* needs.
    #[test]
    fn re_adding_a_contact_keeps_their_name_and_relay() {
        let list = ContactList::default()
            .with(Contact {
                pubkey: pk('a'),
                relay: Some("wss://relay.example".into()),
                petname: Some("ana".into()),
            })
            .added(&pk('a'));

        assert_eq!(list.contacts.len(), 1);
        assert_eq!(list.petname(&pk('a')), Some("ana"));
        assert_eq!(
            list.contacts[0].relay.as_deref(),
            Some("wss://relay.example")
        );
    }

    #[test]
    fn adding_someone_new_puts_them_on_the_list() {
        let list = ContactList::default().added(&pk('b'));
        assert!(list.contains(&pk('b')));
        assert_eq!(list.petname(&pk('b')), None);
    }

    #[test]
    fn renaming_keeps_the_relay_hint_and_everyone_else() {
        let list = ContactList::default()
            .with(Contact {
                pubkey: pk('a'),
                relay: Some("wss://relay.example".into()),
                petname: None,
            })
            .with(Contact {
                pubkey: pk('b'),
                relay: None,
                petname: Some("bee".into()),
            })
            .renamed(&pk('a'), Some("ana".into()));

        assert_eq!(list.petname(&pk('a')), Some("ana"));
        assert_eq!(
            list.contacts
                .iter()
                .find(|c| c.pubkey == pk('a'))
                .and_then(|c| c.relay.as_deref()),
            Some("wss://relay.example"),
        );
        assert_eq!(list.petname(&pk('b')), Some("bee"));
    }

    /// A petname is absent or it is something. `Some("")` would publish a tag
    /// claiming you call them nothing.
    #[test]
    fn a_blank_rename_clears_the_name() {
        let list = ContactList::default()
            .with(Contact {
                pubkey: pk('a'),
                relay: None,
                petname: Some("ana".into()),
            })
            .renamed(&pk('a'), Some("   ".into()));

        assert_eq!(list.petname(&pk('a')), None);
        assert!(list.contains(&pk('a')));
    }

    /// Renaming is not a decision to publish a relationship. This list is
    /// public, so a rename that added someone would out them.
    #[test]
    fn renaming_a_stranger_does_not_add_them() {
        let list = ContactList::default()
            .with(Contact {
                pubkey: pk('a'),
                relay: None,
                petname: None,
            })
            .renamed(&pk('z'), Some("nobody".into()));

        assert!(!list.contains(&pk('z')));
        assert_eq!(list.contacts.len(), 1);
    }

    /// A junk entry from another client is skipped rather than becoming a
    /// contact whose key cannot be used.
    #[test]
    fn malformed_entries_are_skipped() {
        let e = event::sign_with(
            &key(1),
            1,
            KIND_CONTACTS,
            vec![
                vec!["p".into(), "not-a-key".into()],
                vec!["p".into(), pk('d')],
                vec!["e".into(), pk('e')],
            ],
            String::new(),
        );
        let parsed = parse_contact_list(&e);
        assert_eq!(parsed.contacts.len(), 1);
        assert_eq!(parsed.contacts[0].pubkey, pk('d'));
    }
}
