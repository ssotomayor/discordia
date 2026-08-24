use std::collections::HashSet;

use dioxus::prelude::*;

use crate::collision::compact_vertical;
use crate::drag::{Interaction, InteractionKind};
use crate::layout::{GridPosition, LayoutMode};
use crate::store::LayoutStore;

#[component]
pub fn GridLayout(
    #[props(default = 12)] cols: u32,
    #[props(default = 30.0)] row_height: f64,
    rows: Option<u32>,
    #[props(default = 10.0)] gap: f64,
    #[props(default = String::new())] class: String,
    #[props(default = false)] editable: bool,
    #[props(default = LayoutMode::Snap)] mode: LayoutMode,
    store: Option<LayoutStore>,
    on_change: Option<EventHandler<Vec<(String, GridPosition)>>>,
    children: Element,
) -> Element {
    let drag = use_signal::<Option<Interaction>>(|| None);
    let container_size = use_signal::<Option<(f64, f64)>>(|| None);
    let pinned_ids = use_signal::<HashSet<String>>(HashSet::new);
    let on_change_cb = use_hook(|| on_change);

    let mut editable_signal = use_signal(|| editable);
    if *editable_signal.peek() != editable {
        editable_signal.set(editable);
    }

    let mut mode_signal = use_signal(|| mode);
    if *mode_signal.peek() != mode {
        mode_signal.set(mode);
    }

    use_context_provider(|| GridContext {
        store,
        cols,
        row_height,
        rows,
        gap,
        editable: editable_signal,
        mode: mode_signal,
        drag,
        container_size,
        pinned_ids,
        on_change: on_change_cb,
    });

    let style = match mode {
        LayoutMode::Free => "position: relative; width: 100%; height: 100%; \
                             overflow: hidden; touch-action: none;"
            .to_string(),
        LayoutMode::Snap => {
            let rows_rule = match rows {
                Some(n) => format!("grid-template-rows: repeat({n}, 1fr); "),
                None => String::new(),
            };
            format!(
                "display: grid; \
                 grid-template-columns: repeat({cols}, 1fr); \
                 {rows_rule}\
                 grid-auto-rows: {row_height}px; \
                 gap: {gap}px; \
                 height: 100%; \
                 touch-action: none; \
                 --grid-cols: {cols}; \
                 --grid-row-height: {row_height}px; \
                 --grid-gap: {gap}px;",
            )
        }
    };
    let editable_class = if editable { " grid-editable" } else { "" };

    rsx! {
        div {
            class: "dioxus-grid-layout{editable_class} {class}",
            style,
            onmounted: move |evt| {
                let data = evt.data();
                let mut cs = container_size;
                spawn(async move {
                    if let Ok(rect) = data.get_client_rect().await {
                        cs.set(Some((rect.size.width, rect.size.height)));
                    }
                });
            },
            onresize: move |evt| {
                let mut cs = container_size;
                if let Ok(size) = evt.get_content_box_size() {
                    cs.set(Some((size.width, size.height)));
                }
            },
            {children}
            DragPlaceholder {}
        }
        DragOverlay {}
    }
}

