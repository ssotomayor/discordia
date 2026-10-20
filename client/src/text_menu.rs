use dioxus::prelude::*;
use serde_json::{Value, json};

#[component]
pub fn TextMenu() -> Element {
    use_drop(move || {
        document::eval("window.__dxfTextMenuOff?.();");
    });
    use_effect(move || {
        let labels = json!({
            "copy": crate::i18n::text("Copy", &[]),
            "react": crate::i18n::text("Add reaction", &[]),
            "message": crate::i18n::text("Copy message", &[]),
            "cut": crate::i18n::text("Cut", &[]),
            "paste": crate::i18n::text("Paste", &[]),
            "all": crate::i18n::text("Select all", &[]),
            "error": crate::i18n::text("Could not access the clipboard.", &[]),
        });
        document::eval(&format!("window.__dxfTextMenuLabels = {labels};"));
    });
    use_future(move || async move {
        let mut eval = document::eval(include_str!("../assets/text-menu.js"));
        while let Ok(request) = eval.recv::<Value>().await {
            let response = match request["action"].as_str() {
                Some("copy") => json!({
                    "ok": request["text"].as_str().is_some_and(crate::clipboard::copy_text),
                }),
                Some("paste") => {
                    let result = tokio::task::spawn_blocking(|| {
                        let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
                        if let Ok(text) = clipboard.get_text() {
                            return Ok(json!({ "ok": true, "text": text }));
                        }
                        if clipboard.get_image().is_ok() {
                            return Ok(json!({ "ok": true, "image": true }));
                        }
                        Err::<Value, String>("No supported clipboard content".into())
                    })
                    .await;
                    match result {
                        Ok(Ok(value)) => value,
                        _ => json!({ "ok": false }),
                    }
                }
                _ => json!({ "ok": false }),
            };
            if eval.send(response).is_err() {
                break;
            }
        }
    });
    rsx! {
        document::Style { {include_str!("../assets/text-menu.css")} }
    }
}
