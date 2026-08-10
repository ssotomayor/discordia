//! `dioxus-grid-layout` — draggable, resizable grid layout for Dioxus.
//!
//! Inspired by [`react-grid-layout`](https://github.com/react-grid-layout/react-grid-layout).
//!
//! # Status
//!
//! **v0.0.1 — static rendering only.** Items are positioned via CSS grid
//! from props; no interactive drag/resize yet. Subsequent commits on the
//! `feature/dashboard-grid` branch add:
//!
//! - [ ] Pointer-driven drag of items by a drag handle
//! - [ ] Resize from the bottom-right corner
//! - [ ] Collision resolution (push displaced items down)
//! - [ ] `LayoutStore` hook for owning + persisting positions
//! - [ ] `editable` mode toggle
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
