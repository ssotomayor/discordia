use dioxus::prelude::*;

#[component]
pub(super) fn ImageAttachment(
    source: crate::image_assets::ImageSource,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    rsx! {img {
        class:"mt-1 rounded-lg border border-[var(--border-strong)] bg-[var(--panel2)] object-contain block hover:border-[var(--accent)] hover:brightness-105 transition-all shadow-sm",
        style:"max-width: min(320px, 82vw); max-height: 240px; width: auto; height: auto; cursor: zoom-in;",
        src:"{source}",
        alt:crate::i18n::text("attachment",&[]),
        title:crate::i18n::text("Click to view full size",&[]),
        onclick:move |event|onclick.call(event),
    }}
}
