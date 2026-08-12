//! `dioxus-grid-layout` — draggable, resizable grid layout for Dioxus.
//!
//! Inspired by [`react-grid-layout`](https://github.com/react-grid-layout/react-grid-layout).
//!
//! # Status
//!
//! Drag, resize, collision resolution, [`LayoutStore`] and `editable` all
//! shipped — this text used to announce them as pending work on a branch that
//! no longer exists, which is the same claim `35b3e00` corrected in the README
//! and missed here. [`LayoutMode::Free`] came later and is what the app in this
//! workspace actually uses; snap mode and its vertical compaction remain for
//! consumers who want a grid.
//!
//! # Quick start
//!
//! ```ignore
//! use dioxus::prelude::*;
//! use dioxus_grid_layout::{GridLayout, GridItem};
//!
//! #[component]
//! fn App() -> Element {
//!     rsx! {
//!         GridLayout { cols: 12, row_height: 30.0, gap: 10.0,
//!             GridItem { id: "a", x: 0, y: 0, w: 4, h: 3,
//!                 div { class: "p-2 bg-emerald-500", "Widget A" }
//!             }
//!             GridItem { id: "b", x: 4, y: 0, w: 8, h: 3,
//!                 div { class: "p-2 bg-indigo-500", "Widget B" }
//!             }
//!         }
//!     }
//! }
//! ```

/// Drag diagnostics, debug-only. Named `gtrace` because `trace` collides with
/// the one `dioxus::prelude` brings in.
///
/// These explain a pointerdown that started no drag — no context, not
/// editable, container unmeasured — which are silent failures otherwise. They
/// are also a library printing to its consumer's stderr, so they compile out of
/// release rather than narrating pointer events in a shipped app. `dlog!` in
/// the client crate is the same idea; it just isn't reachable from here.
macro_rules! gtrace {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        eprintln!($($arg)*);
    };
}

mod collision;
mod drag;
mod grid;
mod item;
mod layout;
mod nodrag;
mod store;

pub use grid::GridLayout;
pub use item::GridItem;
pub use layout::{FloatRect, GridPosition, LayoutMode};
pub use nodrag::NoDrag;
pub use store::{LayoutStore, use_layout_store};
