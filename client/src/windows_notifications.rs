use std::sync::Mutex;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsIconic, IsWindowVisible};
use windows::core::{HSTRING, PCWSTR};

const APP_ID: &str = "com.discordia.app";
static ACTIVE_CALL_TOAST: Mutex<Option<ToastNotification>> = Mutex::new(None);

pub fn is_minimized_or_hidden() -> bool {
    let title: Vec<u16> = crate::MAIN_WINDOW_TITLE
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: the title is null-terminated and outlives the call.
    let hwnd = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())) };
    if let Ok(hwnd) = hwnd
        && !hwnd.is_invalid()
    {
        // SAFETY: hwnd is checked as valid.
        unsafe { !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() }
    } else {
        true
    }
}

pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn show_dm_notification(account: &str, sender: &str, message: &str, peer: &str) {
    if !crate::settings::load_or_default().tray_notifications || !is_minimized_or_hidden() {
        return;
    }
    let xml = format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
        xml_escape(sender),
        xml_escape(message),
    );
    let doc = match XmlDocument::new() {
        Ok(doc) => doc,
        Err(e) => {
            tracing::warn!(%e, "Could not create XmlDocument for DM toast");
            return;
        }
    };
    if let Err(e) = doc.LoadXml(&HSTRING::from(&xml)) {
        tracing::warn!(%e, "Could not load XML for DM toast");
        return;
    }
    let toast = match ToastNotification::CreateToastNotification(&doc) {
        Ok(toast) => toast,
        Err(e) => {
            tracing::warn!(%e, "Could not create DM ToastNotification");
            return;
        }
    };
    let click_account = account.to_string();
    let click_peer = peer.to_string();
    let _ = toast.Activated(&TypedEventHandler::new(move |_sender, _args| {
        crate::app::request_restore();
        crate::dm_notifications::request_open_dm(&click_account, &click_peer);
        Ok(())
    }));
    let notifier = match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
    {
        Ok(notifier) => notifier,
        Err(e) => {
            tracing::warn!(%e, "Could not create ToastNotifier for DM toast");
            return;
        }
    };
    if let Err(e) = notifier.Show(&toast) {
        tracing::warn!(%e, "Could not show DM toast");
    }
}

pub fn show_call_notification(account: &str, caller_name: &str, peer: &str) {
    if !crate::settings::load_or_default().tray_notifications || !is_minimized_or_hidden() {
        return;
    }
    let title = crate::i18n::text("Incoming voice call", &[]);
    let body = crate::i18n::text("{0} is calling you", &[caller_name.to_string()]);
    let xml = format!(
        "<toast scenario=\"incomingCall\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual><audio silent=\"true\"/></toast>",
        xml_escape(&title),
        xml_escape(&body),
    );
    let doc = match XmlDocument::new() {
        Ok(doc) => doc,
        Err(e) => {
            tracing::warn!(%e, "Could not create XmlDocument for call toast");
            return;
        }
    };
    if let Err(e) = doc.LoadXml(&HSTRING::from(&xml)) {
        tracing::warn!(%e, "Could not load XML for call toast");
        return;
    }
    let toast = match ToastNotification::CreateToastNotification(&doc) {
        Ok(toast) => toast,
        Err(e) => {
            tracing::warn!(%e, "Could not create call ToastNotification");
            return;
        }
    };
    let click_account = account.to_string();
    let click_peer = peer.to_string();
    let _ = toast.Activated(&TypedEventHandler::new(move |_sender, _args| {
        crate::app::request_restore();
        crate::dm_notifications::request_open_dm(&click_account, &click_peer);
        Ok(())
    }));
    let notifier = match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
    {
        Ok(notifier) => notifier,
        Err(e) => {
            tracing::warn!(%e, "Could not create ToastNotifier for call toast");
            return;
        }
    };
    if let Err(e) = notifier.Show(&toast) {
        tracing::warn!(%e, "Could not show call toast");
    } else if let Ok(mut active) = ACTIVE_CALL_TOAST.lock() {
        *active = Some(toast);
    }
}

pub fn hide_call_notification() {
    let toast = match ACTIVE_CALL_TOAST.lock() {
        Ok(mut active) => active.take(),
        Err(_) => None,
    };
    if let Some(toast) = toast
        && let Ok(notifier) =
            ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
    {
        let _ = notifier.Hide(&toast);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escaping_escapes_special_characters() {
        assert_eq!(xml_escape("Bob & Alice"), "Bob &amp; Alice");
        assert_eq!(
            xml_escape("<tag> \"quote\" 'apostrophe'"),
            "&lt;tag&gt; &quot;quote&quot; &apos;apostrophe&apos;"
        );
        assert_eq!(xml_escape("Plain Text 123"), "Plain Text 123");
    }
}
