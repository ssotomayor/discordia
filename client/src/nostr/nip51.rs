//! Kind 10000, replaceable like kind 3: read-modify-written whole, keeping the
//! tags and encrypted private half another client wrote.

use secp256k1::SecretKey;

use super::event::{self, Event};

pub const KIND_MUTE_LIST: u16 = 10000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MuteList {
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub created_at: i64,
}

impl MuteList {
    pub fn from_pubkeys(pubkeys: impl IntoIterator<Item = String>) -> Self {
        pubkeys
            .into_iter()
            .fold(Self::default(), |list, pk| list.with(&pk))
    }

    pub fn with(mut self, pubkey: &str) -> Self {
        if !self.contains(pubkey) {
            self.tags.push(vec!["p".to_string(), pubkey.to_string()]);
        }
        self
    }

    pub fn without(mut self, pubkey: &str) -> Self {
        self.tags.retain(|t| !is_p(t, pubkey));
        self
    }

    pub fn contains(&self, pubkey: &str) -> bool {
        self.tags.iter().any(|t| is_p(t, pubkey))
    }

    pub fn pubkeys(&self) -> impl Iterator<Item = &str> {
        self.tags
            .iter()
            .filter(|t| t.first().map(String::as_str) == Some("p"))
            .filter_map(|t| t.get(1).map(String::as_str))
    }
}

fn is_p(tag: &[String], pubkey: &str) -> bool {
    tag.first().map(String::as_str) == Some("p") && tag.get(1).map(String::as_str) == Some(pubkey)
}

pub fn mute_list_event(secret: &SecretKey, list: &MuteList, now: i64) -> Event {
    event::sign_with(
        secret,
        now,
        KIND_MUTE_LIST,
        list.tags.clone(),
        list.content.clone(),
    )
}

pub fn parse_mute_list(event: &Event) -> Option<MuteList> {
    (event.kind == KIND_MUTE_LIST).then(|| MuteList {
        tags: event.tags.clone(),
        content: event.content.clone(),
        created_at: event.created_at,
    })
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
    fn a_mute_list_round_trips_with_what_we_do_not_read() {
        let foreign = event::sign_with(
            &key(1),
            5,
            KIND_MUTE_LIST,
            vec![vec!["t".into(), "spam".into()], vec!["p".into(), pk('a')]],
            "nip44-private-items".into(),
        );
        let list = parse_mute_list(&foreign)
            .expect("kind 10000")
            .with(&pk('b'));
        let republished = mute_list_event(&key(1), &list, 6);
        assert!(republished.verify());
        let again = parse_mute_list(&republished).expect("kind 10000");
        assert_eq!(again.content, "nip44-private-items");
        assert!(again.tags.contains(&vec!["t".into(), "spam".into()]));
        assert_eq!(again.pubkeys().collect::<Vec<_>>(), [pk('a'), pk('b')]);
    }

    #[test]
    fn muting_twice_keeps_one_entry_and_unmuting_keeps_the_rest() {
        let list = MuteList::default()
            .with(&pk('a'))
            .with(&pk('a'))
            .with(&pk('b'))
            .without(&pk('a'));
        assert!(!list.contains(&pk('a')));
        assert_eq!(list.pubkeys().collect::<Vec<_>>(), [pk('b')]);
    }

    #[test]
    fn another_kind_is_not_a_mute_list() {
        let e = event::sign_with(&key(1), 1, 3, vec![], String::new());
        assert!(parse_mute_list(&e).is_none());
    }
}
