//! `GridLayout` — the container component.

use std::collections::HashSet;

use dioxus::prelude::*;

use crate::collision::compact_vertical;
use crate::drag::{Interaction, InteractionKind};
use crate::layout::GridPosition;
use crate::store::LayoutStore;

/// Container that positions its `GridItem` children on a CSS grid.
///
/// # Props
/// - `cols`, `row_height`, `gap`: CSS grid configuration
/// - `editable`: when false, drag/resize are disabled regardless of `store`
/// - `store`: optional [`LayoutStore`] (positions stored + mutated reactively)
/// - `on_change`: fired with the full layout snapshot after each user-driven
///   commit (drag end, resize end). Use it to persist the layout.
#[component]
pub fn GridLayout(
    #[props(default = 12)] cols: u32,
    #[props(default = 30.0)] row_height: f64,
    #[props(default = 10.0)] gap: f64,
    #[props(default = String::new())] class: String,
    #[props(default = false)] editable: bool,
    store: Option<LayoutStore>,
    on_change: Option<EventHandler<Vec<(String, GridPosition)>>>,
    children: Element,
) -> Element {
    let drag = use_signal::<Option<Interaction>>(|| None);
    let container_width = use_signal::<Option<f64>>(|| None);
    let pinned_ids = use_signal::<HashSet<String>>(HashSet::new);
    let on_change_cb = use_hook(|| on_change.clone());

    // `editable` is a prop that may change at runtime (host toggles it).
    // `use_context_provider`'s initializer only runs once, so we hold the
    // editable state in a Signal that the GridContext exposes, and sync it
    // to the latest prop value on every render.
    let mut editable_signal = use_signal(|| editable);
    if *editable_signal.peek() != editable {
        editable_signal.set(editable);
    }

    use_context_provider(|| GridContext {
        store,
        cols,
        row_height,
        gap,
        editable: editable_signal,
        drag,
        container_width,
        pinned_ids,
        on_change: on_change_cb,
    });

    let style = format!(
        "display: grid; \
         grid-template-columns: repeat({cols}, 1fr); \
         grid-auto-rows: {row_height}px; \
         gap: {gap}px; \
         touch-action: none; \
         --grid-cols: {cols}; \
         --grid-row-height: {row_height}px; \
         --grid-gap: {gap}px;",
    );
    let editable_class = if editable { " grid-editable" } else { "" };

    rsx! {
        div {
            class: "dioxus-grid-layout{editable_class} {class}",
            style,
            onmounted: move |evt| {
                let data = evt.data();
                let mut cw = container_width;
                spawn(async move {
                    if let Ok(rect) = data.get_client_rect().await {
                        cw.set(Some(rect.size.width));
                    }
                });
            },
            {children}
            DragPlaceholder {}
        }
        DragOverlay {}
    }
}

/// Translucent box showing where the active drag will snap to. Only renders
/// during a Drag-kind interaction.
#[component]
fn DragPlaceholder() -> Element {
    let ctx: GridContext = use_context();
    let snap = ctx.drag.read().as_ref().and_then(|state| {
        if state.kind != InteractionKind::Drag {
            return None;
        }
        Some(state.project(state.pointer_current_x, state.pointer_current_y))
    });
    let Some(p) = snap else {
        return rsx! { Fragment {} };
    };

    let style = format!(
        "grid-column: {col} / span {w}; grid-row: {row} / span {h}; \
         background: rgba(99, 102, 241, 0.18); \
         border: 1.5px dashed rgba(99, 102, 241, 0.7); \
         border-radius: 8px; \
         pointer-events: none;",
        col = p.x + 1,
        row = p.y + 1,
        w = p.w,
        h = p.h,
    );
    rsx! { div { class: "dioxus-grid-placeholder", style } }
}

