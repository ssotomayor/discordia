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

/// Position + size of one item in pixels, relative to the container's
/// top-left. Used by [`LayoutMode::Free`], where an item is placed absolutely
/// rather than on a grid cell.
///
/// `f64` rather than the grid's `u32` for a reason: the whole point of free
/// mode is that a window can sit anywhere, so there is no cell index to round
/// to and nothing to snap against.
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

    /// Keep at least `margin` px of the item inside a `container` so a window
    /// can never be dragged somewhere it can't be grabbed back from.
    pub fn clamp_visible(mut self, container_w: f64, container_h: f64, margin: f64) -> Self {
        let max_x = (container_w - margin).max(0.0);
        let max_y = (container_h - margin).max(0.0);
        // The lower bounds let an item hang off the left/top by all but
        // `margin`, which is what makes edge-to-edge placement possible.
        self.x = self.x.clamp(margin - self.w, max_x);
        self.y = self.y.clamp(0.0, max_y);
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

    /// A window may hang off the left, but never so far that there is nothing
    /// left to grab — that is how you lose a panel in free mode.
    #[test]
    fn clamp_keeps_a_grabbable_strip_on_screen() {
        let margin = 64.0;
        let r = FloatRect::new(-5000.0, -5000.0, 300.0, 200.0).clamp_visible(1000.0, 800.0, margin);
        assert!(r.x + r.w >= margin, "left edge: {r:?}");
        assert!(r.y >= 0.0, "top edge: {r:?}");

        let r = FloatRect::new(9999.0, 9999.0, 300.0, 200.0).clamp_visible(1000.0, 800.0, margin);
        assert!(r.x <= 1000.0 - margin, "right edge: {r:?}");
        assert!(r.y <= 800.0 - margin, "bottom edge: {r:?}");
    }

    /// Clamping must not move or resize a window already comfortably inside.
    #[test]
    fn clamp_leaves_an_onscreen_window_alone() {
        let r = FloatRect::new(100.0, 120.0, 300.0, 200.0);
        assert_eq!(r.clamp_visible(1000.0, 800.0, 64.0), r);
    }
}
