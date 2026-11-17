use std::collections::HashMap;

use super::{
    event::{self, Rumor},
    service::{conversation_id, message_id},
};
use crate::{protocol::Id, state::AppState};

pub const KIND: u16 = 7;
const MAX_ENTRIES: usize = 4096;
type Key = (Id, Id, String, String);
type Version = ((i64, String), bool);

#[derive(Clone, Default)]
pub(crate) struct Reactions {
    versions: HashMap<Key, Version>,
}

pub fn rumor(from: &str, peer: &str, target: &str, emoji: &str, added: bool, now: i64) -> Rumor {
    event::rumor(
        from,
        now,
        KIND,
        vec![
            vec!["p".into(), peer.into()],
            vec!["e".into(), target.into()],
            vec![
                "discordia-reaction".into(),
                if added { "add" } else { "remove" }.into(),
            ],
        ],
        emoji.into(),
    )
}

pub fn valid_emoji(emoji: &str) -> bool {
    emoji.len() <= 64 && matches!(crate::emoji::unicode_parts(emoji).as_slice(), [(_, true)])
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
        || !valid_emoji(&rumor.content)
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
        }
    }
    versions.insert(key, (version, added));
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
        let add = rumor(&ours, &peer, &original.id, "👍", true, 2);
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
            let remove = rumor(&ours, &peer, &original.id, "👍", false, 3);
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
            rumor(&peer, &stranger, &target, "👍", true, 1),
            rumor(&peer, &ours, "bad target", "👍", true, 1),
            rumor(&peer, &ours, &target, "not an emoji", true, 1),
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
            &rumor(&peer, &ours, &target, "👍", true, 1),
        );
        assert!(state.dm_reactions.versions.is_empty());
        assert!(valid_emoji("❤️"));
        assert!(!valid_emoji("👍👍"));
        assert!(!valid_emoji(":custom:"));
    }
}
