use dioxus::document;
use dioxus::prelude::*;

use crate::protocol::rendezvous::GeoPoint;

/// Inlined like the LiveKit bundle: the join screen is shown before any
/// network is trusted, and it must draw with none at all.
const GLOBE_JS: &str = include_str!("../../assets/globe.js");

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GlobePin {
    pub code: String,
    pub label: String,
    pub lat: f64,
    pub lon: f64,
    pub fresh: bool,
}

/// A dotted globe on a canvas. Drag spins it; a pin click reports its code,
/// and in `pick` mode a click on bare ground reports where.
#[component]
pub fn Globe(
    pins: ReadSignal<Vec<GlobePin>>,
    selected: ReadSignal<Option<String>>,
    pick: ReadSignal<bool>,
    place: ReadSignal<Option<GeoPoint>>,
    height: u32,
    on_pick: EventHandler<String>,
    on_place: EventHandler<GeoPoint>,
) -> Element {
    let id = use_hook(|| format!("globe-{}", uuid::Uuid::new_v4().simple()));

    let id_mount = id.clone();
    let onmounted = move |_| {
        let id = id_mount.clone();
        spawn(async move {
            let mut eval = document::eval(&format!(
                "{GLOBE_JS}\nwindow.dxGlobe.mount({id:?}, function (m) {{ dioxus.send(m); }});"
            ));
            while let Ok(msg) = eval.recv::<serde_json::Value>().await {
                match msg.get("__dxf").and_then(|v| v.as_str()) {
                    Some("globe-pick") => {
                        if let Some(code) = msg.get("code").and_then(|v| v.as_str()) {
                            on_pick.call(code.to_string());
                        }
                    }
                    Some("globe-place") if pick() => {
                        if let Ok(request) = serde_json::from_value::<
                            super::globe_geometry::PlaceRequest,
                        >(msg.clone())
                            && let Some(point) = request.point()
                        {
                            on_place.call(point);
                        }
                    }
                    Some("globe-resize") => {
                        if let Some(radius) = msg.get("radius").and_then(|v| v.as_f64()) {
                            match tokio::task::spawn_blocking(move || {
                                super::globe_geometry::dots(radius)
                            })
                            .await
                            {
                                Ok(dots) => {
                                    if let Ok(json) = serde_json::to_string(&dots) {
                                        let _ = document::eval(&format!(
                                            "window.dxGlobe && window.dxGlobe.setDots({id:?}, {radius}, {json});"
                                        ));
                                    }
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "Globe geometry worker failed")
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
    };

    // The script parks a call made before mount and replays it, so these may
    // fire in either order relative to `onmounted`.
    let id_pins = id.clone();
    use_effect(move || {
        let prepared: Vec<_> = pins().into_iter().filter_map(|pin| {
            let (u, focus) = super::globe_geometry::coordinates(GeoPoint { lat: pin.lat, lon: pin.lon })?;
            Some(serde_json::json!({ "code": pin.code, "label": pin.label, "fresh": pin.fresh, "u": u, "focus": focus }))
        }).collect();
        let json = serde_json::to_string(&prepared).unwrap_or_else(|_| "[]".into());
        let _ = document::eval(&format!(
            "{GLOBE_JS}\nwindow.dxGlobe.setPins({id_pins:?}, {json});"
        ));
    });
    let id_sel = id.clone();
    use_effect(move || {
        let code = serde_json::to_string(&selected()).unwrap_or_else(|_| "null".into());
        let _ = document::eval(&format!(
            "{GLOBE_JS}\nwindow.dxGlobe.setSelected({id_sel:?}, {code});"
        ));
    });
    let id_pick = id.clone();
    use_effect(move || {
        let _ = document::eval(&format!(
            "{GLOBE_JS}\nwindow.dxGlobe.setPick({id_pick:?}, {});",
            pick()
        ));
    });
    let id_place = id.clone();
    use_effect(move || {
        let prepared = place().and_then(|point| {
            let (u, focus) = super::globe_geometry::coordinates(point)?;
            Some(serde_json::json!({ "u": u, "focus": focus }))
        });
        let json = serde_json::to_string(&prepared).unwrap_or_else(|_| "null".into());
        let _ = document::eval(&format!(
            "{GLOBE_JS}\nwindow.dxGlobe.setPlace({id_place:?}, {json});"
        ));
    });

    let id_drop = id.clone();
    use_drop(move || {
        let _ = document::eval(&format!("{GLOBE_JS}\nwindow.dxGlobe.destroy({id_drop:?});"));
    });

    rsx! {
        canvas {
            id: "{id}",
            class: "block w-full select-none",
            style: "height: {height}px; touch-action: none; cursor: grab;",
            onmounted,
        }
    }
}

/// `17.3°N 62.7°W`, the way a pin is read back to the person who placed it.
pub fn describe(p: GeoPoint) -> String {
    let ns = if p.lat < 0.0 { 'S' } else { 'N' };
    let ew = if p.lon < 0.0 { 'W' } else { 'E' };
    format!("{:.1}°{ns} {:.1}°{ew}", p.lat.abs(), p.lon.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_reads_as_a_bearing() {
        assert_eq!(
            describe(GeoPoint {
                lat: 17.3,
                lon: -62.7
            }),
            "17.3°N 62.7°W"
        );
        assert_eq!(
            describe(GeoPoint {
                lat: -33.9,
                lon: 151.2
            }),
            "33.9°S 151.2°E"
        );
        assert_eq!(describe(GeoPoint { lat: 0.0, lon: 0.0 }), "0.0°N 0.0°E");
    }
}
