//! Which list a conversation belongs in. Pure, so the app and a push that
//! wakes a cold process (trap 40) decide from the same rule.

use std::collections::HashMap;

/// A bot with `Members` can harvest every pubkey and DM it over relays,
/// outside any server's reach (#311).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmStanding {
    Inbox,
    Request,
    Ignored,
    Blocked,
}

/// What the person chose for a peer: one value, so a peer cannot be both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmChoice {
    Accepted,
    Ignored,
}

/// `known` is a contact or a guild-mate: either outranks an ignore.
pub fn standing(blocked: bool, choice: Option<DmChoice>, known: bool) -> DmStanding {
    if blocked {
        DmStanding::Blocked
    } else if known || choice == Some(DmChoice::Accepted) {
        DmStanding::Inbox
    } else if choice == Some(DmChoice::Ignored) {
        DmStanding::Ignored
    } else {
        DmStanding::Request
    }
}

/// `settings.json` keeps two lists. A key in both was written by older builds,
/// where the accept won.
pub fn choice_in(accepted: &[String], ignored: &[String], peer: &str) -> Option<DmChoice> {
    if accepted.iter().any(|key| key == peer) {
        Some(DmChoice::Accepted)
    } else if ignored.iter().any(|key| key == peer) {
        Some(DmChoice::Ignored)
    } else {
        None
    }
}

pub fn choices_from(accepted: &[String], ignored: &[String]) -> HashMap<String, DmChoice> {
    ignored
        .iter()
        .map(|key| (key.clone(), DmChoice::Ignored))
        .chain(accepted.iter().map(|key| (key.clone(), DmChoice::Accepted)))
        .collect()
}

/// The two sorted lists `settings.json` holds.
pub fn lists_of(choices: &HashMap<String, DmChoice>) -> (Vec<String>, Vec<String>) {
    let mut accepted = Vec::new();
    let mut ignored = Vec::new();
    for (key, choice) in choices {
        match choice {
            DmChoice::Accepted => accepted.push(key.clone()),
            DmChoice::Ignored => ignored.push(key.clone()),
        }
    }
    accepted.sort();
    ignored.sort();
    (accepted, ignored)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_standing() {
        use DmChoice::Accepted;
        use DmStanding::*;
        let table = [
            (true, Some(Accepted), true, Blocked),
            (true, None, false, Blocked),
            (false, Some(Accepted), false, Inbox),
            (false, None, true, Inbox),
            (false, Some(DmChoice::Ignored), true, Inbox),
            (false, Some(DmChoice::Ignored), false, Ignored),
            (false, None, false, Request),
        ];
        for (blocked, choice, known, expected) in table {
            assert_eq!(
                standing(blocked, choice, known),
                expected,
                "{blocked} {choice:?} {known}"
            );
        }
    }

    #[test]
    fn old_settings_with_a_key_in_both_lists_load_as_accepted() {
        let accepted = vec!["a".to_string(), "both".to_string()];
        let ignored = vec!["i".to_string(), "both".to_string()];
        let choices = choices_from(&accepted, &ignored);
        for key in ["a", "both", "i", "none"] {
            assert_eq!(
                choices.get(key).copied(),
                choice_in(&accepted, &ignored, key),
                "{key}"
            );
        }
        assert_eq!(choices["both"], DmChoice::Accepted);
        assert_eq!(
            lists_of(&choices),
            (
                vec!["a".to_string(), "both".to_string()],
                vec!["i".to_string()]
            )
        );
    }
}
