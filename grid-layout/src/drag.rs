//! Pointer-driven drag state shared between `GridLayout` and `GridItem`.

use crate::layout::GridPosition;

/// In-flight drag: which item is being moved, the layout snapshot at start,
/// the pointer's start position, and the cached cell pixel geometry needed
/// to translate pointer deltas into grid units.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DragState {
    pub item_id: String,
    pub start_pos: GridPosition,
    pub pointer_start_x: f64,
    pub pointer_start_y: f64,
    pub cell_w_px: f64,
    pub cell_h_px: f64,
    pub gap_px: f64,
    pub cols: u32,
}

impl DragState {
    /// Translate the current pointer position into a new `GridPosition`,
    /// snapping to the nearest grid cell and clamping to the column count.
    pub fn project(&self, pointer_x: f64, pointer_y: f64) -> GridPosition {
        let dx_px = pointer_x - self.pointer_start_x;
        let dy_px = pointer_y - self.pointer_start_y;
        let dx_cells = (dx_px / (self.cell_w_px + self.gap_px)).round() as i32;
        let dy_cells = (dy_px / (self.cell_h_px + self.gap_px)).round() as i32;

        let max_x = self.cols.saturating_sub(self.start_pos.w) as i32;
        let new_x = ((self.start_pos.x as i32) + dx_cells).clamp(0, max_x.max(0)) as u32;
        let new_y = ((self.start_pos.y as i32) + dy_cells).max(0) as u32;
        GridPosition {
            x: new_x,
            y: new_y,
            w: self.start_pos.w,
            h: self.start_pos.h,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag_at(start_x: u32, start_y: u32, w: u32) -> DragState {
        DragState {
            item_id: "i".into(),
            start_pos: GridPosition::new(start_x, start_y, w, 2),
            pointer_start_x: 0.0,
            pointer_start_y: 0.0,
            cell_w_px: 50.0,
            cell_h_px: 50.0,
            gap_px: 10.0,
            cols: 12,
        }
    }

    #[test]
    fn move_right_one_cell() {
        let d = drag_at(0, 0, 2);
        assert_eq!(d.project(60.0, 0.0).x, 1);
        assert_eq!(d.project(60.0, 0.0).y, 0);
    }

    #[test]
    fn clamps_to_left_edge() {
        let d = drag_at(0, 0, 2);
        assert_eq!(d.project(-500.0, 0.0).x, 0);
    }

    #[test]
    fn clamps_to_right_edge() {
        let d = drag_at(5, 0, 4); // max_x = 12 - 4 = 8
        let projected = d.project(10000.0, 0.0);
        assert_eq!(projected.x, 8);
    }

    #[test]
    fn move_down_one_cell() {
        let d = drag_at(0, 0, 2);
        assert_eq!(d.project(0.0, 60.0).y, 1);
    }
}
