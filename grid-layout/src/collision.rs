//! Vertical-compact collision resolution.
//!
//! Given a layout (a list of `(id, GridPosition)` pairs) and the id of the
//! item the user is currently dragging or resizing, [`compact_vertical`]:
//!
//! 1. Treats the active item as immovable (it's at the position the user
//!    just projected for it).
//! 2. Walks the rest of the layout top-down and pushes any item that
//!    overlaps a higher neighbour downwards until it doesn't overlap.
//! 3. Walks the layout top-down again and pulls each non-active item
//!    upwards as far as it can go without colliding — gravity-up compaction.
//!
//! Result: no overlaps anywhere; non-active items occupy the highest legal
//! row given the active item's current position; layout converges back to
//! the original arrangement when the active item moves away.

use crate::layout::GridPosition;

pub(crate) fn overlaps(a: &GridPosition, b: &GridPosition) -> bool {
    a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y
}

pub(crate) fn compact_vertical(layout: &mut [(String, GridPosition)], pinned_id: &str) {
    // Push down to resolve any overlaps with the pinned item or higher items.
    let mut order: Vec<usize> = (0..layout.len()).collect();
    order.sort_by_key(|&i| (layout[i].1.y, layout[i].1.x));

    for &i in &order {
        if layout[i].0 == pinned_id {
            continue;
        }
        loop {
            let pos_i = layout[i].1;
            let collides = layout
                .iter()
                .enumerate()
                .any(|(j, (_, p))| j != i && overlaps(&pos_i, p));
            if !collides {
                break;
            }
            layout[i].1.y += 1;
        }
    }

    // Compact upwards.
    order.sort_by_key(|&i| (layout[i].1.y, layout[i].1.x));
    for &i in &order {
        if layout[i].0 == pinned_id {
            continue;
        }
        while layout[i].1.y > 0 {
            let candidate = GridPosition {
                y: layout[i].1.y - 1,
                ..layout[i].1
            };
            let collides = layout
                .iter()
                .enumerate()
                .any(|(j, (_, p))| j != i && overlaps(&candidate, p));
            if collides {
                break;
            }
            layout[i].1.y -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: u32, y: u32, w: u32, h: u32) -> GridPosition {
        GridPosition::new(x, y, w, h)
    }

    #[test]
    fn overlap_detector() {
        assert!(overlaps(&pos(0, 0, 2, 2), &pos(1, 1, 2, 2)));
        assert!(!overlaps(&pos(0, 0, 2, 2), &pos(2, 0, 2, 2))); // touching, not overlapping
        assert!(!overlaps(&pos(0, 0, 2, 2), &pos(0, 2, 2, 2))); // touching vertically
    }

    #[test]
    fn pushes_overlapping_item_down() {
        let mut layout = vec![
            ("a".into(), pos(0, 0, 4, 2)), // active item moves here
            ("b".into(), pos(0, 0, 4, 2)), // was at the same spot — must shift down
        ];
        compact_vertical(&mut layout, "a");
        assert_eq!(layout[0].1, pos(0, 0, 4, 2));
        assert_eq!(layout[1].1, pos(0, 2, 4, 2));
    }

    #[test]
    fn compacts_upwards_when_space_frees() {
        // 'a' was originally at y=4 blocking 'b' at y=6. If we move 'a' down
        // to y=10 (out of the way), 'b' should float up to 0.
        let mut layout = vec![
            ("a".into(), pos(0, 10, 4, 2)),
            ("b".into(), pos(0, 6, 4, 2)),
        ];
        compact_vertical(&mut layout, "a");
        assert_eq!(layout[0].1.y, 10); // active unchanged
        assert_eq!(layout[1].1.y, 0); // compacted up
    }

    #[test]
    fn cascading_pushes() {
        // Active 'a' at (0,0,4,4) forces 'b' and 'c' below in a stack.
        let mut layout = vec![
            ("a".into(), pos(0, 0, 4, 4)),
            ("b".into(), pos(0, 0, 4, 2)),
            ("c".into(), pos(0, 1, 4, 2)),
        ];
        compact_vertical(&mut layout, "a");
        // b must come after a (which ends at y=4)
        assert!(layout[1].1.y >= 4);
        // c must come after b
        assert!(layout[2].1.y >= layout[1].1.y + layout[1].1.h);
    }

    #[test]
    fn no_overlap_no_change() {
        let mut layout = vec![
            ("a".into(), pos(0, 0, 4, 2)),
            ("b".into(), pos(4, 0, 4, 2)),
            ("c".into(), pos(8, 0, 4, 2)),
        ];
        let before = layout.clone();
        compact_vertical(&mut layout, "a");
        assert_eq!(layout, before);
    }
}
