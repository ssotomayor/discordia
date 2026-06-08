//! `GridItem` — a single tile inside a `GridLayout`.

use dioxus::prelude::*;
use dioxus_elements::input_data::MouseButton;

use crate::drag::DragState;
use crate::grid::GridContext;
use crate::layout::GridPosition;

/// Positions a child element at `(x, y)` and gives it `w` columns × `h` rows.
///
/// When the parent `GridLayout` has a `store`, drag/resize interactions
/// mutate the store and re-render the item at its new position. Without a
/// store the props are used directly (static render).
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
    let pos = resolve_pos(&id, ctx, GridPosition { x, y, w, h });

    let style = format!(
        "grid-column: {col} / span {w}; grid-row: {row} / span {h};",
        col = pos.x + 1,
        row = pos.y + 1,
        w = pos.w,
        h = pos.h,
    );
    let pinned_class = if pinned { " grid-item-pinned" } else { "" };
    let editable = ctx.map(|c| c.editable).unwrap_or(false);
    let cursor_style = if editable && !pinned { " cursor: grab;" } else { "" };
    let _ = (min_w, min_h); // reserved for resize logic in commit 3

    let item_id_for_handler = id.clone();
    let onpointerdown = move |evt: PointerEvent| {
        let Some(ctx) = ctx else { return };
        if pinned || !ctx.editable {
            return;
        }
        // Only respond to primary button presses (left mouse / single finger).
        if !evt.held_buttons().contains(MouseButton::Primary) {
            return;
        }
        // Cell pixel size relies on the container measurement landing —
        // skip the drag if we haven't measured yet (very early frame).
        let Some(cell_w) = ctx.cell_w_px() else { return };

        let current = ctx
            .store
            .and_then(|s| s.get(&item_id_for_handler))
            .unwrap_or(GridPosition { x, y, w, h });

        let state = DragState {
            item_id: item_id_for_handler.clone(),
            start_pos: current,
            pointer_start_x: evt.client_coordinates().x,
            pointer_start_y: evt.client_coordinates().y,
            cell_w_px: cell_w,
            cell_h_px: ctx.row_height,
            gap_px: ctx.gap,
            cols: ctx.cols,
        };
        let mut drag = ctx.drag;
        drag.set(Some(state));
    };

    rsx! {
        div {
            class: "dioxus-grid-item{pinned_class} {class}",
            "data-id": "{id}",
            style: "{style}{cursor_style}",
            onpointerdown,
            {children}
        }
    }
}

fn resolve_pos(id: &str, ctx: Option<GridContext>, fallback: GridPosition) -> GridPosition {
    let Some(ctx) = ctx else { return fallback };
    match ctx.store {
        Some(mut store) => match store.get(id) {
            Some(p) => p,
            None => {
                // Seed the store with this item's initial position on first
                // render. Subsequent renders read the live store value.
                store.set(id.to_string(), fallback);
                fallback
            }
        },
        None => fallback,
    }
}
