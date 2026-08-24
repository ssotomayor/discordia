//! Kind 3 is public and replaceable: publishing a partial list deletes
//! everyone missing from it, so the list is read-modify-written whole.

use secp256k1::SecretKey;

use super::event::{self, Event};

pub const KIND_CONTACTS: u16 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub pubkey: String,
    pub relay: Option<String>,
    pub petname: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContactList {
    pub contacts: Vec<Contact>,
}

impl ContactList {
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

    pub fn without(mut self, pubkey: &str) -> Self {
        self.contacts.retain(|c| c.pubkey != pubkey);
        self
    }

    pub fn contains(&self, pubkey: &str) -> bool {
        self.contacts.iter().any(|c| c.pubkey == pubkey)
    }

    #[allow(dead_code)]
    pub fn petname(&self, pubkey: &str) -> Option<&str> {
        self.contacts
            .iter()
            .find(|c| c.pubkey == pubkey)
            .and_then(|c| c.petname.as_deref())
    }
}

pub fn contact_list_event(secret: &SecretKey, list: &ContactList, now: i64) -> Event {
    let tags = list
        .contacts
        .iter()
        .map(|c| {
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
