#[cfg(not(any(target_os = "android", target_os = "ios")))]
use std::cell::RefCell;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
thread_local! {
    // X11 serves clipboard contents from the owner, so it must outlive the copy operation.
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn copy_text(text: &str) -> bool {
    let result = CLIPBOARD.with(|slot| -> Result<(), String> {
        let mut clipboard = slot.borrow_mut();
        if clipboard.is_none() {
            *clipboard = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
        }
        match clipboard.as_mut() {
            Some(clipboard) => clipboard.set_text(text).map_err(|e| e.to_string()),
            None => Err("Clipboard unavailable".into()),
        }
    });
    if let Err(error) = result {
        tracing::warn!(%error, "Couldn't copy text to the system clipboard");
        return false;
    }
    true
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn read() -> Result<serde_json::Value, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    if let Ok(text) = clipboard.get_text() {
        return Ok(serde_json::json!({ "ok": true, "text": text }));
    }
    if clipboard.get_image().is_ok() {
        return Ok(serde_json::json!({ "ok": true, "image": true }));
    }
    Err("No supported clipboard content".into())
}

#[cfg(target_os = "android")]
pub fn copy_text(text: &str) -> bool {
    match crate::android::copy_text(text) {
        Ok(copied) => copied,
        Err(error) => {
            tracing::warn!(%error, "Couldn't copy text to the Android clipboard");
            false
        }
    }
}

#[cfg(target_os = "android")]
pub fn read() -> Result<serde_json::Value, String> {
    crate::android::read_clipboard()
}

#[cfg(target_os = "ios")]
pub fn copy_text(_text: &str) -> bool {
    tracing::warn!("Clipboard buttons are not available on mobile yet");
    false
}

#[cfg(target_os = "ios")]
pub fn read() -> Result<serde_json::Value, String> {
    Err("Clipboard buttons are not available on mobile yet.".into())
}
