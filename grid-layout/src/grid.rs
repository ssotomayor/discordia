//! `GridLayout` — the container component.

use dioxus::prelude::*;

use crate::store::LayoutStore;

/// Container that positions its `GridItem` children on a CSS grid.
///
/// # Props
///
/// - `cols`: number of columns (default 12)
/// - `row_height`: pixel height of each row (default 30.0)
/// - `gap`: pixel gap between cells (default 10.0)
/// - `class`: extra classes appended to the container's `class` attribute
/// - `editable`: stub for future drag/resize edit mode (no effect yet)
/// - `store`: optional [`LayoutStore`] — when provided, children read their
///   positions from it by `id` instead of from props; later commits mutate
///   the store on drag/resize.
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
    // Make the store + grid settings available to children via context.
    use_context_provider(|| GridContext {
        store,
        cols,
        row_height,
        gap,
    });

    let style = format!(
        "display: grid; \
         grid-template-columns: repeat({cols}, 1fr); \
         grid-auto-rows: {row_height}px; \
         gap: {gap}px; \
         --grid-cols: {cols}; \
         --grid-row-height: {row_height}px; \
         --grid-gap: {gap}px;",
    );
    let editable_class = if editable { " grid-editable" } else { "" };

    rsx! {
        div {
            class: "dioxus-grid-layout{editable_class} {class}",
            style,
            {children}
        }
    }
}

/// Internal: GridLayout settings + (optional) store, exposed to GridItem via
/// Dioxus context.
#[derive(Clone, Copy)]
#[allow(dead_code)] // row_height/gap read by drag/resize logic in next commits
pub(crate) struct GridContext {
    pub store: Option<LayoutStore>,
    pub cols: u32,
    pub row_height: f64,
    pub gap: f64,
}
