use dioxus::prelude::*;

// Capture phase on the document, not a `fixed inset-0` overlay: inside a raised grid
// panel an overlay covers only that panel (trap 22), so a press on another missed it.
const DISMISS_JS: &str = r#"
const key = __KEY__;
window.__dxfDismiss = window.__dxfDismiss || {};
if (window.__dxfDismiss[key]) window.__dxfDismiss[key]();
const down = e => {
  const t = e.target;
  if (!(t && t.closest && t.closest('[data-dismiss="' + key + '"]'))) dioxus.send(true);
};
document.addEventListener('pointerdown', down, true);
window.__dxfDismiss[key] = () => {
  document.removeEventListener('pointerdown', down, true);
  delete window.__dxfDismiss[key];
};
"#;

fn remove(key: &str) {
    let key = serde_json::to_string(key).unwrap_or_default();
    let _ = document::eval(&format!(
        "window.__dxfDismiss && window.__dxfDismiss[{key}] && window.__dxfDismiss[{key}]();"
    ));
}

/// Closes `open` on a press outside every element carrying `data-dismiss` set to
/// the returned key — the popover and the button that toggles it.
pub(crate) fn use_dismiss(mut open: Signal<bool>) -> String {
    let key = use_hook(|| format!("dxf-dismiss-{}", uuid::Uuid::new_v4().simple()));
    let mut listening = use_signal(|| None::<dioxus::core::Task>);
    let effect_key = key.clone();
    use_effect(move || {
        let is_open = open();
        if let Some(task) = listening.write().take() {
            task.cancel();
        }
        if !is_open {
            remove(&effect_key);
            return;
        }
        let js = DISMISS_JS.replace(
            "__KEY__",
            &serde_json::to_string(&effect_key).unwrap_or_default(),
        );
        let mut eval = document::eval(&js);
        listening.set(Some(spawn(async move {
            while let Ok(true) = eval.recv::<bool>().await {
                open.set(false);
            }
        })));
    });
    let drop_key = key.clone();
    use_drop(move || remove(&drop_key));
    key
}
