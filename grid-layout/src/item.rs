//! `GridItem` — a single tile inside a `GridLayout`.

use dioxus::prelude::*;
use dioxus_elements::input_data::MouseButton;

use crate::drag::{Interaction, InteractionKind};
use crate::grid::GridContext;
use crate::layout::GridPosition;

/// Positions a child element at `(x, y)` and gives it `w` columns × `h` rows.
///
/// When the parent `GridLayout` has a `store`, drag and resize interactions
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

    // Keep the GridContext-level pinned set in sync with this item's prop.
    if let Some(ctx) = ctx {
        let id_for_effect = id.clone();
        use_effect(move || {
            let mut ids = ctx.pinned_ids;
            let present = ids.read().contains(&id_for_effect);
            if pinned && !present {
                ids.write().insert(id_for_effect.clone());
            } else if !pinned && present {
                ids.write().remove(&id_for_effect);
            }
        });
        let id_for_drop = id.clone();
        let mut pinned_ids_for_drop = ctx.pinned_ids;
        use_drop(move || {
            pinned_ids_for_drop.write().remove(&id_for_drop);
        });
    }

    let pos = resolve_pos(&id, ctx, GridPosition { x, y, w, h });

    let cell_style = format!(
        "grid-column: {col} / span {w}; grid-row: {row} / span {h}; position: relative;",
        col = pos.x + 1,
        row = pos.y + 1,
        w = pos.w,
        h = pos.h,
    );
    let pinned_class = if pinned { " grid-item-pinned" } else { "" };
    let editable = ctx.map(|c| *c.editable.read()).unwrap_or(false);
    let interactive = editable && !pinned;
    let drag_cursor = if interactive { " cursor: grab;" } else { "" };

    let item_id_for_drag = id.clone();
    let id_for_log = id.clone();
    let onpointerdown_drag = move |evt: PointerEvent| {
        let Some(ctx) = ctx else {
            eprintln!("[grid] {id_for_log}: pointerdown but no GridContext");
            return;
        };
        if !interactive {
            eprintln!(
                "[grid] {id_for_log}: pointerdown ignored (editable={} pinned={})",
                *ctx.editable.read(),
                pinned
            );
            return;
        }
        if !evt.held_buttons().contains(MouseButton::Primary) {
            return;
        }
        let Some(cell_w) = ctx.cell_w_px() else {
            eprintln!(
                "[grid] {id_for_log}: pointerdown but cell width not yet measured \
                 (container_width={:?})",
                ctx.container_width.read()
            );
            return;
        };
        eprintln!("[grid] {id_for_log}: drag started, cell_w={cell_w:.1}px");

        let current = ctx
            .store
            .and_then(|s| s.get(&item_id_for_drag))
            .unwrap_or(GridPosition { x, y, w, h });

        let state = Interaction {
            kind: InteractionKind::Drag,
            item_id: item_id_for_drag.clone(),
            start_pos: current,
            pointer_start_x: evt.client_coordinates().x,
            pointer_start_y: evt.client_coordinates().y,
            pointer_current_x: evt.client_coordinates().x,
            pointer_current_y: evt.client_coordinates().y,
            cell_w_px: cell_w,
            cell_h_px: ctx.row_height,
            gap_px: ctx.gap,
            cols: ctx.cols,
            min_w,
            min_h,
        };
        let mut drag = ctx.drag;
        drag.set(Some(state));
    };

    let item_id_for_resize = id.clone();
    let onpointerdown_resize = move |evt: PointerEvent| {
        let Some(ctx) = ctx else { return };
        if !interactive {
            return;
        }
        if !evt.held_buttons().contains(MouseButton::Primary) {
            return;
        }
        evt.stop_propagation();
        let Some(cell_w) = ctx.cell_w_px() else { return };

        let current = ctx
            .store
            .and_then(|s| s.get(&item_id_for_resize))
            .unwrap_or(GridPosition { x, y, w, h });

        let state = Interaction {
            kind: InteractionKind::Resize,
            item_id: item_id_for_resize.clone(),
            start_pos: current,
            pointer_start_x: evt.client_coordinates().x,
            pointer_start_y: evt.client_coordinates().y,
            pointer_current_x: evt.client_coordinates().x,
            pointer_current_y: evt.client_coordinates().y,
            cell_w_px: cell_w,
            cell_h_px: ctx.row_height,
            gap_px: ctx.gap,
            cols: ctx.cols,
            min_w,
            min_h,
        };
        let mut drag = ctx.drag;
        drag.set(Some(state));
    };

    rsx! {
        div {
            class: "dioxus-grid-item{pinned_class} {class}",
            "data-id": "{id}",
            style: "{cell_style}{drag_cursor}",
            onpointerdown: onpointerdown_drag,
            {children}
            if interactive {
                div {
                    class: "dioxus-grid-resize-handle",
                    style: "position: absolute; right: 0; bottom: 0; \
                            width: 14px; height: 14px; \
                            cursor: nwse-resize; \
                            background: linear-gradient(135deg, transparent 0%, transparent 50%, rgba(255,255,255,0.45) 50%, rgba(255,255,255,0.45) 100%); \
                            border-bottom-right-radius: inherit;",
                    onpointerdown: onpointerdown_resize,
                }
            }
        }
    }
}

fn resolve_pos(id: &str, ctx: Option<GridContext>, fallback: GridPosition) -> GridPosition {
    let Some(ctx) = ctx else { return fallback };
    match ctx.store {
        Some(mut store) => match store.get(id) {
            Some(p) => p,
            None => {
                store.set(id.to_string(), fallback);
                fallback
            }
        },
        None => fallback,
    }
}
