use dioxus_grid_layout::{FloatRect, GridPosition};

#[derive(Clone, Default, PartialEq)]
pub(super) struct Arrangement {
    pub cells: Vec<(String, GridPosition)>,
    pub free: Vec<(String, FloatRect)>,
}

pub(super) fn above_chat(mut base: Arrangement, cols: u32, rows: u32) -> Arrangement {
    let Some((_, chat)) = base.cells.iter_mut().find(|(id, _)| id == "chat") else {
        return base;
    };
    let original = *chat;
    let top = (original.h * 19 / 30)
        .max(1)
        .min(original.h.saturating_sub(1));
    let streams = GridPosition::new(original.x, original.y, original.w, top);
    chat.y += top;
    chat.h -= top;
    let rect = base
        .free
        .iter()
        .find(|(id, _)| id == "chat")
        .map(|(_, r)| *r)
        .unwrap_or_else(|| {
            FloatRect::new(
                original.x as f64 / cols as f64,
                original.y as f64 / rows as f64,
                original.w as f64 / cols as f64,
                original.h as f64 / rows as f64,
            )
        });
    base.cells.retain(|(id, _)| id != "streams");
    base.cells.push(("streams".into(), streams));
    base.free.retain(|(id, _)| id != "chat" && id != "streams");
    base.free.push((
        "streams".into(),
        FloatRect::new(rect.x, rect.y, rect.w, rect.h * (19.0 / 30.0)),
    ));
    base.free.push((
        "chat".into(),
        FloatRect::new(
            rect.x,
            rect.y + rect.h * (19.0 / 30.0),
            rect.w,
            rect.h * (11.0 / 30.0),
        ),
    ));
    base
}

pub(super) fn restore_streams(mut base: Arrangement, saved: Arrangement) -> Arrangement {
    for id in ["streams", "chat"] {
        if let Some(cell) = saved.cells.iter().find(|(key, _)| key == id) {
            base.cells.retain(|(key, _)| key != id);
            base.cells.push(cell.clone());
            base.free.retain(|(key, _)| key != id);
            if let Some(rect) = saved.free.iter().find(|(key, _)| key == id) {
                base.free.push(rect.clone());
            }
        }
    }
    base
}

