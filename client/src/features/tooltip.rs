use dioxus::prelude::*;

/// A label that appears the moment the pointer arrives. The browser's `title`
/// waits a second and draws in the system's style, so nothing here uses one.
#[component]
pub fn Tooltip(text: String, children: Element) -> Element {
    let mut shown = use_signal(|| false);
    rsx! {
        span {
            class: "relative inline-flex cursor-pointer",
            onmouseenter: move |_| shown.set(true),
            onmouseleave: move |_| shown.set(false),
            {children}
            if shown() {
                span {
                    class: "absolute bottom-full left-1/2 -translate-x-1/2 mb-1 z-50 px-2 py-1 rounded-md whitespace-nowrap pointer-events-none bg-[var(--panel-solid)] border border-[var(--border)] shadow-lg text-[11px] font-medium text-[var(--text)]",
                    "{text}"
                }
            }
        }
    }
}
