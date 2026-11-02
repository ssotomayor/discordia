use dioxus::prelude::*;

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
        class:"mt-1 rounded-lg border border-[var(--border-strong)] bg-[var(--panel2)] object-contain block hover:border-[var(--accent)] hover:brightness-105 transition-all shadow-sm",
        style: if zoomable { "max-width: min(320px, 82vw); max-height: 240px; width: auto; height: auto; cursor: zoom-in;" } else { "max-width: min(320px, 82vw); max-height: 240px; width: auto; height: auto;" },
        src:"{source}",
        alt:crate::i18n::text("attachment",&[]),
        title:"{title}",
        onclick:move |event| if zoomable { onclick.call(event) },
    }}
}
