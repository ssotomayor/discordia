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
