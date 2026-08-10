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

    /// Stacking order for `id` (0 when it has never been raised).
    pub fn z_of(&self, id: &str) -> u32 {
        self.z.read().get(id).copied().unwrap_or(0)
    }

    /// Bring `id` to the front. No-op if it is already there, so clicking the
    /// top window repeatedly doesn't churn the signal and re-render.
    ///
    /// Order is re-ranked into 1..=n on every raise rather than handed an
    /// ever-increasing counter. A monotonic counter would eventually push a
    /// panel's z-index past the app's own overlay layers — modals and toasts
    /// live in the 30-50 band — and a panel would start painting over dialogs
    /// after enough clicks. Ranks stay small and bounded by the item count.
    pub fn raise(&mut self, id: &str) {
        let current = self.z.read().clone();
        if is_top(&current, id) {
            return;
        }
        let next = rerank(current, id);
        let top = next.len() as u32;
        *self.z.write() = next;
        self.z_top.set(top);
    }

    pub fn free_snapshot(&self) -> Vec<(String, FloatRect)> {
        let mut v: Vec<_> = self
            .free
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
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

/// Whether `id` already sits on top, so a repeat click doesn't churn state.
fn is_top(z: &HashMap<String, u32>, id: &str) -> bool {
    match z.get(id) {
        Some(v) => *v == z.values().copied().max().unwrap_or(0),
        None => false,
    }
}

/// Re-rank stacking order into 1..=n with `id` on top.
///
/// Split out from `raise` so it can be tested: `LayoutStore` is built on Dioxus
/// signals and cannot be touched outside a runtime, but this — the part with
/// the actual reasoning in it — is plain data.
fn rerank(current: HashMap<String, u32>, id: &str) -> HashMap<String, u32> {
    let mut order: Vec<(String, u32)> = current.into_iter().collect();
    // Existing order first (ties broken by id for determinism), raised last.
    order.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
    order.retain(|(k, _)| k != id);
    order.push((id.to_string(), u32::MAX));
    order
        .into_iter()
        .enumerate()
        .map(|(rank, (k, _))| (k, rank as u32 + 1))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn z(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn raise_puts_the_item_on_top() {
        let m = rerank(z(&[("a", 1), ("b", 2), ("c", 3)]), "a");
        assert!(m["a"] > m["b"] && m["a"] > m["c"]);
        // Relative order of the others is preserved.
        assert!(m["b"] < m["c"]);
    }

    /// Ranks stay small no matter how much clicking happens. A monotonic
    /// counter would eventually carry a panel past the app's modal/toast
    /// layers (z 30-50) and it would start painting over dialogs.
    #[test]
    fn ranks_stay_bounded_by_the_item_count() {
        let mut m = z(&[("a", 1), ("b", 2), ("c", 3)]);
        for i in 0..500 {
            m = rerank(m, ["a", "b", "c"][i % 3]);
        }
        assert_eq!(m.len(), 3);
        for (id, v) in &m {
            assert!(*v >= 1 && *v <= 3, "{id} reached z {v}");
        }
    }

    /// Raising something not yet ranked adds it on top without disturbing the
    /// rest — this is the first click on a never-touched panel.
    #[test]
    fn raising_an_unranked_item_adds_it_on_top() {
        let m = rerank(z(&[("a", 1), ("b", 2)]), "new");
        assert!(m["new"] > m["a"] && m["new"] > m["b"]);
        assert!(m["a"] < m["b"]);
    }

    #[test]
    fn is_top_detects_the_frontmost_item() {
        let m = z(&[("a", 1), ("b", 2)]);
        assert!(is_top(&m, "b"));
        assert!(!is_top(&m, "a"));
        // Never-raised items are not on top, so the first click always ranks.
        assert!(!is_top(&m, "missing"));
    }
}
