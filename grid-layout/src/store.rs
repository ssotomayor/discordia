//! Owned, mutable layout state.
//!
//! When a `GridLayout` is given a `LayoutStore` (via the `store` prop), all
//! `GridItem` children inside read their position from the store by `id`
//! instead of the static `x/y/w/h` props. Drag/resize interactions in later
//! commits mutate the store, which automatically re-renders affected items.

use std::collections::HashMap;

use dioxus::prelude::*;

use crate::layout::GridPosition;

/// Reactive map of `item_id -> GridPosition`. `Copy + 'static`, cheap to
/// clone, safe to capture in event handlers. Equality is identity-based
/// (two stores are equal iff they point at the same underlying signal).
#[derive(Clone, Copy, PartialEq)]
pub struct LayoutStore {
    inner: Signal<HashMap<String, GridPosition>>,
}

impl LayoutStore {
    pub fn new(positions: impl IntoIterator<Item = (String, GridPosition)>) -> Self {
        let map: HashMap<_, _> = positions.into_iter().collect();
        Self {
            inner: Signal::new(map),
        }
    }

    /// Position for `id`, reactive — reading this in a component scope
    /// subscribes that component to changes for this item.
    pub fn get(&self, id: &str) -> Option<GridPosition> {
        self.inner.read().get(id).copied()
    }

    pub fn set(&mut self, id: impl Into<String>, pos: GridPosition) {
        self.inner.write().insert(id.into(), pos);
    }

    pub fn snapshot(&self) -> Vec<(String, GridPosition)> {
        let mut v: Vec<_> = self
            .inner
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        // Stable ordering by id so callers persisting the layout get
        // deterministic output.
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

/// Convenience hook: create a `LayoutStore` once and reuse on subsequent
/// renders.
pub fn use_layout_store<F>(initial: F) -> LayoutStore
where
    F: FnOnce() -> Vec<(String, GridPosition)>,
{
    use_hook(|| LayoutStore::new(initial()))
}
