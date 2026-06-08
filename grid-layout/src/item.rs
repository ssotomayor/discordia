//! `GridItem` — a single tile inside a `GridLayout`.

use dioxus::prelude::*;

use crate::grid::GridContext;
use crate::layout::GridPosition;

/// Positions a child element at `(x, y)` and gives it `w` columns × `h` rows.
///
/// When the parent `GridLayout` has a `store`, the item registers its initial
/// position (from props) into the store on mount and then reads its live
/// position back from the store on every render. When no store is present,
/// the props are used directly (v0.0.1 static-render behavior).
///
/// # Props
///
/// - `id`: unique identifier within the parent grid
/// - `x`, `y`: initial grid cell origin (0-based)
/// - `w`, `h`: initial width/height in grid units
/// - `min_w`, `min_h`: enforced lower bounds when resizing (used by resize logic)
/// - `pinned`: when true, the item cannot be moved or resized
/// - `class`: extra classes appended to the item's `class` attribute
#[component]
pub fn GridItem(
    #[props(into)] id: String,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    #[props(default = 1)] min_w: u32,
    #[props(default = 1)] min_h: u32,
    #[props(default = false)] pinned: bool,
    #[props(default = String::new())] class: String,
    children: Element,
) -> Element {
    let ctx: Option<GridContext> = try_consume_context();

    // Resolve the live position: store first, then props as initial.
    let pos = match ctx.and_then(|c| c.store) {
        Some(mut store) => {
            // First render with a store: seed it from props if the id isn't
            // present yet.
            let existing = store.get(&id);
            match existing {
                Some(p) => p,
                None => {
                    let p = GridPosition { x, y, w, h };
                    store.set(id.clone(), p);
                    p
                }
            }
        }
        None => GridPosition { x, y, w, h },
    };

    let style = format!(
        "grid-column: {col_start} / span {w}; grid-row: {row_start} / span {h};",
        col_start = pos.x + 1,
        row_start = pos.y + 1,
        w = pos.w,
        h = pos.h,
    );
    let pinned_class = if pinned { " grid-item-pinned" } else { "" };
    let _ = (min_w, min_h); // reserved for resize logic in commit 3

    rsx! {
        div {
            class: "dioxus-grid-item{pinned_class} {class}",
            "data-id": "{id}",
            style,
            {children}
        }
    }
}
