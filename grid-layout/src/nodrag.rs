//! A no-op now that panel content no longer raises drags. Kept so existing
//! call sites compile unchanged.

use dioxus::prelude::*;

#[component]
pub fn NoDrag(children: Element) -> Element {
    rsx! {
        div {
            class: "dioxus-grid-no-drag",
            style: "display: contents;",
            {children}
        }
    }
}
