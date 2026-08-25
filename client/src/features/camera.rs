use dioxus::prelude::*;
use serde_json::Value;

use crate::features::screenshare::{Drag, SCREEN_JS, attach_js, detach_js};
use crate::protocol::ClientMessage;
use crate::state::{CameraDevice, use_app_state, use_gateway};

const CAM_W: u32 = 1280;
const CAM_H: u32 = 720;
const CAM_FPS: u32 = 30;
const CAM_BITRATE: u32 = 1_200_000;

fn start_camera_js(device_id: Option<&str>, w: u32, h: u32, fps: u32, bitrate: u32) -> String {
    let dev = serde_json::to_string(&device_id).unwrap_or_else(|_| "null".into());
    format!(
        "{SCREEN_JS}\nwindow.dxScreen.startCamera({{deviceId:{dev},width:{w},height:{h},\
         fps:{fps},bitrate:{bitrate}}});"
    )
}

fn stop_camera_js() -> String {
    format!("{SCREEN_JS}\nwindow.dxScreen.stopCamera();")
}

pub fn list_cameras_js() -> String {
    format!("{SCREEN_JS}\nwindow.dxScreen.listCameras();")
}

fn attach_local_camera_js(container: &str) -> String {
    let container = crate::features::screenshare::js_str(container);
    format!("{SCREEN_JS}\nwindow.dxScreen.attachLocalCamera({container});")
}

/// The label fallback is not belt-and-braces: a deviceId is origin-salted and
/// can change between launches, so the label is often the only match left.
fn resolve_device(
    available: &[CameraDevice],
    saved_id: Option<&str>,
    saved_label: Option<&str>,
) -> Option<String> {
    if let Some(id) = saved_id
        && available.iter().any(|d| d.id == id)
    {
        return Some(id.to_string());
    }
    if let Some(label) = saved_label.filter(|l| !l.is_empty()) {
        return available
            .iter()
            .find(|d| d.label == label)
            .map(|d| d.id.clone());
    }
    None
}

pub fn toggle_camera(
    mut state: Signal<crate::state::AppState>,
    settings: Signal<crate::settings::ClientSettings>,
    on: bool,
) {
    if !on {
        state.write().camera_on = false;
        state.write().camera_starting = false;
        let _ = document::eval(&stop_camera_js());
        return;
    }
    let device = {
        let s = state.read();
        let cfg = settings.read();
        resolve_device(
            &s.available_cameras,
            cfg.camera_device_id.as_deref(),
            cfg.camera_device_label.as_deref(),
        )
    };
    state.write().camera_starting = true;
    let _ = document::eval(&start_camera_js(
        device.as_deref(),
        CAM_W,
        CAM_H,
        CAM_FPS,
        CAM_BITRATE,
    ));
}

