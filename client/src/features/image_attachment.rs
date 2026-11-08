use dioxus::prelude::*;

/// A chat picture holds this height from before it loads, so nothing below it
/// moves when it does; width follows the picture, which never moves the chat.
pub(super) const FRAME: &str = "height: 240px; max-width: min(480px, 82vw);";

#[component]
pub(super) fn ImageAttachment(
    source: crate::image_assets::ImageSource,
    zoomable: bool,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    let title = if zoomable {
        crate::i18n::text("Click to view full size", &[])
    } else {
        String::new()
    };
    rsx! {img {
        class:"mt-1 rounded-lg border border-[var(--border-strong)] bg-[var(--panel2)] block hover:border-[var(--accent)] hover:brightness-105 transition-all shadow-sm",
        style: format!("{FRAME} width: auto; object-fit: scale-down;{}", if zoomable { " cursor: zoom-in;" } else { " cursor: default;" }),
        src:"{source}",
        alt:crate::i18n::text("attachment",&[]),
        title:"{title}",
        onclick:move |event| if zoomable { onclick.call(event) },
    }}
}
