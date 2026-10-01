use std::cell::RefCell;

thread_local! {
    // X11 serves clipboard contents from the owner, so it must outlive the copy operation.
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
}

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