#[component]
fn DragPlaceholder() -> Element {
    let ctx: GridContext = use_context();
    if ctx.is_free() {
        return rsx! { Fragment {} };
    }
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

                let (kind, item_id, dx, dy, projected) = {
                    let Some(state) = drag.read().clone() else { return };
                    let dx = cx - state.pointer_start_x;
                    let dy = cy - state.pointer_start_y;
                    let projected = state.project(cx, cy);
                    (state.kind, state.item_id, dx, dy, projected)
                };

                drag.with_mut(|d| {
                    if let Some(s) = d.as_mut() {
                        s.pointer_current_x = cx;
                        s.pointer_current_y = cy;
                    }
                });

                if ctx.is_free() {
                    let (Some(mut s), Some(state)) = (store, drag.read().clone()) else {
                        return;
                    };
                    if let Some(rect) = state.project_free(cx, cy) {
                        let others: Vec<_> = s
                            .free_snapshot()
                            .into_iter()
                            .filter(|(id, _)| id != &item_id)
                            .map(|(_, r)| r)
                            .collect();
                        let tol = ctx.snap_tolerance();
                        let rect = match state.kind {
                            InteractionKind::Drag => rect.snap_edges(&others, tol),
                            InteractionKind::Resize => rect.snap_size(&others, tol),
                        };
                        s.set_free(item_id, rect.clamp_inside());
                    }
                    return;
                }

                match kind {
                    InteractionKind::Drag => {
                        let js = format!(
                            "var el=document.querySelector('[data-id=\"{}\"]');\
                             if(el){{el.style.transform='translate({:.2}px,{:.2}px)';\
                             el.style.zIndex='1000';}}",
                            item_id, dx, dy,
                        );
                        let _ = document::eval(&js);

                        if let Some(store) = store {
                            let pinned = ctx.pinned_ids.read().clone();
                            settle_layout(store, &item_id, projected, &pinned);
                        }
                    }
                    InteractionKind::Resize => {
                        if let Some(mut s) = store {
                            s.set(item_id.clone(), projected);
                        }
                        if let Some(store) = store {
                            let pinned = ctx.pinned_ids.read().clone();
                            settle_layout(store, &item_id, projected, &pinned);
                        }
                    }
                }
            },
            onpointerup: move |_| { commit_and_clear(&mut drag, store, ctx.on_change, ctx.is_free()); },
            onpointercancel: move |_| { commit_and_clear(&mut drag, store, ctx.on_change, ctx.is_free()); },
        }
    }
}

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
    is_free: bool,
) {
    let Some(state) = drag.read().clone() else {
        return;
    };
    if is_free {
        drag.set(None);
        if let (Some(handler), Some(s)) = (on_change, store) {
            handler.call(s.snapshot());
        }
        return;
    }
    let js = format!(
        "var el=document.querySelector('[data-id=\"{}\"]');\
         if(el){{el.style.transform='';el.style.zIndex='';}}",
        state.item_id,
    );
    let _ = document::eval(&js);

    if matches!(state.kind, InteractionKind::Drag)
        && let Some(mut s) = store
    {
        let projected = state.project(state.pointer_current_x, state.pointer_current_y);
        s.set(state.item_id.clone(), projected);
    }
    drag.set(None);

    if let (Some(handler), Some(s)) = (on_change, store) {
        handler.call(s.snapshot());
    }
}

#[derive(Clone, Copy)]
pub(crate) struct GridContext {
    pub store: Option<LayoutStore>,
    pub cols: u32,
    pub row_height: f64,
    pub rows: Option<u32>,
    pub gap: f64,
    pub editable: Signal<bool>,
    pub mode: Signal<LayoutMode>,
    pub drag: Signal<Option<Interaction>>,
    pub container_size: Signal<Option<(f64, f64)>>,
    pub pinned_ids: Signal<HashSet<String>>,
    pub on_change: Option<EventHandler<Vec<(String, GridPosition)>>>,
}

impl GridContext {
    pub fn cell_w_px(&self) -> Option<f64> {
        let (w, _) = (*self.container_size.read())?;
        let cell = cell_w_from(w, self.cols, self.gap);
        (cell > 0.0).then_some(cell)
    }

    pub fn cell_h_px(&self) -> f64 {
        let h = (*self.container_size.read()).map(|(_, h)| h);
        match (self.rows, h) {
            (Some(_), Some(h)) => cell_h_from(h, self.rows, self.row_height, self.gap),
            _ => self.row_height,
        }
    }

    pub fn container_rect(&self) -> Option<(f64, f64)> {
        *self.container_size.read()
    }

    pub fn snap_tolerance(&self) -> f64 {
        const SNAP_PX: f64 = 12.0;
        match *self.container_size.read() {
            Some((w, _)) if w > 0.0 => SNAP_PX / w,
            _ => 0.01,
        }
    }

    pub fn is_free(&self) -> bool {
        *self.mode.read() == LayoutMode::Free
    }
}

fn cell_w_from(total_w: f64, cols: u32, gap: f64) -> f64 {
    let inner_gap = gap * (cols.saturating_sub(1) as f64);
    (total_w - inner_gap) / (cols.max(1) as f64)
}

fn cell_h_from(total_h: f64, rows: Option<u32>, row_height: f64, gap: f64) -> f64 {
    match rows {
        Some(n) if n > 0 => {
            let inner_gap = gap * (n.saturating_sub(1) as f64);
            ((total_h - inner_gap) / n as f64).max(1.0)
        }
        _ => row_height,
    }
}
