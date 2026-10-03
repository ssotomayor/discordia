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
    let top = (original.h * 3 / 4)
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
        FloatRect::new(rect.x, rect.y, rect.w, rect.h * 0.75),
    ));
    base.free.push((
        "chat".into(),
        FloatRect::new(rect.x, rect.y + rect.h * 0.75, rect.w, rect.h * 0.25),
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!((streams.h - 0.6).abs() < 1e-9);
        assert!((chat.y - 0.7).abs() < 1e-9);
        assert!((chat.h - 0.2).abs() < 1e-9);
        assert_eq!(split.cells[0].1, GridPosition::new(4, 21, 6, 6));
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
        assert_eq!(chat.y, 0.75);
        assert_eq!(chat.h, 0.25);
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
