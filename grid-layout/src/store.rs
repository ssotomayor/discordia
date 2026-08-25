use std::collections::HashMap;

use dioxus::prelude::*;

use crate::layout::{FloatRect, GridPosition};

#[derive(Clone, Copy, PartialEq)]
pub struct LayoutStore {
    inner: Signal<HashMap<String, GridPosition>>,
    free: Signal<HashMap<String, FloatRect>>,
    z: Signal<HashMap<String, u32>>,
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

    pub fn get_free(&self, id: &str) -> Option<FloatRect> {
        self.free.read().get(id).copied()
    }

    pub fn set_free(&mut self, id: impl Into<String>, rect: FloatRect) {
        self.free.write().insert(id.into(), rect);
    }

    pub fn z_of(&self, id: &str) -> u32 {
        self.z.read().get(id).copied().unwrap_or(0)
    }

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
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

pub fn use_layout_store<F>(initial: F) -> LayoutStore
where
    F: FnOnce() -> Vec<(String, GridPosition)>,
{
    use_hook(|| LayoutStore::new(initial()))
}

fn is_top(z: &HashMap<String, u32>, id: &str) -> bool {
    match z.get(id) {
        Some(v) => *v == z.values().copied().max().unwrap_or(0),
        None => false,
    }
}

fn rerank(current: HashMap<String, u32>, id: &str) -> HashMap<String, u32> {
    let mut order: Vec<(String, u32)> = current.into_iter().collect();
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
        assert!(m["b"] < m["c"]);
    }

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
        assert!(!is_top(&m, "missing"));
    }
}
