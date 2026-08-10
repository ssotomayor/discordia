//! Vertical-compact collision resolution.
//!
//! Given a layout (a list of `(id, GridPosition)` pairs) and the set of ids
//! that must NOT move, [`compact_vertical`]:
//!
//! 1. Treats every id in `immovable` as a fixed obstacle. This includes:
//!    - the item the user is currently dragging or resizing (it's at the
//!      position the user just projected for it), and
//!    - any item the host marked `pinned`.
//! 2. Walks the rest of the layout top-down and pushes any item that
//!    overlaps a neighbour downwards until it doesn't overlap.
//! 3. Walks the layout top-down again and pulls each movable item upwards
//!    as far as it can go without colliding — gravity-up compaction.
//!
//! Result: no overlaps anywhere; movable items occupy the highest legal
//! row given the obstacles' positions; layout converges back to the
//! original arrangement when the active item moves away.

use std::collections::HashSet;

use crate::layout::GridPosition;

pub(crate) fn overlaps(a: &GridPosition, b: &GridPosition) -> bool {
    a.x < b.x + b.w && a.x + a.w > b.x && a.y < b.y + b.h && a.y + a.h > b.y
}

pub(crate) fn compact_vertical(layout: &mut [(String, GridPosition)], immovable: &HashSet<String>) {
    // Push down to resolve any overlaps with immovable items or higher items.
    let mut order: Vec<usize> = (0..layout.len()).collect();
    order.sort_by_key(|&i| (layout[i].1.y, layout[i].1.x));

    for &i in &order {
        if immovable.contains(&layout[i].0) {
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
        if immovable.contains(&layout[i].0) {
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

    fn immovable(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn overlap_detector() {
        assert!(overlaps(&pos(0, 0, 2, 2), &pos(1, 1, 2, 2)));
        assert!(!overlaps(&pos(0, 0, 2, 2), &pos(2, 0, 2, 2)));
        assert!(!overlaps(&pos(0, 0, 2, 2), &pos(0, 2, 2, 2)));
    }

    #[test]
    fn pushes_overlapping_item_down() {
        let mut layout = vec![("a".into(), pos(0, 0, 4, 2)), ("b".into(), pos(0, 0, 4, 2))];
        compact_vertical(&mut layout, &immovable(&["a"]));
        assert_eq!(layout[0].1, pos(0, 0, 4, 2));
        assert_eq!(layout[1].1, pos(0, 2, 4, 2));
    }

    #[test]
    fn compacts_upwards_when_space_frees() {
        let mut layout = vec![
            ("a".into(), pos(0, 10, 4, 2)),
            ("b".into(), pos(0, 6, 4, 2)),
        ];
        compact_vertical(&mut layout, &immovable(&["a"]));
        assert_eq!(layout[0].1.y, 10);
        assert_eq!(layout[1].1.y, 0);
    }

    #[test]
    fn cascading_pushes() {
        let mut layout = vec![
            ("a".into(), pos(0, 0, 4, 4)),
            ("b".into(), pos(0, 0, 4, 2)),
            ("c".into(), pos(0, 1, 4, 2)),
        ];
        compact_vertical(&mut layout, &immovable(&["a"]));
        assert!(layout[1].1.y >= 4);
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
        compact_vertical(&mut layout, &immovable(&["a"]));
        assert_eq!(layout, before);
    }

    #[test]
    fn pinned_items_are_skipped_in_push_and_gravity() {
        // 'p' is a pinned widget at y=5. Even if there's empty space above,
        // it must NOT compact upward.
        let mut layout = vec![
            ("active".into(), pos(0, 0, 4, 2)),
            ("p".into(), pos(0, 5, 4, 2)),
        ];
        compact_vertical(&mut layout, &immovable(&["active", "p"]));
        assert_eq!(layout[1].1.y, 5);
    }

    #[test]
    fn other_items_compact_around_pinned() {
        // 'p' is pinned at y=2. 'b' starts at y=10. 'b' must compact up but
        // can only go as high as y=4 (below the pinned 4-row obstacle).
        let mut layout = vec![
            ("active".into(), pos(0, 0, 4, 2)),
            ("p".into(), pos(0, 2, 4, 2)),
            ("b".into(), pos(0, 10, 4, 2)),
        ];
        compact_vertical(&mut layout, &immovable(&["active", "p"]));
        assert_eq!(layout[1].1.y, 2); // pinned unchanged
        assert_eq!(layout[2].1.y, 4); // b stops just below pinned
    }
}
