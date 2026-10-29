use dioxus_grid_layout::FloatRect;

use super::stream_layout::Arrangement;

const GUILD_RAIL_WIDTH: f64 = 96.0;

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
