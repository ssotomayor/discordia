//! Mobile push. `headless` works from files alone because a push can start a
//! process that never had an Activity; `registration` runs in the live app.

pub mod registration;

const NAME_CHARS: usize = 100;

/// What a notification may show of a name someone else chose.
pub(crate) fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(NAME_CHARS)
        .collect()
}