#[component]
pub fn CameraBridge() -> Element {
    let mut state = use_app_state();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let gateway = use_gateway();

    use_future(move || {
        let mut s = state;
        async move {
            let mut eval = document::eval(
                "dioxus.send(!!(navigator && navigator.mediaDevices && navigator.mediaDevices.getUserMedia));",
            );
            match eval.recv::<bool>().await {
                Ok(v) => s.write().camera_capture_available = v,
                Err(e) => {
                    eprintln!("[camera] capture probe failed, assuming available: {e:?}");
                    s.write().camera_capture_available = true;
                }
            }
        }
    });

    use_hook(|| {
        let _ = document::eval(&list_cameras_js());
    });

    let mut had_token = use_signal(|| false);
    use_effect(move || {
        let has = state.read().screen_token.is_some();
        if has != *had_token.peek() {
            if !has {
                let mut w = state.write();
                w.camera_on = false;
                w.camera_starting = false;
                w.cameras_watching.clear();
            }
            had_token.set(has);
        }
    });

    let gateway_pump = gateway.clone();
    use_future(move || {
        let mut state = state;
        let gateway = gateway_pump.clone();
        async move {
            let bridge_js = r#"
            window.__dxfCameraSink = function (m) { try { dioxus.send(m); } catch (err) {} };
            if (!window.__dxfCameraWired) {
              window.__dxfCameraWired = true;
              var KINDS = ['camera-started','camera-ended','camera-denied','camera-error','camera-unavailable','camera-devices'];
              window.addEventListener('message', function (e) {
                var d = e.data;
                if (d && KINDS.indexOf(d.__dxf) !== -1 && window.__dxfCameraSink) {
                  window.__dxfCameraSink(d);
                }
              });
            }
            "#;
            let mut eval = document::eval(bridge_js);
            while let Ok(msg) = eval.recv::<Value>().await {
                match msg.get("__dxf").and_then(|v| v.as_str()) {
                    Some("camera-started") => {
                        {
                            let mut w = state.write();
                            w.camera_on = true;
                            w.camera_starting = false;
                        }
                        // Persist what actually opened, not what was asked
                        // for, so a fallback is not saved as the choice.
                        let id = msg.get("deviceId").and_then(|v| v.as_str()).unwrap_or("");
                        let label = msg.get("label").and_then(|v| v.as_str()).unwrap_or("");
                        if !id.is_empty() {
                            let mut next = settings.read().clone();
                            next.camera_device_id = Some(id.to_string());
                            next.camera_device_label =
                                (!label.is_empty()).then(|| label.to_string());
                            settings.set(next.clone());
                            crate::settings::save(&next);
                        }
                        gateway.send(ClientMessage::SetCamera { on: true });
                    }
                    Some("camera-ended") => {
                        let was = state.read().camera_on;
                        {
                            let mut w = state.write();
                            w.camera_on = false;
                            w.camera_starting = false;
                        }
                        if was {
                            gateway.send(ClientMessage::SetCamera { on: false });
                        }
                    }
                    Some("camera-denied") => {
                        let mut w = state.write();
                        w.camera_on = false;
                        w.camera_starting = false;
                        w.error_toast = Some(
                            "Camera access was refused. On macOS, allow it under System \
                             Settings › Privacy & Security › Camera, then try again."
                                .into(),
                        );
                    }
                    Some("camera-error") => {
                        let detail = msg
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown error");
                        eprintln!("[camera] start failed: {detail}");
                        let mut w = state.write();
                        w.camera_on = false;
                        w.camera_starting = false;
                        w.error_toast = Some(format!("Couldn't start your camera: {detail}"));
                    }
                    Some("camera-unavailable") => {
                        let mut w = state.write();
                        w.camera_capture_available = false;
                        w.camera_on = false;
                        w.camera_starting = false;
                        w.error_toast = Some("This build's webview has no camera support.".into());
                    }
                    Some("camera-devices") => {
                        let devices: Vec<CameraDevice> = msg
                            .get("devices")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|d| {
                                        Some(CameraDevice {
                                            id: d.get("id")?.as_str()?.to_string(),
                                            label: d
                                                .get("label")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string(),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        state.write().available_cameras = devices;
                    }
                    _ => {}
                }
            }
        }
    });

    rsx! { Fragment {} }
}

const SELF_CHROME: f64 = 32.0;

#[component]
pub fn CameraSelfPreview() -> Element {
    let state = use_app_state();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();

    let mut px = use_signal(|| 968.0_f64);
    let mut py = use_signal(|| 280.0_f64);
    let mut pw = use_signal(|| 360.0_f64);
    let mut ph = use_signal(|| 360.0 * 9.0 / 16.0 + SELF_CHROME);
    let mut drag = use_signal(|| None::<Drag>);

    let on = use_memo(move || state.read().camera_on);
    let starting = use_memo(move || state.read().camera_starting);

    let mut last = use_signal(|| false);
    use_effect(move || {
        let v = on();
        if v != *last.peek() {
            if v {
                let _ = document::eval(&attach_local_camera_js("camera-self"));
            }
            last.set(v);
        }
    });

    if !on() && !starting() {
        return rsx! { Fragment {} };
    }

    rsx! {
        if drag().is_some() {
            div {
                class: "fixed inset-0 z-50",
                onmousemove: move |e| {
                    let c = e.client_coordinates();
                    match drag() {
                        Some(Drag::Move { dx, dy }) => { px.set(c.x - dx); py.set(c.y - dy); }
                        Some(Drag::Resize { px: spx, w0, .. }) => {
                            let w = (w0 + (c.x - spx)).max(240.0);
                            pw.set(w);
                            ph.set(w * 9.0 / 16.0 + SELF_CHROME);
                        }
                        None => {}
                    }
                },
                onmouseup: move |_| drag.set(None),
            }
        }
        div {
            class: "fixed z-30 flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden dxf-pop-in",
            style: "left: {px}px; top: {py}px; width: {pw}px; height: {ph}px;",
            div {
                class: "h-8 px-2.5 flex items-center gap-1.5 border-b border-[var(--border)] shrink-0 cursor-move select-none",
                onmousedown: move |e| {
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Move { dx: c.x - px(), dy: c.y - py() }));
                },
                span { class: "w-2 h-2 rounded-full shrink-0", style: "background: var(--accent);" }
                span {
                    class: "text-[11px] text-[var(--text)] truncate flex-1",
                    if starting() && !on() { "Starting your camera…" } else { "Your camera" }
                }
                button {
                    class: "text-[9px] uppercase tracking-wider text-[var(--danger)] hover:text-[var(--accent-strong)] font-semibold",
                    onmousedown: move |e| {
                        e.stop_propagation();
                        toggle_camera(state, settings, false);
                    },
                    "Stop"
                }
            }
            div { id: "camera-self", class: "flex-1 min-h-0 bg-black" }
            div {
                class: "absolute right-0 bottom-0 w-3 h-3 cursor-nwse-resize",
                onmousedown: move |e| {
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Resize { px: c.x, py: c.y, w0: pw(), h0: ph() }));
                },
            }
        }
    }
}

