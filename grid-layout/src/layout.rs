use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GridPosition {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl GridPosition {
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
/// Fractions rather than pixels, so a resized window rescales panels instead
/// of stranding them off-screen.
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

    /// Must not disturb a rect already comfortably inside.
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

const MIN_FRAC: f64 = 0.05;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    #[default]
    Snap,
    Free,
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn clamp_leaves_an_inside_window_alone() {
        let r = FloatRect::new(0.1, 0.12, 0.3, 0.2);
        assert_eq!(r.clamp_inside(), r);
    }

    #[test]
    fn clamp_enforces_a_minimum_size() {
        let r = FloatRect::new(0.5, 0.5, 0.0, -1.0).clamp_inside();
        assert!(r.w >= MIN_FRAC && r.h >= MIN_FRAC, "{r:?}");
    }

    #[test]
    fn snap_aligns_a_near_miss_to_a_neighbour() {
        let neighbour = FloatRect::new(0.5, 0.0, 0.5, 1.0);
        let dragged = FloatRect::new(0.492, 0.3, 0.2, 0.2);
        let snapped = dragged.snap_edges(&[neighbour], 0.01);
        assert!((snapped.x - 0.5).abs() < 1e-9, "{snapped:?}");
        assert_eq!((snapped.w, snapped.h), (dragged.w, dragged.h));
    }

    #[test]
    fn snap_leaves_a_deliberate_placement_alone() {
        let neighbour = FloatRect::new(0.5, 0.0, 0.5, 1.0);
        let dragged = FloatRect::new(0.2, 0.3, 0.2, 0.2);
        assert_eq!(dragged.snap_edges(&[neighbour], 0.01), dragged);
    }

    #[test]
    fn snap_aligns_to_the_container_edges() {
        let r = FloatRect::new(0.004, 0.0, 0.3, 0.2).snap_edges(&[], 0.01);
        assert!(r.x.abs() < 1e-9, "{r:?}");
    }

    #[test]
    fn snap_size_keeps_the_origin() {
        let neighbour = FloatRect::new(0.6, 0.0, 0.4, 1.0);
        let resized = FloatRect::new(0.1, 0.1, 0.495, 0.3);
        let snapped = resized.snap_size(&[neighbour], 0.01);
        assert_eq!((snapped.x, snapped.y), (resized.x, resized.y));
        assert!((snapped.x + snapped.w - 0.6).abs() < 1e-9, "{snapped:?}");
    }
}
