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