#[component]
fn CameraTile(pubkey: String) -> Element {
    let state: Signal<crate::state::AppState> = use_app_state();
    let cid = format!("camera-{pubkey}");

    let name = state.read().display_name(&pubkey);

    let attach_pk = pubkey.clone();
    let attach_cid = cid.clone();
    use_effect(move || {
        let _ = document::eval(&attach_js(&attach_pk, &attach_cid, "camera"));
    });

    let drop_cid = cid.clone();
    use_drop(move || {
        let _ = document::eval(&detach_js(&drop_cid));
    });

    rsx! {
        div {
            class: "relative rounded overflow-hidden bg-black w-full h-full min-h-0",
            div {
                class: "absolute inset-0 flex items-center justify-center text-[10px] text-[var(--text-dim)]",
                "Connecting…"
            }
            div { id: "{cid}", class: "absolute inset-0" }
            div {
                class: "absolute left-1 bottom-1 px-1 py-0.5 rounded bg-black/70 text-[9px] pointer-events-none",
                "{name}"
            }
        }
    }
}

#[component]
pub fn CameraGridWindow() -> Element {
    let mut state = use_app_state();

    let mut px = use_signal(|| 340.0_f64);
    let mut py = use_signal(|| 120.0_f64);
    let mut pw = use_signal(|| 760.0_f64);
    let mut ph = use_signal(|| 500.0_f64);
    let mut drag = use_signal(|| None::<Drag>);

    let others = use_memo(move || {
        let s = state.read();
        let Some(cid) = s.voice.channel_id else {
            return Vec::new();
        };
        let me = s.self_user.as_ref().map(|u| u.pubkey.clone());
        s.cameras_in(cid)
            .into_iter()
            .filter(|pk| Some(pk) != me.as_ref() && s.cameras_watching.contains(pk))
            .collect::<Vec<_>>()
    });

    let cols = use_memo(move || (others().len() as f64).sqrt().ceil().max(1.0) as usize);
    let rows = use_memo(move || others().len().div_ceil(cols()).max(1));

    if others().is_empty() {
        return rsx! { Fragment {} };
    }

    rsx! {
        if drag().is_some() {
            div {
                class: "fixed inset-0 z-50",
                onmousemove: move |e| {
                    let c = e.client_coordinates();
                    match drag() {
                        Some(Drag::Move { dx, dy }) => { px.set(c.x - dx); py.set(c.y - dy); }
                        Some(Drag::Resize { px: spx, py: spy, w0, h0 }) => {
                            pw.set((w0 + (c.x - spx)).max(300.0));
                            ph.set((h0 + (c.y - spy)).max(220.0));
                        }
                        None => {}
                    }
                },
                onmouseup: move |_| drag.set(None),
            }
        }
        div {
            class: "fixed flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden dxf-pop-in",
            style: "left: {px}px; top: {py}px; width: {pw}px; height: {ph}px; z-index: 35;",
            div {
                class: "h-8 px-2.5 flex items-center gap-1.5 border-b border-[var(--border)] shrink-0 cursor-move select-none",
                onmousedown: move |e| {
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Move { dx: c.x - px(), dy: c.y - py() }));
                },
                span { class: "text-[11px] text-[var(--text)] truncate flex-1", "Cameras · {others().len()}" }
                button {
                    class: "text-[11px] text-[var(--text-muted)] hover:text-[var(--accent)]",
                    title: "Stop watching every camera",
                    onmousedown: move |e| {
                        e.stop_propagation();
                        state.write().cameras_watching.clear();
                    },
                    "✕"
                }
            }
            div {
                class: "flex-1 min-h-0 overflow-hidden p-1.5",
                style: "display: grid; grid-template-columns: repeat({cols()}, minmax(0, 1fr)); grid-template-rows: repeat({rows()}, minmax(0, 1fr)); gap: 6px;",
                for pk in others() {
                    CameraTile { key: "{pk}", pubkey: pk.clone() }
                }
            }
            div {
                class: "absolute right-0 bottom-0 w-3 h-3 cursor-nwse-resize",
                onmousedown: move |e| {
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Resize { px: c.x, py: c.y, w0: pw(), h0: ph() }));
                },
            }
        }
    }
}
