//! Mobile push. `headless` works from files alone because a push can start a
//! process that never had an Activity; the rest runs in the live app.

#[cfg(target_os = "android")]
mod android;
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

pub(crate) const DM_ALERT_CHARS: usize = 300;

/// The line a DM alert shows: the message on one line, or only that one arrived when `show` is off.
pub(crate) fn dm_alert_text(
    content: &str,
    show: bool,
    translate: impl Fn(&'static str) -> String,
) -> String {
    if !show {
        return translate("New private message");
    }
    if content == crate::nostr::nip17::IMAGE_PLACEHOLDER {
        return translate("Sent a picture");
    }
    let invited = crate::features::parse_voice_invite(content).is_some();
    let content = crate::features::strip_voice_invite(content);
    // An alert cannot ask before showing, so a spoiler never reaches one.
    let content = crate::features::markup::plain(&content);
    let cleaned: String = content
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let line = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() {
        return translate(if invited {
            "Invited you to a voice channel"
        } else {
            "New private message"
        });
    }
    if line.chars().count() <= DM_ALERT_CHARS {
        return line;
    }
    let mut cut: String = line.chars().take(DM_ALERT_CHARS).collect();
    cut.push('…');
    cut
}

pub(crate) fn is_event_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
