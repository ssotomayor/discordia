//! `GridItem` — a single tile inside a `GridLayout`.

use dioxus::prelude::*;
use dioxus_elements::input_data::MouseButton;

use crate::drag::{Interaction, InteractionKind};
use crate::grid::GridContext;
use crate::layout::{FloatRect, GridPosition};

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

    let cell_style = match ctx.filter(|c| c.is_free()) {
        Some(c) => free_style(&id, c, pos),
        None => format!(
            "grid-column: {col} / span {w}; grid-row: {row} / span {h}; position: relative;",
            col = pos.x + 1,
            row = pos.y + 1,
            w = pos.w,
            h = pos.h,
        ),
    };
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
                "[grid] {id_for_log}: pointerdown but container not yet measured \
                 (container_size={:?})",
                ctx.container_size.read()
            );
            return;
        };

        let current = ctx
            .store
            .and_then(|s| s.get(&item_id_for_drag))
            .unwrap_or(GridPosition { x, y, w, h });
        // Click-to-front, so the window you grab is the one on top.
        if let (true, Some(mut s)) = (ctx.is_free(), ctx.store) {
            s.raise(&item_id_for_drag);
        }

        let state = Interaction {
            kind: InteractionKind::Drag,
            item_id: item_id_for_drag.clone(),
            start_pos: current,
            start_free: ctx
                .store
                .and_then(|s| s.get_free(&item_id_for_drag))
                .or_else(|| cell_to_px(ctx, current)),
            pointer_start_x: evt.client_coordinates().x,
            pointer_start_y: evt.client_coordinates().y,
            pointer_current_x: evt.client_coordinates().x,
            pointer_current_y: evt.client_coordinates().y,
            cell_w_px: cell_w,
            cell_h_px: ctx.cell_h_px(),
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
            start_free: ctx
                .store
                .and_then(|s| s.get_free(&item_id_for_resize))
                .or_else(|| cell_to_px(ctx, current)),
            pointer_start_x: evt.client_coordinates().x,
            pointer_start_y: evt.client_coordinates().y,
            pointer_current_x: evt.client_coordinates().x,
            pointer_current_y: evt.client_coordinates().y,
            cell_w_px: cell_w,
            cell_h_px: ctx.cell_h_px(),
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

/// Absolute placement CSS for free mode.
///
/// Always returns an absolute rule — never grid properties. That distinction
/// matters more than it looks: in free mode the container is
/// `position: relative`, not a grid, so emitting `grid-column`/`grid-row` there
/// leaves an item with no position and no size, and every panel collapses into
/// a pile in the top-left corner. Falling back to grid CSS in a non-grid
/// container is what "the layout completely breaks" looked like.
///
/// An item the user has never dragged has no pixel rect yet, and rather than
/// invent one (or write to the store mid-render, which is its own bug) it is
/// placed by *percentage* derived from its grid cell. That needs no
/// measurement, can never be off-screen, and lands exactly where the snap
/// layout had it — so switching to Free looks like nothing moved.
fn free_style(id: &str, ctx: GridContext, cell: GridPosition) -> String {
    let z = ctx.store.map(|s| s.z_of(id)).unwrap_or(0);

    if let Some(rect) = ctx.store.and_then(|s| s.get_free(id)) {
        return format!(
            "position: absolute; left: {:.1}px; top: {:.1}px; \
             width: {:.1}px; height: {:.1}px; z-index: {z};",
            rect.x, rect.y, rect.w, rect.h,
        );
    }

    let cols = ctx.cols.max(1) as f64;
    // Without a row count, treat the item's own extent as the full height —
    // the best available guess, and only ever used before the first drag.
    let rows = ctx.rows.unwrap_or_else(|| cell.y + cell.h).max(1) as f64;
    let gap = ctx.gap;
    format!(
        "position: absolute; left: {:.4}%; top: {:.4}%; \
         width: calc({:.4}% - {gap}px); height: calc({:.4}% - {gap}px); z-index: {z};",
        cell.x as f64 / cols * 100.0,
        cell.y as f64 / rows * 100.0,
        cell.w as f64 / cols * 100.0,
        cell.h as f64 / rows * 100.0,
    )
}

/// Pixel rect for a grid cell, for starting a free-mode drag on an item that
/// has only ever been placed by percentage. `None` until the container has
/// been measured.
fn cell_to_px(ctx: GridContext, cell: GridPosition) -> Option<FloatRect> {
    let cell_w = ctx.cell_w_px()?;
    let cell_h = ctx.cell_h_px();
    let gap = ctx.gap;
    Some(FloatRect {
        x: cell.x as f64 * (cell_w + gap),
        y: cell.y as f64 * (cell_h + gap),
        w: cell.w as f64 * cell_w + (cell.w.saturating_sub(1) as f64) * gap,
        h: cell.h as f64 * cell_h + (cell.h.saturating_sub(1) as f64) * gap,
    })
}

/// How much of a window must stay reachable inside the container.
pub(crate) const MIN_VISIBLE_PX: f64 = 64.0;

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
