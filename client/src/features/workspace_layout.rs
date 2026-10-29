use dioxus::prelude::*;
use dioxus_grid_layout::{FloatRect, GridPosition, LayoutStore};

use super::stream_layout::Arrangement;

const GUILD_RAIL_WIDTH: f64 = 96.0;

pub(super) fn use_arrangement(
    layout: LayoutStore,
    settings: Signal<crate::settings::ClientSettings>,
    active: Memo<bool>,
    width: Signal<Option<f64>>,
    editing: Signal<bool>,
    gap: f64,
) {
    let mut normal = use_signal(|| None::<Arrangement>);
    use_effect(move || {
        let active = active();
        let width = width();
        let editing = editing();
        let current = Arrangement {
            cells: layout.snapshot(),
            free: layout.free_snapshot(),
        };
        let mut next = current.clone();
        if active && normal.peek().is_none() {
            let mut base = current.clone();
            base.cells.retain(|(id, _)| id != "streams");
            base.free.retain(|(id, _)| id != "streams");
            normalize(&mut base, width, editing, gap);
            let saved = settings.peek();
            next = if saved
                .stream_layout_cells
                .iter()
                .any(|(id, _)| id == "streams")
            {
                let mut saved = Arrangement {
                    cells: saved
                        .stream_layout_cells
                        .iter()
                        .map(|(id, [x, y, w, h])| (id.clone(), GridPosition::new(*x, *y, *w, *h)))
                        .collect(),
                    free: saved
                        .stream_layout_free
                        .iter()
                        .map(|(id, [x, y, w, h])| (id.clone(), FloatRect::new(*x, *y, *w, *h)))
                        .collect(),
                };
                normalize(&mut saved, width, editing, gap);
                super::stream_layout::restore_streams(
                    super::stream_layout::above_chat(base.clone(), 12, 30),
                    saved,
                )
            } else {
                super::stream_layout::above_chat(base.clone(), 12, 30)
            };
            drop(saved);
            normal.set(Some(base));
        } else if !active {
            let previous = normal.peek().clone();
            if let Some(previous) = previous {
                normal.set(None);
                next = previous;
            }
        }
        normalize(&mut next, width, editing, gap);
        let mut store = layout;
        if next.cells != current.cells {
            store.restore(next.cells, next.free);
        } else {
            for (id, rect) in next.free {
                if current
                    .free
                    .iter()
                    .find(|(key, _)| key == &id)
                    .map(|(_, rect)| *rect)
                    != Some(rect)
                {
                    store.set_free(id, rect);
                }
            }
        }
    });
}

fn normalize(base: &mut Arrangement, width: Option<f64>, editing: bool, gap: f64) {
    if !editing
        && let Some(width) = width
        && let Some(rects) = compact_rail(base, width, gap)
    {
        base.free = rects;
    }
}

pub(super) fn compact_rail(
    base: &Arrangement,
    width: f64,
    gap: f64,
) -> Option<Vec<(String, FloatRect)>> {
    if !width.is_finite() || width < 2.0 * (GUILD_RAIL_WIDTH + gap) {
        return None;
    }
    let target = (GUILD_RAIL_WIDTH + gap) / width;
    let resolved: Vec<_> = base
        .cells
        .iter()
        .map(|(id, cell)| {
            let rect = base
                .free
                .iter()
                .find(|(key, _)| key == id)
                .map(|(_, rect)| *rect)
                .unwrap_or_else(|| {
                    FloatRect::new(
                        cell.x as f64 / 12.0,
                        cell.y as f64 / 30.0,
                        cell.w as f64 / 12.0,
                        cell.h as f64 / 30.0,
                    )
                    .clamp_inside()
                });
            (id.clone(), rect)
        })
        .collect();
    let (_, rail) = resolved.iter().find(|(id, _)| id == "guilds")?;
    if rail.x.abs() > 1e-6
        || rail.y.abs() > 1e-6
        || (rail.h - 1.0).abs() > 1e-6
        || !rail.w.is_finite()
        || rail.w <= 0.0
        || rail.w >= 1.0
        || (rail.w - target).abs() * width < 0.1
    {
        return None;
    }
    let old = rail.w;
    let project = |x: f64| {
        if x <= old {
            x * target / old
        } else {
            target + (x - old) * (1.0 - target) / (1.0 - old)
        }
    };
    Some(
        resolved
            .into_iter()
            .map(|(id, rect)| {
                let left = project(rect.x);
                let right = project(rect.x + rect.w);
                (id, FloatRect::new(left, rect.y, right - left, rect.h))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Copy)]
    struct Controls {
        active: Signal<bool>,
        width: Signal<Option<f64>>,
        layout: LayoutStore,
    }

    type Export = Rc<RefCell<Option<Controls>>>;

    fn harness(export: Export) -> Element {
        let active = use_signal(|| false);
        let width = use_signal(|| Some(2560.0));
        let editing = use_signal(|| false);
        let active_memo = use_memo(move || *active.read());
        let layout = dioxus_grid_layout::use_layout_store(super::super::workspace::tpl_default);
        let settings = use_signal(|| {
            let saved = super::super::stream_layout::above_chat(
                Arrangement {
                    cells: super::super::workspace::tpl_default(),
                    free: vec![],
                },
                12,
                30,
            );
            crate::settings::ClientSettings {
                stream_layout_cells: saved
                    .cells
                    .into_iter()
                    .map(|(id, p)| (id, [p.x, p.y, p.w, p.h]))
                    .collect(),
                stream_layout_free: saved
                    .free
                    .into_iter()
                    .map(|(id, p)| (id, [p.x, p.y, p.w, p.h]))
                    .collect(),
                ..Default::default()
            }
        });
        use_arrangement(layout, settings, active_memo, width, editing, 8.0);
        *export.borrow_mut() = Some(Controls {
            active,
            width,
            layout,
        });
        rsx! { div {} }
    }

    #[tokio::test]
    async fn watching_restoring_and_resizing_keep_the_live_rail_compact() {
        let export: Export = Rc::new(RefCell::new(None));
        let mut dom = VirtualDom::new_with_props(harness, export.clone());
        dom.rebuild_in_place();
        for _ in 0..4 {
            dom.render_immediate_to_vec();
        }
        let mut controls = export.borrow().unwrap();
        for (active, width) in [
            (false, 2560.0),
            (true, 2560.0),
            (true, 3840.0),
            (false, 3840.0),
            (true, 1920.0),
            (false, 1920.0),
        ] {
            controls.active.set(active);
            controls.width.set(Some(width));
            for _ in 0..4 {
                dom.render_immediate_to_vec();
            }
            let rail = controls.layout.get_free("guilds").unwrap();
            assert!((rail.w * width - 8.0 - 96.0).abs() < 1e-6);
            assert_eq!(controls.layout.get("streams").is_some(), active);
            let channels = controls.layout.get_free("channels").unwrap();
            assert!((channels.x - rail.w).abs() < 1e-6);
            let chat = controls.layout.get_free("chat").unwrap();
            assert!((chat.x - channels.x - channels.w).abs() < 1e-6);
            if active {
                let stream = controls.layout.get_free("streams").unwrap();
                assert!((stream.x - chat.x).abs() < 1e-6);
                assert!((stream.y + stream.h - chat.y).abs() < 1e-6);
            }
        }
    }
}
