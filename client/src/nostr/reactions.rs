use std::collections::HashMap;

use super::{
    event::{self, Rumor},
    service::{conversation_id, message_id},
};
use crate::{protocol::Id, state::AppState};

pub const KIND: u16 = 7;
const MAX_ENTRIES: usize = 4096;
// Each picture may be an inline 16 KiB data url from the peer.
const MAX_PICTURES: usize = 1024;
type Key = (Id, Id, String, String);
type Version = ((i64, String), bool);

#[derive(Clone, Default)]
pub(crate) struct Reactions {
    versions: HashMap<Key, Version>,
    /// The first picture seen for a `:code:` on a message is the one drawn.
    pictures: HashMap<(Id, Id, String), String>,
}

pub fn rumor(
    from: &str,
    peer: &str,
    target: &str,
    emoji: &str,
    picture: Option<&str>,
    added: bool,
    now: i64,
) -> Rumor {
    let mut tags = vec![
        vec!["p".into(), peer.into()],
        vec!["e".into(), target.into()],
        vec![
            "discordia-reaction".into(),
            if added { "add" } else { "remove" }.into(),
        ],
    ];
    if let (Some(code), Some(url)) = (custom_code(emoji), picture) {
        tags.push(vec!["emoji".into(), code.into(), url.into()]);
    }
    event::rumor(from, now, KIND, tags, emoji.into())
}

pub fn valid_emoji(emoji: &str) -> bool {
    emoji.len() <= 64 && matches!(crate::emoji::unicode_parts(emoji).as_slice(), [(_, true)])
}

/// The shortcode of a NIP-25 custom reaction, `:code:`.
pub fn custom_code(emoji: &str) -> Option<&str> {
    emoji
        .strip_prefix(':')?
        .strip_suffix(':')
        .filter(|code| crate::protocol::valid_shortcode(code))
}

pub fn valid_picture(url: &str) -> bool {
    url.len() <= crate::emoji::DM_INLINE_MAX
        && (url.starts_with("https://") || url.starts_with("data:image/"))
}

impl Reactions {
    pub fn next_time(&self, channel: Id, target: Id, author: &str, emoji: &str, now: i64) -> i64 {
        self.versions
            .get(&(channel, target, author.into(), emoji.into()))
            .map_or(now, |((previous, _), _)| {
                now.max(previous.saturating_add(1))
            })
    }

    pub fn forget(&mut self, target: Id) {
        self.versions.retain(|(_, id, _, _), _| *id != target);
        self.pictures.retain(|(_, id, _), _| *id != target);
    }

    pub fn picture(&self, channel: Id, target: Id, emoji: &str) -> Option<&str> {
        self.pictures
            .get(&(channel, target, emoji.to_string()))
            .map(String::as_str)
    }

    /// `(code, url)` for every custom reaction on a message, as `EmojiText` takes them.
    pub fn pictures_of(&self, channel: Id, target: Id) -> Vec<(String, String)> {
        self.pictures
            .iter()
            .filter(|((cid, mid, _), _)| *cid == channel && *mid == target)
            .filter_map(|((_, _, emoji), url)| Some((custom_code(emoji)?.to_string(), url.clone())))
            .collect()
    }
}

