//! Webcam sharing, captured in the webview on every platform.
//!
//! The camera is the one video path that is *not* split per-platform. Screen
//! capture is native on macOS and webview on Windows (see `sysvideo` and
//! `features::screenshare`); a camera is `getUserMedia` everywhere, which is
//! also why the local preview costs nothing — it is the capture stream in a
//! `<video>`, never a round trip through the SFU.
//!
//! **It publishes on the screen room's existing connection.** The webview is
//! already in `screen-{channel}` under the bare pubkey with publish rights, and
//! LiveKit tells a webcam from a screen by `TrackSource`, not by identity — so
//! the camera needed no fourth identity, no extra token, and no change to token
//! minting at all. The cost of that choice is that every video track in the JS
//! controller has to be keyed by identity *and* source; see `SCREEN_JS`.
//!
//! Who has a camera on travels over **our** protocol, not over LiveKit's track
//! events: `camera_on` on `VoiceState`. That is what makes it survive a
//! reconnect (`Ready` carries the voice roster) and reach people who are not in
//! the channel to see the publication for themselves.
//!
//! Surfaces, each a container the JS attaches a `<video>` into:
//! - `#camera-self`      — your own preview, from the local stream.
//! - `#camera-{pubkey}`  — one tile per remote publisher, in the grid window.

use dioxus::prelude::*;
use serde_json::Value;

use crate::features::screenshare::{Drag, SCREEN_JS, attach_js, detach_js};
use crate::protocol::ClientMessage;
use crate::state::{CameraDevice, use_app_state, use_gateway};

/// 720p30 at 1.2 Mbit. Not a preset table like the screen share's: a face is
/// forgiving about resolution in a way a spreadsheet is not, and the tile it
/// lands in is usually small. Simulcast (set in the JS) is what actually keeps
/// a grid affordable.
const CAM_W: u32 = 1280;
const CAM_H: u32 = 720;
const CAM_FPS: u32 = 30;
const CAM_BITRATE: u32 = 1_200_000;

