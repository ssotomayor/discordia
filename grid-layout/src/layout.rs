//! Core layout data structures shared by `GridLayout` and `GridItem`.

use serde::{Deserialize, Serialize};

/// Position + size of one item in grid units (columns × rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GridPosition {
    /// Column index from the left, 0-based.
    pub x: u32,
    /// Row index from the top, 0-based.
    pub y: u32,
    /// Width in columns.
    pub w: u32,
    /// Height in rows.
    pub h: u32,
}

impl GridPosition {
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
}

/// Layout configuration of the container. Items are addressed by `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutSpec {
    pub cols: u32,
    pub row_height: u32,
    pub gap: u32,
    pub items: Vec<(String, GridPosition)>,
}

impl Default for LayoutSpec {
    fn default() -> Self {
        Self {
            cols: 12,
            row_height: 30,
            gap: 10,
            items: Vec::new(),
        }
    }
}

/// Position + size of one item as **fractions of the container** (0..=1), used
/// by [`LayoutMode::Free`].
///
/// Fractions rather than pixels, for two reasons that both came from bug
/// reports. A fraction in [0, 1-w] is on-screen *by construction*, so a window
/// cannot be dragged somewhere it can't be grabbed back from no matter what
/// the container does afterwards. And a fractional layout rescales with the
/// window for free, instead of holding pixel positions that were correct for
/// some earlier size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FloatRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl FloatRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    /// Force the rect fully inside the container. In fraction space this is
    /// total: there is no "mostly off-screen" state to recover from.
    pub fn clamp_inside(mut self) -> Self {
        self.w = self.w.clamp(MIN_FRAC, 1.0);
        self.h = self.h.clamp(MIN_FRAC, 1.0);
        self.x = self.x.clamp(0.0, (1.0 - self.w).max(0.0));
        self.y = self.y.clamp(0.0, (1.0 - self.h).max(0.0));
        self
    }

    fn edges_x(&self) -> [f64; 2] {
        [self.x, self.x + self.w]
    }

    fn edges_y(&self) -> [f64; 2] {
        [self.y, self.y + self.h]
    }

    /// Magnetic edge alignment: nudge this rect so an edge that is *nearly*
    /// flush with a neighbour's edge (or the container's) becomes exactly
    /// flush. `tol` is in fraction units.
    ///
    /// This is what replaces grid snapping — you can put a window anywhere, but
    /// lining two of them up doesn't require pixel-perfect aim.
    pub fn snap_edges(mut self, others: &[FloatRect], tol: f64) -> Self {
        let mut xs = vec![0.0, 1.0];
        let mut ys = vec![0.0, 1.0];
        for o in others {
            xs.extend(o.edges_x());
            ys.extend(o.edges_y());
        }
        if let Some(dx) = best_delta(&[self.x, self.x + self.w], &xs, tol) {
            self.x += dx;
        }
        if let Some(dy) = best_delta(&[self.y, self.y + self.h], &ys, tol) {
            self.y += dy;
        }
        self
    }

    /// Like `snap_edges` but for a resize: only the far edges move, so the
    /// item's origin stays put and its size changes instead.
    pub fn snap_size(mut self, others: &[FloatRect], tol: f64) -> Self {
        let mut xs = vec![1.0];
        let mut ys = vec![1.0];
        for o in others {
            xs.extend(o.edges_x());
            ys.extend(o.edges_y());
        }
        if let Some(dx) = best_delta(&[self.x + self.w], &xs, tol) {
            self.w += dx;
        }
        if let Some(dy) = best_delta(&[self.y + self.h], &ys, tol) {
            self.h += dy;
        }
        self
    }
}

/// Where an item lives. The two variants are kept side by side rather than
/// converted destructively, so switching modes and back returns the layout you
/// had instead of a lossy round-trip through the other coordinate system.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Placement {
    Cell(GridPosition),
    Free(FloatRect),
}

/// Smallest fraction of the container an item may shrink to. Keeps a window
/// grabbable and stops a resize collapsing it to nothing.
const MIN_FRAC: f64 = 0.05;