pub fn receive(state: &mut AppState, ours: &str, rumor: &Rumor) -> bool {
    if rumor.kind != KIND {
        return false;
    }
    let Some(recipient) = rumor.tag("p") else {
        return true;
    };
    if !crate::protocol::is_canonical_pubkey(recipient)
        || !crate::protocol::is_canonical_pubkey(&rumor.pubkey)
        || (rumor.pubkey != ours && recipient != ours)
    {
        return true;
    }
    let peer = if rumor.pubkey == ours {
        recipient
    } else {
        &rumor.pubkey
    };
    let Some(target) = rumor
        .tag("e")
        .filter(|id| crate::protocol::is_canonical_pubkey(id))
    else {
        return true;
    };
    if state.mutes.contains(peer) || state.dm_deleted_ids.contains(target) {
        return true;
    }
    let added = match rumor.tag("discordia-reaction") {
        None | Some("add") => true,
        Some("remove") => false,
        _ => return true,
    };
    let picture = custom_code(&rumor.content).and_then(|code| {
        super::nip17::emoji_tags(rumor)
            .into_iter()
            .find(|(c, url)| c == code && valid_picture(url))
            .map(|(_, url)| url)
    });
    // A removal names its emoji only to take it away, so it needs no picture.
    let valid = match custom_code(&rumor.content) {
        Some(_) => picture.is_some() || !added,
        None => valid_emoji(&rumor.content),
    };
    if !valid {
        return true;
    }
    let channel = conversation_id(peer);
    let target = message_id(target);
    let key = (channel, target, rumor.pubkey.clone(), rumor.content.clone());
    let version = (rumor.created_at, rumor.id.clone());
    let versions = &mut state.dm_reactions.versions;
    if versions.get(&key).is_some_and(|(held, _)| *held >= version) {
        return true;
    }
    if !versions.contains_key(&key) {
        let emoji_count = versions
            .iter()
            .filter(|((cid, mid, _, _), (_, added))| *cid == channel && *mid == target && *added)
            .map(|((_, _, _, emoji), _)| emoji)
            .collect::<std::collections::HashSet<_>>();
        if added && emoji_count.len() >= 20 && !emoji_count.contains(&rumor.content) {
            return true;
        }
        if versions.len() >= MAX_ENTRIES {
            let oldest = versions.iter().min_by(|(_, a), (_, b)| a.0.cmp(&b.0));
            let Some((oldest_key, _)) = oldest else {
                return true;
            };
            let oldest_key = oldest_key.clone();
            versions.remove(&oldest_key);
            let (cid, mid, _, emoji) = oldest_key;
            if !versions.keys().any(|(c, m, _, e)| (c, m, e) == (&cid, &mid, &emoji)) {
                state.dm_reactions.pictures.remove(&(cid, mid, emoji));
            }
        }
    }
    let versions = &mut state.dm_reactions.versions;
    versions.insert(key, (version, added));
    let pictures = &mut state.dm_reactions.pictures;
    if let Some(url) = picture
        && pictures.len() < MAX_PICTURES
    {
        pictures
            .entry((channel, target, rumor.content.clone()))
            .or_insert(url);
    }
    if let Some(message) = state
        .messages
        .get_mut(&channel)
        .and_then(|messages| messages.iter_mut().find(|message| message.id == target))
    {
        crate::protocol::apply_reaction(
            &mut message.reactions,
            &rumor.content,
            &rumor.pubkey,
            added,
        );
    }
    true
}

