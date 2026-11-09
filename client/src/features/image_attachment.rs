use dioxus::prelude::*;

/// The box a chat picture holds from before it loads, so nothing below it moves
/// when it does: its own shape fitted into 400×300, never enlarged. A picture
/// with no stored size gets a fixed height instead. Every branch names the same
/// properties, since one left out of a new style is restored (trap 31).
pub(super) fn frame(size: Option<(u32, u32)>) -> String {
    match size.filter(|(w, h)| *w > 0 && *h > 0) {
        Some((w, h)) => {
            let scale = (400.0 / f64::from(w)).min(300.0 / f64::from(h)).min(1.0);
            let width = (f64::from(w) * scale).round().max(1.0);
            format!("width: {width}px; height: auto; aspect-ratio: {w} / {h}; max-width: 100%;")
        }
        None => "width: 280px; height: 240px; aspect-ratio: auto; max-width: 100%;".into(),
    }
}

#[component]
pub(super) fn ImageAttachment(
    source: crate::image_assets::ImageSource,
    size: Option<(u32, u32)>,
    zoomable: bool,
    onclick: EventHandler<MouseEvent>,
) -> Element {
    let title = if zoomable {
        crate::i18n::text("Click to view full size", &[])
    } else {
        String::new()
    };
    // Without a stored size the width follows the picture; the height is what holds.
    let width = if size.is_some() {
        ""
    } else {
        " width: auto; max-width: min(480px, 100%);"
    };
    rsx! {img {
        class:"mt-1 rounded-lg border border-[var(--border-strong)] bg-[var(--panel2)] block hover:border-[var(--accent)] hover:brightness-105 transition-all shadow-sm",
        style: format!("{}{width} object-fit: scale-down;{}", frame(size), if zoomable { " cursor: zoom-in;" } else { " cursor: default;" }),
        src:"{source}",
        alt:crate::i18n::text("attachment",&[]),
        title:"{title}",
        onclick:move |event| if zoomable { onclick.call(event) },
    }}
}
