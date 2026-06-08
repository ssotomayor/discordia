//! `NoDrag` — wrap interactive children inside a `GridItem` to stop a
//! pointerdown from starting a drag.

use dioxus::prelude::*;

/// Children of `NoDrag` keep their own `onpointerdown` / `onclick`
/// handlers, but pointerdown events stop propagating to the parent
/// `GridItem` so dragging never starts here. Use this around buttons,
/// inputs, links, and any other interactive content inside a tile.
///
/// The wrapper uses `display: contents` so it doesn't introduce any
/// CSS layout. Children render as if they were direct siblings.
///
/// ```ignore
/// GridItem { id: "users", ...,
///     div { class: "header", "Users" }  // draggable
///     NoDrag {
///         button { onclick: |_| println!("clicked"), "Add user" }
///     }
/// }
/// ```
#[component]
pub fn NoDrag(children: Element) -> Element {
    rsx! {
        div {
            class: "dioxus-grid-no-drag",
            style: "display: contents;",
            onpointerdown: |evt| evt.stop_propagation(),
            {children}
        }
    }
}
