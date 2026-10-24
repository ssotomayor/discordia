use super::{ComposerPick, ComposerSuggestion, MAX_SUGGESTIONS};
use crate::{protocol::Id, state::AppState};

pub(super) fn active_token(before: &str) -> Option<String> {
    let at = before.rfind('@')?;
    let query = &before[at + 1..];
    if !query
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return None;
    }
    before[..at]
        .chars()
        .next_back()
        .is_none_or(char::is_whitespace)
        .then(|| format!("@{}", query.to_lowercase()))
}

pub(super) fn suggestions(s: &AppState, channel: Id, query: &str) -> Vec<ComposerSuggestion> {
    let mut candidates = Vec::new();
    if let Some(peer) = s.dm_of(channel) {
        if let Some(me) = &s.self_user {
            candidates.push((me.pubkey.clone(), me.username.clone()));
        }
        let username = s
            .members
            .iter()
            .find(|m| m.user.pubkey == peer.other_pubkey)
            .map(|m| m.user.username.clone())
            .unwrap_or_else(|| s.display_name(&peer.other_pubkey));
        candidates.push((peer.other_pubkey.clone(), username));
    } else if let Some(ch) = s.channels.iter().find(|ch| ch.id == channel) {
        candidates.extend(
            s.members
                .iter()
                .filter(|m| m.guild_id == ch.guild_id)
                .map(|m| (m.user.pubkey.clone(), m.user.username.clone())),
        );
    }
    let mut seen = std::collections::HashSet::new();
    let mut matches: Vec<_> = candidates
        .into_iter()
        .filter_map(|(pubkey, username)| {
            if !seen.insert(pubkey.clone()) {
                return None;
            }
            let name = s.display_name(&pubkey);
            let (display, canonical) = (name.to_lowercase(), username.to_lowercase());
            if !display.contains(query) && !canonical.contains(query) {
                return None;
            }
            let prefix = display.starts_with(query) || canonical.starts_with(query);
            Some((
                !prefix,
                display,
                pubkey.clone(),
                ComposerSuggestion {
                    name,
                    pick: ComposerPick::Mention { pubkey, username },
                },
            ))
        })
        .collect();
    matches.sort_by(|a, b| (&a.0, &a.1, &a.2).cmp(&(&b.0, &b.1, &b.2)));
    matches
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|m| m.3)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Channel, ChannelKind, Member, User};
    fn member(guild_id: Id, name: &str, key: &str) -> Member {
        Member {
            user: User {
                pubkey: key.into(),
                username: name.into(),
            },
            guild_id,
            online: true,
            bot: false,
            roles: vec![],
            xp: 0,
            look: Default::default(),
        }
    }
    fn channel(guild_id: Id) -> Channel {
        Channel {
            id: Id::new_v4(),
            guild_id,
            name: "general".into(),
            kind: ChannelKind::Text,
            topic: None,
            read_only: false,
            slowmode_secs: 0,
            position: 0,
            access: None,
            divider: Default::default(),
            markdown: false,
        }
    }
    #[test]
    fn at_opens_immediately_and_matches_unicode_without_matching_emails() {
        assert_eq!(active_token("@"), Some("@".into()));
        assert_eq!(active_token("hola @Ál"), Some("@ál".into()));
        assert_eq!(active_token("\n@alex_2"), Some("@alex_2".into()));
        for text in ["a@b", "@alex ", "@@alex", "@alex!", "https://a/@alex"] {
            assert_eq!(active_token(text), None, "{text}");
        }
    }
    #[test]
    fn suggestions_are_guild_scoped_ranked_deduplicated_and_bounded() {
        let mut s = AppState::empty();
        let guild = Id::new_v4();
        let ch = channel(guild);
        let id = ch.id;
        s.channels.push(ch);
        s.members = vec![
            member(guild, "Calex", "a"),
            member(guild, "Alex", "b"),
            member(Id::new_v4(), "Alex elsewhere", "c"),
            member(guild, "Alex", "b"),
        ];
        let got = suggestions(&s, id, "al");
        assert_eq!(
            got.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            ["Alex", "Calex"]
        );
        assert!(suggestions(&s, id, "missing").is_empty());
        for i in 0..20 {
            s.members
                .push(member(guild, &format!("Alex{i}"), &format!("key{i}")));
        }
        assert_eq!(suggestions(&s, id, "").len(), MAX_SUGGESTIONS);
        assert!(suggestions(&s, Id::new_v4(), "").is_empty());
    }
    #[test]
    fn dm_completion_contains_only_its_participants() {
        let mut s = AppState::empty();
        let id = Id::new_v4();
        let guild = Id::new_v4();
        s.self_user = Some(User {
            pubkey: "me".into(),
            username: "Me".into(),
        });
        s.dms.push(crate::state::DmInfo {
            channel_id: id,
            other_pubkey: "peer".into(),
        });
        s.members = vec![
            member(guild, "Peer", "peer"),
            member(guild, "Other", "other"),
        ];
        let got = suggestions(&s, id, "");
        let keys: Vec<_> = got
            .iter()
            .filter_map(|x| {
                if let ComposerPick::Mention { pubkey, .. } = &x.pick {
                    Some(pubkey.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(keys.len(), 2);
        assert!(keys.contains(&"me") && keys.contains(&"peer"));
        assert!(!keys.contains(&"other"));
    }
}
