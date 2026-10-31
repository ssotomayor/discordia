use dioxus::prelude::*;
use dioxus_grid_layout::{FloatRect, GridPosition, LayoutStore};

use super::stream_layout::Arrangement;

/// The rail's thickness in pixels, across a left dock or along a top/bottom one.
pub(crate) const RAIL_DEFAULT: u16 = 64;
/// Narrower clips the "+ server" button.
pub(super) const RAIL_MIN: u16 = 60;
const RAIL_MAX: u16 = 240;

/// The workspace in pixels.
pub(super) type Size = (f64, f64);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Dock {
    Left,
    Top,
    Bottom,
}

pub(super) fn use_arrangement(
    layout: LayoutStore,
    mut settings: Signal<crate::settings::ClientSettings>,
    active: Memo<bool>,
    size: Signal<Option<Size>>,
    editing: Signal<bool>,
    gap: f64,
) {
    let mut normal = use_signal(|| None::<Arrangement>);
    let mut was_editing = use_signal(|| false);
    use_effect(move || {
        let active = active();
        let size = size();
        let editing = editing();
        let current = Arrangement {
            cells: layout.snapshot(),
            free: layout.free_snapshot(),
        };
        // A rail resized by hand keeps that thickness from here on.
        if *was_editing.peek() != editing {
            was_editing.set(editing);
            if !editing
                && let Some(size) = size
                && let Some(px) = rail_thickness(&current, size, gap)
            {
                let px = (px.round() as u16).clamp(RAIL_MIN, RAIL_MAX);
                if settings.peek().rail_size != px {
                    let mut next = settings.peek().clone();
                    next.rail_size = px;
                    crate::settings::save(&next);
                    settings.set(next);
                }
            }
        }
        let thickness = rail_px(&settings.peek());
        let mut next = current.clone();
        if active && normal.peek().is_none() {
            let mut base = current.clone();
            base.cells.retain(|(id, _)| id != "streams");
            base.free.retain(|(id, _)| id != "streams");
            normalize(&mut base, size, editing, gap, thickness);
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
                normalize(&mut saved, size, editing, gap, thickness);
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
        normalize(&mut next, size, editing, gap, thickness);
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

pub(super) fn rail_px(settings: &crate::settings::ClientSettings) -> f64 {
    f64::from(settings.rail_size.clamp(RAIL_MIN, RAIL_MAX))
}

fn normalize(base: &mut Arrangement, size: Option<Size>, editing: bool, gap: f64, thickness: f64) {
    if !editing
        && let Some(size) = size
        && let Some(rects) = compact_rail(base, size, gap, thickness)
    {
        base.free = rects;
    }
}

/// Every panel as a fraction of the workspace, whether it was last placed
/// freely or only ever had grid cells.
fn resolved(base: &Arrangement) -> Vec<(String, FloatRect)> {
    base.cells
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
        .collect()
}

/// Which edge the rail spans, if it spans one; a rail moved anywhere else is
/// left where it was put.
pub(super) fn dock_of(rail: FloatRect) -> Option<Dock> {
    let near = |a: f64, b: f64| (a - b).abs() <= 1e-6;
    let inside = |extent: f64| extent.is_finite() && extent > 0.0 && extent < 1.0;
    if near(rail.x, 0.0) && near(rail.y, 0.0) && near(rail.h, 1.0) && inside(rail.w) {
        Some(Dock::Left)
    } else if near(rail.x, 0.0) && near(rail.w, 1.0) && inside(rail.h) {
        if near(rail.y, 0.0) {
            Some(Dock::Top)
        } else if near(rail.y + rail.h, 1.0) {
            Some(Dock::Bottom)
        } else {
            None
        }
    } else {
        None
    }
}

/// The docked rail's panel thickness in pixels (a free rect includes the gap).
fn rail_thickness(base: &Arrangement, (width, height): Size, gap: f64) -> Option<f64> {
    let rail = resolved(base).into_iter().find(|(id, _)| id == "guilds")?.1;
    match dock_of(rail)? {
        Dock::Left => Some(rail.w * width - gap),
        Dock::Top | Dock::Bottom => Some(rail.h * height - gap),
    }
}

/// Holds a docked rail at `thickness` pixels as the window resizes, handing
/// the difference to the panels beside it.
pub(super) fn compact_rail(
    base: &Arrangement,
    (width, height): Size,
    gap: f64,
    thickness: f64,
) -> Option<Vec<(String, FloatRect)>> {
    let resolved = resolved(base);
    let (_, rail) = resolved.iter().find(|(id, _)| id == "guilds")?;
    let dock = dock_of(*rail)?;
    let length = if dock == Dock::Left { width } else { height };
    if !length.is_finite() || length < 2.0 * (thickness + gap) {
        return None;
    }
    let target = (thickness + gap) / length;
    let old = if dock == Dock::Left { rail.w } else { rail.h };
    if (old - target).abs() * length < 0.1 {
        return None;
    }
    let project = |x: f64| {
        if x <= old {
            x * target / old
        } else {
            target + (x - old) * (1.0 - target) / (1.0 - old)
        }
    };
    // A bottom rail is a top rail seen from the other end.
    let along = |start: f64, extent: f64| match dock {
        Dock::Bottom => {
            let begin = 1.0 - project(1.0 - start);
            let end = 1.0 - project(1.0 - start - extent);
            (begin, end - begin)
        }
        _ => {
            let begin = project(start);
            (begin, project(start + extent) - begin)
        }
    };
    Some(
        resolved
            .into_iter()
            .map(|(id, rect)| {
                let rect = if dock == Dock::Left {
                    let (x, w) = along(rect.x, rect.w);
                    FloatRect::new(x, rect.y, w, rect.h)
                } else {
                    let (y, h) = along(rect.y, rect.h);
                    FloatRect::new(rect.x, y, rect.w, h)
                };
                (id, rect)
            })
            .collect(),
    )
}

/// Puts the rail along `dock` and fits the other panels, as arranged, into
/// what is left.
pub(super) fn dock_rail(
    base: &Arrangement,
    dock: Dock,
    (width, height): Size,
    gap: f64,
    thickness: f64,
) -> Option<Vec<(String, FloatRect)>> {
    let resolved = resolved(base);
    let length = if dock == Dock::Left { width } else { height };
    if !length.is_finite() || length < 2.0 * (thickness + gap) {
        return None;
    }
    let t = (thickness + gap) / length;
    let (rail, room) = match dock {
        Dock::Left => (
            FloatRect::new(0.0, 0.0, t, 1.0),
            FloatRect::new(t, 0.0, 1.0 - t, 1.0),
        ),
        Dock::Top => (
            FloatRect::new(0.0, 0.0, 1.0, t),
            FloatRect::new(0.0, t, 1.0, 1.0 - t),
        ),
        Dock::Bottom => (
            FloatRect::new(0.0, 1.0 - t, 1.0, t),
            FloatRect::new(0.0, 0.0, 1.0, 1.0 - t),
        ),
    };
    let others: Vec<FloatRect> = resolved
        .iter()
        .filter(|(id, _)| id != "guilds")
        .map(|(_, rect)| *rect)
        .collect();
    let left = others.iter().map(|r| r.x).fold(f64::INFINITY, f64::min);
    let top = others.iter().map(|r| r.y).fold(f64::INFINITY, f64::min);
    let right = others
        .iter()
        .map(|r| r.x + r.w)
        .fold(f64::NEG_INFINITY, f64::max);
    let bottom = others
        .iter()
        .map(|r| r.y + r.h)
        .fold(f64::NEG_INFINITY, f64::max);
    let (span_x, span_y) = (right - left, bottom - top);
    if !(span_x > 0.0 && span_y > 0.0) {
        return None;
    }
    Some(
        resolved
            .into_iter()
            .map(|(id, rect)| {
                if id == "guilds" {
                    return (id, rail);
                }
                let x = room.x + (rect.x - left) / span_x * room.w;
                let y = room.y + (rect.y - top) / span_y * room.h;
                let w = rect.w / span_x * room.w;
                let h = rect.h / span_y * room.h;
                (id, FloatRect::new(x, y, w, h))
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
        size: Signal<Option<Size>>,
        layout: LayoutStore,
    }

    type Export = Rc<RefCell<Option<Controls>>>;

    fn harness(export: Export) -> Element {
        let active = use_signal(|| false);
        let size = use_signal(|| Some((2560.0, 1440.0)));
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
        use_arrangement(layout, settings, active_memo, size, editing, 8.0);
        *export.borrow_mut() = Some(Controls {
            active,
            size,
            layout,
        });
        rsx! { div {} }
    }

    /// What "Done rearranging" saves: the docked rail's panel pixels, from
    /// whichever side it is docked on; a floating rail saves nothing.
    #[test]
    fn a_hand_resized_rail_is_measured_from_its_dock() {
        let size = (2000.0, 1000.0);
        let with_rail = |rail: FloatRect| Arrangement {
            cells: super::super::workspace::tpl_default(),
            free: vec![("guilds".into(), rail)],
        };
        let left = with_rail(FloatRect::new(0.0, 0.0, 88.0 / 2000.0, 1.0));
        assert!((rail_thickness(&left, size, 8.0).unwrap() - 80.0).abs() < 1e-9);
        let bottom = with_rail(FloatRect::new(0.0, 1.0 - 78.0 / 1000.0, 1.0, 78.0 / 1000.0));
        assert!((rail_thickness(&bottom, size, 8.0).unwrap() - 70.0).abs() < 1e-9);
        let floating = with_rail(FloatRect::new(0.3, 0.2, 0.1, 0.5));
        assert_eq!(rail_thickness(&floating, size, 8.0), None);
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
            controls.size.set(Some((width, 1440.0)));
            for _ in 0..4 {
                dom.render_immediate_to_vec();
            }
            let rail = controls.layout.get_free("guilds").unwrap();
            assert!((rail.w * width - 8.0 - f64::from(RAIL_DEFAULT)).abs() < 1e-6);
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
