//! `GridLayout` — the container component.

use dioxus::prelude::*;

use crate::drag::DragState;
use crate::layout::GridPosition;
use crate::store::LayoutStore;

/// Container that positions its `GridItem` children on a CSS grid.
#[component]
pub fn GridLayout(
    #[props(default = 12)] cols: u32,
    #[props(default = 30.0)] row_height: f64,
    #[props(default = 10.0)] gap: f64,
    #[props(default = String::new())] class: String,
    #[props(default = false)] editable: bool,
    store: Option<LayoutStore>,
    children: Element,
) -> Element {
    let drag = use_signal::<Option<DragState>>(|| None);
    let container_width = use_signal::<Option<f64>>(|| None);

    use_context_provider(|| GridContext {
        store,
        cols,
        row_height,
        gap,
        editable,
        drag,
        container_width,
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
                // Measure container width once for cell-px math. Re-running
                // on resize is a follow-up commit.
                let data = evt.data();
                let mut cw = container_width;
                spawn(async move {
                    if let Ok(rect) = data.get_client_rect().await {
                        cw.set(Some(rect.size.width));
                    }
                });
            },
            {children}
        }
        DragOverlay {}
    }
}

/// Renders an invisible fixed-position overlay over the entire viewport
/// while a drag is active, so global `pointermove`/`pointerup` flow even when
/// the cursor moves outside the originating item.
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
                let Some(state) = drag.read().clone() else { return };
                let projected = state.project(cx, cy);
                if let Some(mut s) = store {
                    s.set(state.item_id.clone(), projected);
                }
            },
            onpointerup: move |_| { drag.set(None); },
            onpointercancel: move |_| { drag.set(None); },
        }
    }
}

/// Internal: GridLayout settings + shared drag state, exposed to GridItem
/// via Dioxus context.
#[derive(Clone, Copy)]
pub(crate) struct GridContext {
    pub store: Option<LayoutStore>,
    pub cols: u32,
    pub row_height: f64,
    pub gap: f64,
    pub editable: bool,
    pub drag: Signal<Option<DragState>>,
    pub container_width: Signal<Option<f64>>,
}

impl GridContext {
    /// Width of one column in CSS pixels, given the measured container width.
    /// Returns None until the first `onmounted` measurement has landed.
    pub fn cell_w_px(&self) -> Option<f64> {
        let total = self.container_width.read().as_ref().copied()?;
        let inner_gap = self.gap * (self.cols.saturating_sub(1) as f64);
        let cell_w = (total - inner_gap) / (self.cols as f64);
        if cell_w > 0.0 { Some(cell_w) } else { None }
    }
}

#[allow(dead_code)] // grid context re-exported for test scaffolding in later commits
pub(crate) fn _initial_position(x: u32, y: u32, w: u32, h: u32) -> GridPosition {
    GridPosition::new(x, y, w, h)
}
