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

    let item_id_for_raise = id.clone();
    let onpointerdown_raise = move |_: PointerEvent| {
        let Some(ctx) = ctx.filter(|c| c.is_free()) else {
            return;
        };
        if let Some(mut s) = ctx.store {
            s.raise(&item_id_for_raise);
        }
    };

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
        let state = Interaction {
            kind: InteractionKind::Drag,
            item_id: item_id_for_drag.clone(),
            start_pos: current,
            start_free: ctx
                .store
                .and_then(|s| s.get_free(&item_id_for_drag))
                .or(Some(cell_to_frac(ctx, current))),
            pointer_start_x: evt.client_coordinates().x,
            pointer_start_y: evt.client_coordinates().y,
            pointer_current_x: evt.client_coordinates().x,
            pointer_current_y: evt.client_coordinates().y,
            cell_w_px: cell_w,
            cell_h_px: ctx.cell_h_px(),
            container_w: ctx.container_rect().map(|(w, _)| w).unwrap_or(0.0),
            container_h: ctx.container_rect().map(|(_, h)| h).unwrap_or(0.0),
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
        let Some(cell_w) = ctx.cell_w_px() else {
            return;
        };

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
                .or(Some(cell_to_frac(ctx, current))),
            pointer_start_x: evt.client_coordinates().x,
            pointer_start_y: evt.client_coordinates().y,
            pointer_current_x: evt.client_coordinates().x,
            pointer_current_y: evt.client_coordinates().y,
            cell_w_px: cell_w,
            cell_h_px: ctx.cell_h_px(),
            container_w: ctx.container_rect().map(|(w, _)| w).unwrap_or(0.0),
            container_h: ctx.container_rect().map(|(_, h)| h).unwrap_or(0.0),
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
            style: "{cell_style}",
            // Raise on any press, any button, edit mode or not. Two reasons:
            // clicking a window bringing it forward is what windows do, and
            // popovers *inside* a panel (a right-click menu, say) paint within
            // that panel's stacking order — so without this they end up behind
            // whichever panel happens to come later in the DOM.
            onpointerdown: onpointerdown_raise,
            {children}
            if interactive {
                // Drag handle: a strip along the top, not the whole panel.
                // Making the entire surface draggable meant a grab cursor over
                // every widget and no way to use the content while the layout
                // was unlocked. A title-bar strip is where people expect to
                // pick a window up from anyway.
                div {
                    class: "dioxus-grid-drag-handle",
                    style: "position: absolute; left: 0; right: 0; top: 0; \
                            height: 22px; cursor: grab; z-index: 2; \
                            background: linear-gradient(180deg, \
                              color-mix(in srgb, currentColor 10%, transparent), transparent); \
                            border-top-left-radius: inherit; \
                            border-top-right-radius: inherit;",
                    title: "Drag to move",
                    onpointerdown: onpointerdown_drag,
                }
                div {
                    class: "dioxus-grid-resize-handle",
                    style: "position: absolute; right: 0; bottom: 0; \
                            width: 14px; height: 14px; z-index: 2; \
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
    // Only emit z-index once the panel has actually been raised. `z-index: 0`
    // is not free — it establishes a stacking context, which traps every
    // popover rendered inside the panel so it cannot paint above a sibling
    // panel. An untouched panel therefore gets no z-index at all and behaves
    // exactly as it did before free mode existed.
    let z = ctx.store.map(|s| s.z_of(id)).unwrap_or(0);
    let z_rule = if z > 0 {
        format!(" z-index: {z};")
    } else {
        String::new()
    };

    // Stored by hand, or derived from the grid cell the item started life in.
    let rect = ctx
        .store
        .and_then(|s| s.get_free(id))
        .unwrap_or_else(|| cell_to_frac(ctx, cell));
    let gap = ctx.gap;
    format!(
        "position: absolute; left: {:.4}%; top: {:.4}%; \
         width: calc({:.4}% - {gap}px); height: calc({:.4}% - {gap}px);{z_rule}",
        rect.x * 100.0,
        rect.y * 100.0,
        rect.w * 100.0,
        rect.h * 100.0,
    )
}

/// Fractional rect for a grid cell — the starting point for the first drag of
/// an item that has never been placed by hand.
pub(crate) fn cell_to_frac(ctx: GridContext, cell: GridPosition) -> FloatRect {
    let cols = ctx.cols.max(1) as f64;
    let rows = ctx.rows.unwrap_or_else(|| cell.y + cell.h).max(1) as f64;
    FloatRect {
        x: cell.x as f64 / cols,
        y: cell.y as f64 / rows,
        w: cell.w as f64 / cols,
        h: cell.h as f64 / rows,
    }
    .clamp_inside()
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
