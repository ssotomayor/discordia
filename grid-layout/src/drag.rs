use crate::layout::{FloatRect, GridPosition};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InteractionKind {
    Drag,
    Resize,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Interaction {
    pub kind: InteractionKind,
    pub item_id: String,
    pub start_pos: GridPosition,
    pub start_free: Option<FloatRect>,
    pub pointer_start_x: f64,
    pub pointer_start_y: f64,
    pub pointer_current_x: f64,
    pub pointer_current_y: f64,
    pub cell_w_px: f64,
    pub cell_h_px: f64,
    pub container_w: f64,
    pub container_h: f64,
    pub gap_px: f64,
    pub cols: u32,
    pub min_w: u32,
    pub min_h: u32,
}

impl Interaction {
    pub fn project_free(&self, pointer_x: f64, pointer_y: f64) -> Option<FloatRect> {
        let start = self.start_free?;
        if self.container_w <= 0.0 || self.container_h <= 0.0 {
            return None;
        }
        let dx = (pointer_x - self.pointer_start_x) / self.container_w;
        let dy = (pointer_y - self.pointer_start_y) / self.container_h;
        Some(match self.kind {
            InteractionKind::Drag => FloatRect {
                x: start.x + dx,
                y: start.y + dy,
                ..start
            },
            InteractionKind::Resize => FloatRect {
                w: start.w + dx,
                h: start.h + dy,
                ..start
            },
        })
    }

    pub fn project(&self, pointer_x: f64, pointer_y: f64) -> GridPosition {
        let dx_px = pointer_x - self.pointer_start_x;
        let dy_px = pointer_y - self.pointer_start_y;
        let dx_cells = (dx_px / (self.cell_w_px + self.gap_px)).round() as i32;
        let dy_cells = (dy_px / (self.cell_h_px + self.gap_px)).round() as i32;

        match self.kind {
            InteractionKind::Drag => {
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
            InteractionKind::Resize => {
                let max_w = self.cols.saturating_sub(self.start_pos.x) as i32;
                let min_w = self.min_w as i32;
                let new_w =
                    ((self.start_pos.w as i32) + dx_cells).clamp(min_w, max_w.max(min_w)) as u32;
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
            start_free: None,
            pointer_start_x: 0.0,
            pointer_start_y: 0.0,
            pointer_current_x: 0.0,
            pointer_current_y: 0.0,
            cell_w_px: 50.0,
            cell_h_px: 50.0,
            container_w: 1000.0,
            container_h: 500.0,
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
        assert_eq!(p.x, 8);
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
        assert_eq!(p.w, 4);
    }

    fn free(kind: InteractionKind, rect: FloatRect) -> Interaction {
        let mut i = base(kind, GridPosition::new(2, 3, 4, 5));
        i.start_free = Some(rect);
        i.pointer_start_x = 100.0;
        i.pointer_start_y = 100.0;
        i.min_w = 2;
        i.min_h = 2;
        i
    }

    #[test]
    fn free_drag_applies_the_delta_verbatim() {
        let i = free(
            InteractionKind::Drag,
            FloatRect::new(0.10, 0.20, 0.30, 0.20),
        );
        let r = i.project_free(170.0, 150.0).unwrap();
        assert!(
            (r.x - 0.17).abs() < 1e-9 && (r.y - 0.30).abs() < 1e-9,
            "{r:?}"
        );
        assert_eq!((r.w, r.h), (0.30, 0.20), "dragging must not resize");

        let snapped = i.project(107.0, 103.0);
        assert_eq!(
            (snapped.x, snapped.y),
            (2, 3),
            "a sub-cell nudge should not move a snapped item"
        );
    }

    #[test]
    fn free_drag_projection_is_unbounded_but_clamps_inside() {
        let i = free(InteractionKind::Drag, FloatRect::new(0.0, 0.0, 0.2, 0.2));
        let r = i.project_free(100_000.0, 100_000.0).unwrap();
        assert!(r.x > 1.0, "projection should not clamp: {r:?}");
        let c = r.clamp_inside();
        assert!(c.x + c.w <= 1.0 + f64::EPSILON, "{c:?}");
    }

    #[test]
    fn free_resize_keeps_the_origin() {
        let i = free(
            InteractionKind::Resize,
            FloatRect::new(0.05, 0.06, 0.30, 0.20),
        );
        let r = i.project_free(-100_000.0, -100_000.0).unwrap();
        assert_eq!((r.x, r.y), (0.05, 0.06));
        assert!(r.clamp_inside().w >= 0.05, "clamp enforces the minimum");
    }

    #[test]
    fn free_projection_needs_a_measured_container() {
        let mut i = free(InteractionKind::Drag, FloatRect::new(0.0, 0.0, 0.2, 0.2));
        i.container_w = 0.0;
        assert!(i.project_free(200.0, 200.0).is_none());
    }

    #[test]
    fn free_projection_needs_a_starting_rect() {
        let i = base(InteractionKind::Drag, GridPosition::new(0, 0, 2, 2));
        assert!(i.project_free(200.0, 200.0).is_none());
    }
}
