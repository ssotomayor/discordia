//! Pointer-driven interaction state shared between `GridLayout` and
//! `GridItem`. Covers both drag (move the item) and resize (change w/h)
//! since they share the same pointer pipeline + cell-pixel geometry.

use crate::layout::GridPosition;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InteractionKind {
    Drag,
    Resize,
}

/// In-flight pointer interaction: which item, the layout snapshot at start,
/// the pointer's start + current coordinates, the cached cell pixel
/// geometry, plus item bounds (min_w/min_h) for resize clamping.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Interaction {
    pub kind: InteractionKind,
    pub item_id: String,
    pub start_pos: GridPosition,
    pub pointer_start_x: f64,
    pub pointer_start_y: f64,
    /// Current pointer position. Updated every pointermove. Used for
    /// snap-target projection and the smooth-drag transform.
    pub pointer_current_x: f64,
    pub pointer_current_y: f64,
    pub cell_w_px: f64,
    pub cell_h_px: f64,
    pub gap_px: f64,
    pub cols: u32,
    pub min_w: u32,
    pub min_h: u32,
}

impl Interaction {
    pub fn project(&self, pointer_x: f64, pointer_y: f64) -> GridPosition {
        let dx_px = pointer_x - self.pointer_start_x;
        let dy_px = pointer_y - self.pointer_start_y;
        let dx_cells = (dx_px / (self.cell_w_px + self.gap_px)).round() as i32;
        let dy_cells = (dy_px / (self.cell_h_px + self.gap_px)).round() as i32;

        match self.kind {
            InteractionKind::Drag => {
                let max_x = self.cols.saturating_sub(self.start_pos.w) as i32;
                let new_x =
                    ((self.start_pos.x as i32) + dx_cells).clamp(0, max_x.max(0)) as u32;
                let new_y = ((self.start_pos.y as i32) + dy_cells).max(0) as u32;
                GridPosition {
                    x: new_x,
                    y: new_y,
                    w: self.start_pos.w,
                    h: self.start_pos.h,
                }
            }
            InteractionKind::Resize => {
                let max_w = self.cols.saturating_sub(self.start_pos.x) as i32;
                let min_w = self.min_w as i32;
                let new_w = ((self.start_pos.w as i32) + dx_cells)
                    .clamp(min_w, max_w.max(min_w)) as u32;
                let new_h = ((self.start_pos.h as i32) + dy_cells).max(self.min_h as i32) as u32;
                GridPosition {
                    x: self.start_pos.x,
                    y: self.start_pos.y,
                    w: new_w,
                    h: new_h,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(kind: InteractionKind, start: GridPosition) -> Interaction {
        Interaction {
            kind,
            item_id: "i".into(),
            start_pos: start,
            pointer_start_x: 0.0,
            pointer_start_y: 0.0,
            pointer_current_x: 0.0,
            pointer_current_y: 0.0,
            cell_w_px: 50.0,
            cell_h_px: 50.0,
            gap_px: 10.0,
            cols: 12,
            min_w: 1,
            min_h: 1,
        }
    }

    #[test]
    fn drag_right_one_cell() {
        let i = base(InteractionKind::Drag, GridPosition::new(0, 0, 2, 2));
        let p = i.project(60.0, 0.0);
        assert_eq!((p.x, p.y, p.w, p.h), (1, 0, 2, 2));
    }

    #[test]
    fn drag_clamps_right() {
        let i = base(InteractionKind::Drag, GridPosition::new(5, 0, 4, 2));
        let p = i.project(10000.0, 0.0);
        assert_eq!(p.x, 8); // max_x = 12 - 4
    }

    #[test]
    fn resize_grows_one_cell() {
        let i = base(InteractionKind::Resize, GridPosition::new(0, 0, 2, 2));
        let p = i.project(60.0, 60.0);
        assert_eq!((p.x, p.y, p.w, p.h), (0, 0, 3, 3));
    }

    #[test]
    fn resize_clamps_to_min() {
        let mut i = base(InteractionKind::Resize, GridPosition::new(0, 0, 4, 4));
        i.min_w = 2;
        i.min_h = 2;
        let p = i.project(-10000.0, -10000.0);
        assert_eq!((p.w, p.h), (2, 2));
    }

    #[test]
    fn resize_clamps_to_columns() {
        let i = base(InteractionKind::Resize, GridPosition::new(8, 0, 2, 2));
        let p = i.project(10000.0, 0.0);
        assert_eq!(p.w, 4); // max_w = 12 - 8
    }
}