/// The smallest movement that brings any of `moving` onto any of `targets`,
/// or `None` when nothing is within `tol`.
fn best_delta(moving: &[f64], targets: &[f64], tol: f64) -> Option<f64> {
    let mut best: Option<f64> = None;
    for m in moving {
        for t in targets {
            let d = t - m;
            if d.abs() <= tol && best.is_none_or(|b: f64| d.abs() < b.abs()) {
                best = Some(d);
            }
        }
    }
    best
}

/// How the container places its items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    /// CSS grid: items occupy whole cells and never overlap — neighbours are
    /// pushed aside and compacted upward.
    #[default]
    Snap,
    /// Absolute pixel placement: no snapping, overlap allowed, click-to-front
    /// z-ordering.
    Free,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// However far a drag goes, the result is fully inside the container. This
    /// is the property that makes "I lost a widget off-screen" impossible
    /// rather than merely unlikely.
    #[test]
    fn clamp_always_lands_fully_inside() {
        for r in [
            FloatRect::new(-50.0, -50.0, 0.3, 0.2),
            FloatRect::new(9999.0, 9999.0, 0.3, 0.2),
            FloatRect::new(0.9, 0.95, 0.5, 0.5),
        ] {
            let c = r.clamp_inside();
            assert!(c.x >= 0.0 && c.y >= 0.0, "{c:?}");
            assert!(c.x + c.w <= 1.0 + f64::EPSILON, "{c:?}");
            assert!(c.y + c.h <= 1.0 + f64::EPSILON, "{c:?}");
        }
    }

    /// Clamping must not disturb a window already comfortably inside.
    #[test]
    fn clamp_leaves_an_inside_window_alone() {
        let r = FloatRect::new(0.1, 0.12, 0.3, 0.2);
        assert_eq!(r.clamp_inside(), r);
    }

    /// A resize can't collapse a window to nothing.
    #[test]
    fn clamp_enforces_a_minimum_size() {
        let r = FloatRect::new(0.5, 0.5, 0.0, -1.0).clamp_inside();
        assert!(r.w >= MIN_FRAC && r.h >= MIN_FRAC, "{r:?}");
    }

    /// Edge snapping: a window dropped *nearly* flush with a neighbour becomes
    /// exactly flush — that is what replaces grid snapping.
    #[test]
    fn snap_aligns_a_near_miss_to_a_neighbour() {
        let neighbour = FloatRect::new(0.5, 0.0, 0.5, 1.0);
        // Left edge 0.008 short of the neighbour's left edge.
        let dragged = FloatRect::new(0.492, 0.3, 0.2, 0.2);
        let snapped = dragged.snap_edges(&[neighbour], 0.01);
        assert!((snapped.x - 0.5).abs() < 1e-9, "{snapped:?}");
        // Snapping moves, never resizes.
        assert_eq!((snapped.w, snapped.h), (dragged.w, dragged.h));
    }

    /// ...but a window dropped clearly away from everything stays exactly
    /// where it was put. A magnet that always grabs is just a grid again.
    #[test]
    fn snap_leaves_a_deliberate_placement_alone() {
        let neighbour = FloatRect::new(0.5, 0.0, 0.5, 1.0);
        let dragged = FloatRect::new(0.2, 0.3, 0.2, 0.2);
        assert_eq!(dragged.snap_edges(&[neighbour], 0.01), dragged);
    }

    /// Container edges are snap targets too, so panels can sit flush with the
    /// window without fighting for the last pixel.
    #[test]
    fn snap_aligns_to_the_container_edges() {
        let r = FloatRect::new(0.004, 0.0, 0.3, 0.2).snap_edges(&[], 0.01);
        assert!(r.x.abs() < 1e-9, "{r:?}");
    }

    /// Resize snapping moves the far edge, leaving the origin where it is.
    #[test]
    fn snap_size_keeps_the_origin() {
        let neighbour = FloatRect::new(0.6, 0.0, 0.4, 1.0);
        let resized = FloatRect::new(0.1, 0.1, 0.495, 0.3);
        let snapped = resized.snap_size(&[neighbour], 0.01);
        assert_eq!((snapped.x, snapped.y), (resized.x, resized.y));
        assert!((snapped.x + snapped.w - 0.6).abs() < 1e-9, "{snapped:?}");
    }
}