fn start_camera_js(device_id: Option<&str>, w: u32, h: u32, fps: u32, bitrate: u32) -> String {
    // A deviceId is an opaque, origin-salted string, not hex — quote it through
    // serde rather than interpolating it into a JS literal.
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

/// Which camera to open: the remembered id if it is still present, else the one
/// whose *label* matches, else the system default.
///
/// The label fallback is not belt-and-braces. deviceIds are origin-salted and
/// can rotate between sessions, so a remembered id can go stale while the
/// camera is still plugged in — matching the label recovers the user's choice
/// instead of silently reverting them to the built-in webcam.
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

/// Turn the camera on or off.
///
/// **Must be called from a user event, never from an effect.** `getUserMedia`
/// does not strictly require transient activation the way `getDisplayMedia`
/// does, but WebKit has historically wanted one to *prompt*, and this codebase
/// has already paid for deferring a capture call past its gesture once (see the
/// note on the direct-publish path in `features::screenshare`). The click's
/// activation is still valid when the eval lands — the IPC round trip is
/// milliseconds against a multi-second window.
///
/// The corollary is the load-bearing half: the automatic republish after a
/// reconnect must never call a capture API, which is why the JS holds the track
/// across rooms rather than re-acquiring it.
pub fn toggle_camera(
    mut state: Signal<crate::state::AppState>,
    settings: Signal<crate::settings::ClientSettings>,
    on: bool,
) {
    if !on {
        // Stopping is local and immediate — there is nothing to fail, so unlike
        // starting, the UI can be told now. Same shape as the share button.
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

/// Owns the camera's side channel: the JS message pump, the capability probe,
/// and the teardown that follows the screen room.
///
/// Renders nothing. Mounted once at the workspace root, like `ScreenShareBridge`
/// — which it deliberately does not extend: the two pumps wire independent
/// listeners so a change to one cannot silently swallow the other's messages.
#[component]
pub fn CameraBridge() -> Element {
    let mut state = use_app_state();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let gateway = use_gateway();

    // Can this webview open a camera at all? Deliberately *not* short-circuited
    // on `sysvideo::supported()` the way the screen probe is — there is no
    // native camera path to fall back to on any platform.
    use_future(move || {
        let mut s = state;
        async move {
            let mut eval = document::eval(
                "dioxus.send(!!(navigator && navigator.mediaDevices && navigator.mediaDevices.getUserMedia));",
            );
            match eval.recv::<bool>().await {
                Ok(v) => s.write().camera_capture_available = v,
                // Fail open, for the same reason the screen probe does: a false
                // negative disables a working feature and tells the user
                // something untrue, while a false positive costs one failed
                // attempt that reports its own reason.
                Err(e) => {
                    eprintln!("[camera] capture probe failed, assuming available: {e:?}");
                    s.write().camera_capture_available = true;
                }
            }
        }
    });

    // Populate the device list once, so the picker is not empty the first time
    // it is opened. Labels will be blank until a grant exists; that is handled
    // where they are rendered.
    use_hook(|| {
        let _ = document::eval(&list_cameras_js());
    });

    // The screen room going away takes the camera with it — the JS `disconnect`
    // already released the device, this is the Rust half. Covers leaving voice,
    // being kicked, and the token being cleared for any other reason.
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
            // Its own listener and its own guard flag: sharing the share pump's
            // would mean one `if` deciding which messages either feature sees.
            // The sink is reassigned on *every* eval; the listener is registered
            // once. That split is the whole point, and it is the shape
            // `features::chat`'s drop handler already uses.
            //
            // A listener holds the `dioxus.send` of the eval that created it,
            // and this component is remounted whenever the session is — a
            // disconnect sets `session` to None and the workspace is rebuilt.
            // The webview is never reloaded, so a guard on registration alone
            // meant the second mount registered nothing and the first mount's
            // send was already dead: every camera message from then on
            // disappeared into the `catch`, for the life of the process. The
            // symptom was a camera that started, previewed, published, and was
            // never announced to anyone — "Starting your camera…" forever.
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
                    // Publishing succeeded — only now is the camera on.
                    Some("camera-started") => {
                        {
                            let mut w = state.write();
                            w.camera_on = true;
                            w.camera_starting = false;
                        }
                        // Persist what actually opened, not what was asked for,
                        // so a fallback to another camera is not remembered as
                        // the user's choice.
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
                    // The track ended: our own Stop, an unplug, or another app
                    // taking the device. All the same to the UI.
                    Some("camera-ended") => {
                        let was = state.read().camera_on;
                        {
                            let mut w = state.write();
                            w.camera_on = false;
                            w.camera_starting = false;
                        }
                        // Only announce a transition we had announced the start
                        // of, or a failed start would tell the channel a camera
                        // it never saw has stopped.
                        if was {
                            gateway.send(ClientMessage::SetCamera { on: false });
                        }
                    }
                    // The user said no, or dismissed the OS prompt. Retrying
                    // cannot help with a decision, so this explains where to
                    // change it instead of offering another go.
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

/// Small floating preview of your own camera.
///
/// Shows the *local capture stream*, not a subscription to our own publication:
/// it has to be up while the room is still connecting, and has to keep working
/// if publishing fails outright. That is the opposite of `ScreenSelfPreview`,
/// which subscribes on macOS precisely to prove the frames crossed the wire.
#[component]
pub fn CameraSelfPreview() -> Element {
    let state = use_app_state();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();

    let mut px = use_signal(|| 968.0_f64);
    let mut py = use_signal(|| 280.0_f64);
    let mut pw = use_signal(|| 240.0_f64);
    let mut ph = use_signal(|| 176.0_f64);
    let mut drag = use_signal(|| None::<Drag>);

    let on = use_memo(move || state.read().camera_on);
    let starting = use_memo(move || state.read().camera_starting);

    // Belt-and-braces with the attach the JS does as soon as the stream opens:
    // this covers the window being (re)mounted after the capture already began.
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
                        Some(Drag::Resize { px: spx, py: spy, w0, h0 }) => {
                            pw.set((w0 + (c.x - spx)).max(180.0));
                            ph.set((h0 + (c.y - spy)).max(130.0));
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
                    // mousedown, not click, and before the header's drag handler
                    // — same wry/WebView2 reason as the share preview's Stop.
                    onmousedown: move |e| {
                        e.stop_propagation();
                        toggle_camera(state, settings, false);
                    },
                    "Stop"
                }
            }
            // Empty on purpose: the JS owns every child of an attach container
            // (`innerHTML = ''`), so anything Dioxus renders inside would be
            // torn out from under the VDOM.
            div { id: "camera-self", class: "flex-1 min-h-0 bg-black" }
        }
    }
}

/// One remote camera. The container is empty; siblings carry the chrome.
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
            class: "relative rounded overflow-hidden bg-black",
            // Inline rather than a Tailwind aspect utility: none is in the
            // committed `tailwind.out.css`, and naming one even in a comment
            // puts it there — the v4 scanner reads this file as plain text, so
            // a class mentioned here is a class generated.
            style: "aspect-ratio: 16 / 9;",
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

/// Floating grid of the cameras you have chosen to watch.
///
/// One window with a grid rather than one window per person: N windows means N
/// drag positions to keep track of, and a screen that fills up on the fourth
/// participant.
///
/// It shows only who is in `cameras_watching` — nobody appears uninvited. That
/// is a bandwidth control as much as a courtesy one: a tile that is not mounted
/// has no attached element, and `adaptiveStream` then tells the SFU not to send
/// that video at all.
#[component]
pub fn CameraGridWindow() -> Element {
    let mut state = use_app_state();

    let mut px = use_signal(|| 340.0_f64);
    let mut py = use_signal(|| 120.0_f64);
    let mut pw = use_signal(|| 560.0_f64);
    let mut ph = use_signal(|| 360.0_f64);
    let mut drag = use_signal(|| None::<Drag>);

    // Intersected with who is *actually* publishing, not taken from the watch
    // set alone: someone can turn their camera off while you are watching, and a
    // tile pointed at a track that no longer exists sits on "Connecting…"
    // forever. Never ourselves — our own picture is the self-preview, and
    // subscribing to it would be a pointless round trip through the SFU.
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
            // z-index inline: between the self-preview (30) and the screen watch
            // window (40). No bracketed z utility for that value is in the
            // committed Tailwind output, and mentioning one here would generate
            // it — see the note on the tile's aspect ratio.
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
                        // Clears the whole watch set rather than hiding a window
                        // that still holds subscriptions: the roster icons are
                        // the switches, so the ✕ has to turn them all off or it
                        // would lie about what is still being downloaded.
                        state.write().cameras_watching.clear();
                    },
                    "✕"
                }
            }
            div {
                class: "flex-1 min-h-0 overflow-y-auto p-1.5",
                // auto-fill/minmax rather than a fixed `grid-cols-N` class: the
                // window is resizable, so the column count has to follow its
                // width — and it keeps a new Tailwind class out of the build.
                style: "display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 6px; align-content: start;",
                for pk in others() {
                    CameraTile { key: "{pk}", pubkey: pk.clone() }
                }
            }
            // Resize grip, same affordance as the screen watch window.
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
