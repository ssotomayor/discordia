//! `NoDrag` — a passthrough kept for source compatibility.
//!
//! It used to stop `pointerdown` propagating to the parent `GridItem`, back
//! when the whole tile was the drag surface and any press inside it would start
//! a drag. Dragging now begins only from the tile's title strip, which does not
//! overlap content, so there is nothing left to guard against.
//!
//! Swallowing the event turned out to be actively harmful: `GridItem` raises
//! its tile to the front on pointerdown, and a stopped event meant clicks on
//! panel content — which is nearly all of it — never raised anything. Dialogs
//! and popovers rendered inside a tile then painted underneath whichever tile
//! came later in the DOM.
//!
//! Kept rather than deleted so existing call sites compile unchanged; it now
//! renders its children and nothing else.

use dioxus::prelude::*;

/// Renders `children` unchanged. See the module docs — this is a no-op today.
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
