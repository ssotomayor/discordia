use dioxus::prelude::*;
use dioxus_elements::input_data::MouseButton;

use crate::drag::{Interaction, InteractionKind};
use crate::grid::GridContext;
use crate::layout::{FloatRect, GridPosition};

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
    #[cfg_attr(not(debug_assertions), allow(unused_variables))]
    let id_for_log = id.clone();
    let onpointerdown_drag = move |evt: PointerEvent| {
        let Some(ctx) = ctx else {
            gtrace!("[grid] {id_for_log}: pointerdown but no GridContext");
            return;
        };
        if !interactive {
            gtrace!(
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
            gtrace!(
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
            onpointerdown: onpointerdown_raise,
            {children}
            if interactive {
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

fn free_style(id: &str, ctx: GridContext, cell: GridPosition) -> String {
    let z = ctx.store.map(|s| s.z_of(id)).unwrap_or(0);
    // Omitted at 0: any z-index establishes a stacking context, which traps
    // fixed-position children inside the panel.
    let z_rule = if z > 0 {
        format!(" z-index: {z};")
    } else {
        String::new()
    };

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