/// Viewport-covering overlay that survives the cursor leaving any
/// individual item. Translates pointer events into store mutations.
#[component]
fn DragOverlay() -> Element {
    let ctx: GridContext = use_context();
    let mut drag = ctx.drag;
    let store = ctx.store;

    if drag.read().is_none() {
        return rsx! { Fragment {} };
    }

    rsx! {
        div {
            class: "dioxus-grid-drag-overlay",
            style: "position: fixed; inset: 0; cursor: grabbing; z-index: 9999; touch-action: none;",
            onpointermove: move |evt| {
                let (cx, cy) = (evt.client_coordinates().x, evt.client_coordinates().y);

                // Snapshot kind + id + delta so we can release the read lock
                // before reaching for write.
                let (kind, item_id, dx, dy, projected) = {
                    let Some(state) = drag.read().clone() else { return };
                    let dx = cx - state.pointer_start_x;
                    let dy = cy - state.pointer_start_y;
                    let projected = state.project(cx, cy);
                    (state.kind, state.item_id, dx, dy, projected)
                };

                // Always keep current pointer up to date so placeholder snaps.
                drag.with_mut(|d| {
                    if let Some(s) = d.as_mut() {
                        s.pointer_current_x = cx;
                        s.pointer_current_y = cy;
                    }
                });

                match kind {
                    InteractionKind::Drag => {
                        // Smooth: move the item directly via CSS transform,
                        // bypassing Dioxus's render. Layout commit happens on
                        // pointerup.
                        let js = format!(
                            "var el=document.querySelector('[data-id=\"{}\"]');\
                             if(el){{el.style.transform='translate({:.2}px,{:.2}px)';\
                             el.style.zIndex='1000';}}",
                            item_id, dx, dy,
                        );
                        let _ = document::eval(&js);

                        // Collision resolution: pretend the active item is
                        // at its projected snap cell, recompute non-active
                        // positions, push results back into the store.
                        if let Some(store) = store {
                            let pinned = ctx.pinned_ids.read().clone();
                            settle_layout(store, &item_id, projected, &pinned);
                        }
                    }
                    InteractionKind::Resize => {
                        if let Some(mut s) = store {
                            s.set(item_id.clone(), projected);
                        }
                        // After resize commit, also let neighbours reflow.
                        if let Some(store) = store {
                            let pinned = ctx.pinned_ids.read().clone();
                            settle_layout(store, &item_id, projected, &pinned);
                        }
                    }
                }
            },
            onpointerup: move |_| { commit_and_clear(&mut drag, store, ctx.on_change); },
            onpointercancel: move |_| { commit_and_clear(&mut drag, store, ctx.on_change); },
        }
    }
}

/// Recompute non-active item positions given the active item's intended
/// position, then write any changed positions back to the store.
fn settle_layout(
    mut store: LayoutStore,
    active_id: &str,
    active_pos: GridPosition,
    pinned: &HashSet<String>,
) {
    let mut layout = store.snapshot();
    let mut found = false;
    for (id, p) in layout.iter_mut() {
        if id == active_id {
            *p = active_pos;
            found = true;
            break;
        }
    }
    if !found {
        layout.push((active_id.to_string(), active_pos));
    }

    // Immovable = the active item PLUS any user-pinned items.
    let mut immovable: HashSet<String> = pinned.clone();
    immovable.insert(active_id.to_string());

    compact_vertical(&mut layout, &immovable);

    for (id, new_pos) in layout {
        if id == active_id {
            continue;
        }
        if store.get(&id) != Some(new_pos) {
            store.set(id, new_pos);
        }
    }
}

fn commit_and_clear(
    drag: &mut Signal<Option<Interaction>>,
    store: Option<LayoutStore>,
    on_change: Option<EventHandler<Vec<(String, GridPosition)>>>,
) {
    let Some(state) = drag.read().clone() else { return };
    // Clear smooth-drag transform / z-index regardless of kind.
    let js = format!(
        "var el=document.querySelector('[data-id=\"{}\"]');\
         if(el){{el.style.transform='';el.style.zIndex='';}}",
        state.item_id,
    );
    let _ = document::eval(&js);

    // Commit the snapped position (drag) — resize already committed live.
    if matches!(state.kind, InteractionKind::Drag) {
        if let Some(mut s) = store {
            let projected = state.project(state.pointer_current_x, state.pointer_current_y);
            s.set(state.item_id.clone(), projected);
        }
    }
    drag.set(None);

    // Fire on_change with the final settled snapshot.
    if let (Some(handler), Some(s)) = (on_change, store) {
        handler.call(s.snapshot());
    }
}

/// Internal: GridLayout settings + shared interaction state, exposed to
/// GridItem via Dioxus context.
#[derive(Clone, Copy)]
pub(crate) struct GridContext {
    pub store: Option<LayoutStore>,
    pub cols: u32,
    pub row_height: f64,
    pub gap: f64,
    /// Reactive — host can toggle this at any time and GridItems see it
    /// without remounting.
    pub editable: Signal<bool>,
    pub drag: Signal<Option<Interaction>>,
    pub container_width: Signal<Option<f64>>,
    pub pinned_ids: Signal<HashSet<String>>,
    pub on_change: Option<EventHandler<Vec<(String, GridPosition)>>>,
}

impl GridContext {
    pub fn cell_w_px(&self) -> Option<f64> {
        let total = self.container_width.read().as_ref().copied()?;
        let inner_gap = self.gap * (self.cols.saturating_sub(1) as f64);
        let cell_w = (total - inner_gap) / (self.cols as f64);
        if cell_w > 0.0 { Some(cell_w) } else { None }
    }
}

#[allow(dead_code)]
pub(crate) fn _initial_position(x: u32, y: u32, w: u32, h: u32) -> GridPosition {
    GridPosition::new(x, y, w, h)
}