pub(super) fn close_streams(mut active: Arrangement, normal: &Arrangement) -> Arrangement {
    let rects = super::workspace_layout::resolved(&active);
    let chat = rects.iter().find(|(id, _)| id == "chat");
    let stream = rects.iter().find(|(id, _)| id == "streams");
    let mut merge = false;
    if let (Some((_, chat)), Some((_, stream))) = (chat, stream) {
        merge = (chat.x - stream.x).abs() < 1e-6
            && (chat.w - stream.w).abs() < 1e-6
            && (stream.y + stream.h - chat.y).abs() < 1e-6;
        if merge {
            active.free.retain(|(id, _)| id != "chat");
            active.free.push((
                "chat".into(),
                FloatRect::new(stream.x, stream.y, stream.w, chat.y + chat.h - stream.y),
            ));
        }
    }
    if !merge && super::workspace_layout::stream_layout_matches_workspace(normal, &active) {
        active.cells.retain(|(id, _)| id != "chat");
        active
            .cells
            .extend(normal.cells.iter().filter(|(id, _)| id == "chat").cloned());
        active.free.retain(|(id, _)| id != "chat");
        active
            .free
            .extend(normal.free.iter().filter(|(id, _)| id == "chat").cloned());
    }
    let stream = active
        .cells
        .iter()
        .find(|(id, _)| id == "streams")
        .map(|(_, cell)| *cell);
    if merge
        && let Some(stream) = stream
        && let Some((_, chat)) = active.cells.iter_mut().find(|(id, _)| id == "chat")
    {
        let right = (chat.x + chat.w).max(stream.x + stream.w);
        let bottom = (chat.y + chat.h).max(stream.y + stream.h);
        chat.x = chat.x.min(stream.x);
        chat.y = chat.y.min(stream.y);
        chat.w = right - chat.x;
        chat.h = bottom - chat.y;
    }
    active.cells.retain(|(id, _)| id != "streams");
    active.free.retain(|(id, _)| id != "streams");
    active
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_a_separately_moved_stream_restores_chat_without_covering_other_panels() {
        let normal = Arrangement {
            cells: super::super::workspace::tpl_default(),
            free: Vec::new(),
        };
        let mut active = above_chat(normal.clone(), 12, 30);
        active
            .free
            .iter_mut()
            .find(|(id, _)| id == "streams")
            .unwrap()
            .1 = FloatRect::new(0.0, 0.0, 0.2, 0.5);
        let closed = close_streams(active, &normal);
        assert_eq!(
            closed.cells.iter().find(|(id, _)| id == "chat"),
            normal.cells.iter().find(|(id, _)| id == "chat")
        );
        assert!(
            closed
                .free
                .iter()
                .all(|(id, _)| id != "chat" && id != "streams")
        );
    }

    #[test]
    fn splitting_a_custom_chat_preserves_other_modules_and_uses_its_bounds() {
        let normal = Arrangement {
            cells: vec![
                ("chat".into(), GridPosition::new(4, 3, 6, 24)),
                ("members".into(), GridPosition::new(10, 0, 2, 30)),
            ],
            free: vec![
                ("chat".into(), FloatRect::new(0.31, 0.1, 0.5, 0.8)),
                ("members".into(), FloatRect::new(0.85, 0.0, 0.15, 1.0)),
            ],
        };
        let split = above_chat(normal.clone(), 12, 30);
        assert_eq!(split.cells[1], normal.cells[1]);
        assert_eq!(split.free[0], normal.free[1]);
        let streams = split.free.iter().find(|(id, _)| id == "streams").unwrap().1;
        let chat = split.free.iter().find(|(id, _)| id == "chat").unwrap().1;
        assert_eq!((streams.x, streams.y, streams.w), (0.31, 0.1, 0.5));
        assert!((streams.h - 0.8 * (19.0 / 30.0)).abs() < 1e-9);
        assert!((chat.y - (0.1 + 0.8 * (19.0 / 30.0))).abs() < 1e-9);
        assert!((chat.h - 0.8 * (11.0 / 30.0)).abs() < 1e-9);
        assert_eq!(split.cells[0].1, GridPosition::new(4, 18, 6, 9));
    }

    #[test]
    fn old_cell_only_layouts_gain_a_stream_module_without_moving_members() {
        let original = super::super::workspace::tpl_default();
        let split = above_chat(
            Arrangement {
                cells: original.clone(),
                free: vec![],
            },
            12,
            30,
        );
        for id in ["guilds", "channels", "members"] {
            assert_eq!(
                split.cells.iter().find(|(key, _)| key == id),
                original.iter().find(|(key, _)| key == id)
            );
        }
        let chat = split.free.iter().find(|(id, _)| id == "chat").unwrap().1;
        assert!((chat.y - 19.0 / 30.0).abs() < 1e-9);
        assert!((chat.h - 11.0 / 30.0).abs() < 1e-9);
    }

    #[test]
    fn restoring_stream_sizes_keeps_the_current_lobby_and_member_positions() {
        let normal = Arrangement {
            cells: super::super::workspace::tpl_default(),
            free: vec![],
        };
        let base = above_chat(normal, 12, 30);
        let mut saved = base.clone();
        saved
            .cells
            .iter_mut()
            .find(|(id, _)| id == "members")
            .unwrap()
            .1
            .x = 0;
        saved
            .free
            .iter_mut()
            .find(|(id, _)| id == "streams")
            .unwrap()
            .1
            .h = 0.6;
        let restored = restore_streams(base.clone(), saved);
        assert_eq!(
            restored.cells.iter().find(|(id, _)| id == "members"),
            base.cells.iter().find(|(id, _)| id == "members")
        );
        assert_eq!(
            restored
                .free
                .iter()
                .find(|(id, _)| id == "streams")
                .unwrap()
                .1
                .h,
            0.6
        );
    }
}
