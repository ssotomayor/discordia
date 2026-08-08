//! Owned, mutable layout state.
//!
//! When a `GridLayout` is given a `LayoutStore` (via the `store` prop), all
//! `GridItem` children inside read their position from the store by `id`
//! instead of the static `x/y/w/h` props. Drag/resize interactions in later
//! commits mutate the store, which automatically re-renders affected items.

use std::collections::HashMap;

use dioxus::prelude::*;

use crate::layout::{FloatRect, GridPosition};

/// Reactive map of `item_id -> GridPosition`. `Copy + 'static`, cheap to
/// clone, safe to capture in event handlers. Equality is identity-based
/// (two stores are equal iff they point at the same underlying signal).
#[derive(Clone, Copy, PartialEq)]
pub struct LayoutStore {
    inner: Signal<HashMap<String, GridPosition>>,
    /// Free-mode rectangles, kept alongside the grid cells rather than
    /// replacing them: switching to Free and back should give you the
    /// arrangement you had, not a rounded-off approximation of it.
    free: Signal<HashMap<String, FloatRect>>,
    /// Stacking order for Free mode. Only meaningful when items can overlap.
    z: Signal<HashMap<String, u32>>,
    /// Highest z handed out so far; `raise` pre-increments it.
    z_top: Signal<u32>,
}

impl LayoutStore {
    pub fn new(positions: impl IntoIterator<Item = (String, GridPosition)>) -> Self {
        let map: HashMap<_, _> = positions.into_iter().collect();
        Self {
            inner: Signal::new(map),
            free: Signal::new(HashMap::new()),
            z: Signal::new(HashMap::new()),
            z_top: Signal::new(0),
        }
    }

    /// Free-mode rect for `id`, if one has been computed yet.
    pub fn get_free(&self, id: &str) -> Option<FloatRect> {
        self.free.read().get(id).copied()
    }

    pub fn set_free(&mut self, id: impl Into<String>, rect: FloatRect) {
        self.free.write().insert(id.into(), rect);
    }

    pub fn has_free(&self) -> bool {
        !self.free.read().is_empty()
    }

    /// Stacking order for `id` (0 when it has never been raised).
    pub fn z_of(&self, id: &str) -> u32 {
        self.z.read().get(id).copied().unwrap_or(0)
    }

    /// Bring `id` to the front. No-op if it is already there, so clicking the
    /// top window repeatedly doesn't churn the signal and re-render.
    pub fn raise(&mut self, id: &str) {
        if self.z_of(id) == *self.z_top.read() && self.z_of(id) != 0 {
            return;
        }
        let next = *self.z_top.read() + 1;
        self.z_top.set(next);
        self.z.write().insert(id.to_string(), next);
    }

    pub fn free_snapshot(&self) -> Vec<(String, FloatRect)> {
        let mut v: Vec<_> = self.free.read().iter().map(|(k, v)| (k.clone(), *v)).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Replace every position, for "reset layout" and for restoring a
    /// persisted arrangement.
    pub fn restore(
        &mut self,
        cells: impl IntoIterator<Item = (String, GridPosition)>,
        free: impl IntoIterator<Item = (String, FloatRect)>,
    ) {
        *self.inner.write() = cells.into_iter().collect();
        *self.free.write() = free.into_iter().collect();
        self.z.write().clear();
        self.z_top.set(0);
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