pub fn apply_pending(state: &mut AppState, channel: Id, target: Id) {
    let Some(message) = state
        .messages
        .get_mut(&channel)
        .and_then(|messages| messages.iter_mut().find(|message| message.id == target))
    else {
        return;
    };
    for ((cid, mid, author, emoji), (_, added)) in &state.dm_reactions.versions {
        if *cid == channel && *mid == target {
            crate::protocol::apply_reaction(&mut message.reactions, emoji, author, *added);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Message, User};

    fn message(channel: Id, target: Id, author: &str) -> Message {
        Message {
            event_notice: None,
            poll: None,
            id: target,
            channel_id: channel,
            author: User {
                pubkey: author.into(),
                username: "peer".into(),
            },
            author_bot: false,
            content: "hello".into(),
            image: None,
            image_metadata: None,
            attachment: None,
            reactions: Vec::new(),
            reply_to: None,
            edited_at: None,
            pinned: false,
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn encrypted_reactions_reach_both_devices_and_removal_survives_replay() {
        let secret = secp256k1::SecretKey::from_slice(&[1; 32]).unwrap();
        let peer_secret = secp256k1::SecretKey::from_slice(&[2; 32]).unwrap();
        let ours = event::xonly_hex(&secret);
        let peer = event::xonly_hex(&peer_secret);
        let original = super::super::nip17::chat_rumor(&peer, &ours, "hello", None, &[], 1);
        let target = message_id(&original.id);
        let add = rumor(&ours, &peer, &original.id, "👍", None, true, 2);
        let (theirs, mine) = super::super::nip17::wrap_both(&secret, &peer, &add, 2).unwrap();
        assert!(!theirs.content.contains("👍"));
        for (reader, key, other, wrap) in [
            (&ours, &secret, &peer, &mine),
            (&peer, &peer_secret, &ours, &theirs),
        ] {
            let opened = super::super::nip59::unwrap(key, wrap).unwrap();
            let channel = conversation_id(other);
            let mut state = AppState::empty();
            assert!(receive(&mut state, reader, &opened));
            assert!(state.messages.is_empty());
            state
                .messages
                .insert(channel, vec![message(channel, target, &peer)]);
            apply_pending(&mut state, channel, target);
            assert_eq!(
                state.messages[&channel][0].reactions[0].users,
                vec![ours.clone()]
            );
            let remove = rumor(&ours, &peer, &original.id, "👍", None, false, 3);
            for event in [&remove, &add, &remove] {
                receive(&mut state, reader, event);
            }
            assert!(state.messages[&channel][0].reactions.is_empty());
            assert_eq!(
                state
                    .dm_reactions
                    .next_time(channel, target, &ours, "👍", 3),
                4
            );
            state.delete_message_locally(channel, target);
            assert!(state.dm_reactions.versions.is_empty());
        }
    }

    #[test]
    fn invalid_or_unrelated_reactions_do_not_modify_the_conversation() {
        let ours = "ab".repeat(32);
        let peer = "cd".repeat(32);
        let stranger = "ef".repeat(32);
        let target = "12".repeat(32);
        let mut state = AppState::empty();
        for event in [
            rumor(&peer, &stranger, &target, "👍", None, true, 1),
            rumor(&peer, &ours, "bad target", "👍", None, true, 1),
            rumor(&peer, &ours, &target, "not an emoji", None, true, 1),
        ] {
            assert!(receive(&mut state, &ours, &event));
        }
        assert!(state.dm_reactions.versions.is_empty());
        assert!(state.messages.is_empty());
        assert!(state.dms.is_empty());
        state.dm_deleted_ids.insert(target.clone());
        receive(
            &mut state,
            &ours,
            &rumor(&peer, &ours, &target, "👍", None, true, 1),
        );
        assert!(state.dm_reactions.versions.is_empty());
        assert!(valid_emoji("❤️"));
        assert!(!valid_emoji("👍👍"));
        assert!(!valid_emoji(":custom:"));
    }

    #[test]
    fn a_saved_emoji_reaction_carries_its_picture_and_a_removal_needs_none() {
        let ours = "ab".repeat(32);
        let peer = "cd".repeat(32);
        let target = "12".repeat(32);
        let channel = conversation_id(&peer);
        let mid = message_id(&target);
        let mut state = AppState::empty();
        state
            .messages
            .insert(channel, vec![message(channel, mid, &ours)]);
        let url = "https://blob.example/cat.png";
        for bare in [
            rumor(&peer, &ours, &target, ":cat:", None, true, 1),
            rumor(&peer, &ours, &target, ":cat:", Some("http://blob.example/cat.png"), true, 1),
            rumor(&peer, &ours, &target, ":not a code:", Some(url), true, 1),
        ] {
            receive(&mut state, &ours, &bare);
        }
        assert!(state.messages[&channel][0].reactions.is_empty());
        let add = rumor(&peer, &ours, &target, ":cat:", Some(url), true, 2);
        assert_eq!(super::super::nip17::emoji_tags(&add), vec![("cat".into(), url.into())]);
        receive(&mut state, &ours, &add);
        assert_eq!(state.messages[&channel][0].reactions[0].emoji, ":cat:");
        assert_eq!(
            state.dm_reactions.pictures_of(channel, mid),
            vec![("cat".to_string(), url.to_string())]
        );
        receive(&mut state, &ours, &rumor(&peer, &ours, &target, ":cat:", None, false, 3));
        assert!(state.messages[&channel][0].reactions.is_empty());
        state.dm_reactions.forget(mid);
        assert!(state.dm_reactions.pictures_of(channel, mid).is_empty());
    }
}
