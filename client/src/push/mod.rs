//! Mobile push. `headless` works from files alone because a push can start a
//! process that never had an Activity; the rest runs in the live app.

pub mod attention;
pub mod device;
pub mod gateway;
pub mod headless;
pub mod peers;
pub mod registration;
pub mod tap;

const NAME_CHARS: usize = 100;

/// What a notification may show of a name someone else chose.
pub(crate) fn clean_name(name: &str) -> String {
    name.chars()
        .filter(|c| !c.is_control())
        .take(NAME_CHARS)
        .collect()
}

pub(crate) fn is_event_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
