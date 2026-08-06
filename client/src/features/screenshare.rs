//! Screen sharing via the LiveKit JS SDK running inside the webview.
//!
//! Native audio stays on the Rust LiveKit SDK; screen *video* runs here because
//! the webview can capture (`getDisplayMedia`) and render (`<video>`) it. The
//! JS client joins a SEPARATE room (`screen-…`) from voice so native-audio
//! peers never download the video. Who's sharing is tracked via our own
//! protocol (`ScreenShareState`), not the JS layer.
//!
//! Two on-screen surfaces, each a container the JS attaches a `<video>` into:
//! - `#screenshare-self`   — small draggable self-preview for the sharer.
//! - `#screenshare-viewer` — large draggable/resizable window for watchers.

use dioxus::prelude::*;

use crate::protocol::ClientMessage;
use crate::state::{use_app_state, use_gateway};

/// The JS controller. Idempotent so it's safe to prepend to every eval.
const SCREEN_JS: &str = r#"
window.dxScreen = window.dxScreen || (function () {
  let room = null;
  const tracks = {}; // identity -> video track
  const CONTAINERS = ['screenshare-self', 'screenshare-viewer'];
  const LK = () => window.LivekitClient || window.LiveKitClient;
  function attachInto(track, c) {
    c.innerHTML = '';
    const el = track.attach();
    el.muted = true; el.autoplay = true; el.playsInline = true;
    el.style.width = '100%'; el.style.height = '100%'; el.style.objectFit = 'contain'; el.style.background = '#000';
    c.appendChild(el);
  }
  // Re-render any container currently pointed at `identity`.
  function reattach(identity) {
    CONTAINERS.forEach(function (cid) {
      const c = document.getElementById(cid);
      if (c && c.getAttribute('data-identity') === identity) {
        const t = tracks[identity];
        if (t) attachInto(t, c); else c.querySelectorAll('video').forEach(function (e) { e.remove(); });
      }
    });
  }
  async function ensureLib() { for (let i = 0; i < 100 && !LK(); i++) { await new Promise(function (r) { setTimeout(r, 100); }); } return !!LK(); }
  async function connect(url, token) {
    if (room) return;
    if (!(await ensureLib())) { console.warn('[dxScreen] livekit lib not loaded'); return; }
    const lk = LK();
    room = new lk.Room({ adaptiveStream: true, dynacast: true });
    room.on(lk.RoomEvent.TrackSubscribed, function (track, pub, participant) {
      if (track.kind !== 'video') return;
      tracks[participant.identity] = track;
      reattach(participant.identity);
    });
    room.on(lk.RoomEvent.TrackUnsubscribed, function (track, pub, participant) {
      if (track.kind !== 'video') return;
      delete tracks[participant.identity];
      reattach(participant.identity);
    });
    room.on(lk.RoomEvent.LocalTrackPublished, function (pub) {
      if (!pub.track || pub.track.kind !== 'video') return;
      tracks[room.localParticipant.identity] = pub.track;
      reattach(room.localParticipant.identity);
    });
    room.on(lk.RoomEvent.LocalTrackUnpublished, function (pub) {
      if (!pub.track || pub.track.kind !== 'video') return;
      delete tracks[room.localParticipant.identity];
      reattach(room.localParticipant.identity);
    });
    try { await room.connect(url, token); } catch (e) { console.warn('[dxScreen] connect failed', e); room = null; }
  }
  function attach(identity, cid) {
    const c = document.getElementById(cid); if (!c) return;
    c.setAttribute('data-identity', identity);
    const t = tracks[identity];
    if (t) attachInto(t, c); else c.querySelectorAll('video').forEach(function (e) { e.remove(); });
  }
  function detach(cid) {
    const c = document.getElementById(cid); if (!c) return;
    c.removeAttribute('data-identity');
    c.querySelectorAll('video').forEach(function (e) { e.remove(); });
  }
  async function startShare() {
    if (!room) { console.warn('[dxScreen] not connected yet'); return; }
    try { await room.localParticipant.setScreenShareEnabled(true); } catch (e) { console.warn('[dxScreen] share failed', e); }
  }
  // Helper that runs getDisplayMedia inside the user's gesture before starting
  // the LiveKit screen-share flow. Browsers block getDisplayMedia unless it's
  // invoked directly by a user gesture; calling this from document.eval in
  // response to the native click ensures the prompt is allowed.
  async function requestAndStartShare() {
    if (window._dxf_share_starting) { console.warn('[dxScreen] already starting, ignoring duplicate call'); return; }
    window._dxf_share_starting = true;
    try {
      return await requestAndStartShareInner();
    } finally {
      window._dxf_share_starting = false;
    }
  }
  async function requestAndStartShareInner() {
    // If the browser doesn't expose navigator.mediaDevices, fall back to
    // directly calling startShare() and let the LiveKit SDK decide. This may
    // still be blocked if not run inside a user gesture, but it's a useful
    // fallback for embedded webviews without mediaDevices.
    if (!(navigator && navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia)) {
      console.warn('[dxScreen] navigator.mediaDevices.getDisplayMedia not available; falling back to startShare');
      // Wait a short moment for room to exist; otherwise call startShare
      // immediately (it may still prompt or fail depending on environment).
      for (let i = 0; i < 20; i++) { if (room) break; await new Promise(function (r) { setTimeout(r, 100); }); }
      try { await startShare(); } catch (e) { console.warn('[dxScreen] startShare fallback failed', e); }
      return;
    }

    let granted = false;
    try {
      // Prompt for screen capture permission in the user gesture.
      // Keep the stream open while we wait for the JS room to be ready so
      // LiveKit can reuse the granted permission. Don't stop the tracks here.
      const s = await navigator.mediaDevices.getDisplayMedia({ video: true });
      granted = !!s;
      // Keep a global marker showing user granted capture. Some browsers may
      // not surface the permission otherwise.
      window._dxf_display_permission_granted = true;
      try { s.getTracks().forEach(t => t.stop()); } catch (e) {}
    } catch (e) {
      console.warn('[dxScreen] getDisplayMedia denied or failed', e);
      return;
    }
    if (!granted) return;

    // Wait briefly for the room to connect (Server should have provided a
    // token and ScreenShareBridge will call dxScreen.connect). If the room
    // isn't ready after a short timeout, abort so we don't leave the UI stuck.
    for (let i = 0; i < 150; i++) {
      if (room) break;
      await new Promise(function (r) { setTimeout(r, 100); });
    }
    if (!room) { console.warn('[dxScreen] room not connected yet, cannot start share'); return; }

    // Now call the normal start path — permission is already granted so any
    // internal getDisplayMedia calls are allowed.
    await startShare();
  }
  async function stopShare() {
    if (!room) return;
    try { await room.localParticipant.setScreenShareEnabled(false); } catch (e) {}
  }
  async function disconnect() {
    if (room) { try { await room.disconnect(); } catch (e) {} room = null; }
    for (const k in tracks) delete tracks[k];
    CONTAINERS.forEach(detach);
  }
  return { connect: connect, attach: attach, detach: detach, startShare: startShare, requestAndStartShare: requestAndStartShare, stopShare: stopShare, disconnect: disconnect };
})();
"#;

/// Start/stop sharing — call from a click handler so `getDisplayMedia` runs in
/// a user gesture.
pub fn share_js(on: bool) -> String {
    // When starting, call the user-gesture variant that prompts getDisplayMedia
    // before delegating to the LiveKit flow. When stopping, just stopShare.
    let call = if on { "requestAndStartShare" } else { "stopShare" };
    format!("{SCREEN_JS}\nwindow.dxScreen.{call}();")
}

fn attach_js(identity: &str, container: &str) -> String {
    format!("{SCREEN_JS}\nwindow.dxScreen.attach('{identity}','{container}');")
}

fn detach_js(container: &str) -> String {
    format!("{SCREEN_JS}\nwindow.dxScreen.detach('{container}');")
}

/// Connects/disconnects the JS screen room as the token comes and goes.
#[component]
pub fn ScreenShareBridge() -> Element {
    let state = use_app_state();
    let token = use_memo(move || state.read().screen_token.clone());
    let mut last = use_signal(|| None::<(String, String)>);

    use_effect(move || {
        let t = token();
        if t != *last.peek() {
            match &t {
                Some((url, tok)) => {
                    let _ = document::eval(&format!(
                        "{SCREEN_JS}\nwindow.dxScreen.connect('{url}','{tok}');"
                    ));
                }
                None => {
                    let _ = document::eval(&format!("{SCREEN_JS}\nwindow.dxScreen.disconnect();"));
                }
            }
            last.set(t);
        }
    });

    // Detect whether the embedded webview exposes getDisplayMedia and record it.
    // Use an async eval so we can `recv::<bool>().await` the JS result.
    use_future(move || {
        let mut s = state.clone();
        async move {
            let mut eval = document::eval("(() => !!(navigator && navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia))()");
            if let Ok(v) = eval.recv::<bool>().await {
                if v {
                    s.write().screen_capture_available = true;
                } else {
                    s.write().screen_capture_available = false;
                    s.write().error_toast = Some("Screen capture is not available in this embedded webview. Install WebView2 or use a browser to share your screen.".into());
                }
            } else {
                s.write().screen_capture_available = false;
            }
        }
    });

    // If both a token and the user's local `screen_sharing` flag are set,
    // initiate the user-gesture sharing flow. This ensures the room is
    // connected (dxScreen.connect ran above) before prompting for capture.
    let mut last_start = use_signal(|| false);
    use_effect(move || {
        let t = token();
        let sharing = state.read().screen_sharing;
        if (t.is_some() && sharing) != *last_start.peek() {
            if t.is_some() && sharing {
                // Trigger the user-gesture request which runs getDisplayMedia
                // and then starts the LiveKit publish. The call is idempotent.
                let _ = document::eval(&format!("{SCREEN_JS}\nwindow.dxScreen.requestAndStartShare();"));
            }
            last_start.set(t.is_some() && sharing);
        }
    });

    rsx! { Fragment {} }
}

/// Small self-preview shown while you're sharing, with a Stop button. Draggable
/// (grab the header) and resizable (bottom-right grip) so it doesn't have to
/// pin the top-right corner — same interaction model as the watch window below.
#[component]
pub fn ScreenSelfPreview() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();

    // Position/size of the floating window. Defaults approximate the old
    // `top: 3.5rem; right: 0.75rem; width: 300px` anchor in a ~1280px viewport;
    // the user drags/resizes from there and the choice persists for the session.
    let mut px = use_signal(|| 968.0_f64);
    let mut py = use_signal(|| 56.0_f64);
    let mut pw = use_signal(|| 300.0_f64);
    let mut ph = use_signal(|| 208.0_f64);
    let mut drag = use_signal(|| None::<Drag>);

    let sharing = use_memo(move || state.read().screen_sharing);
    let self_pk = use_memo(move || state.read().self_user.as_ref().map(|u| u.pubkey.clone()));

    // Attach our local preview when sharing turns on.
    let mut last = use_signal(|| false);
    use_effect(move || {
        let sh = sharing();
        if sh != *last.peek() {
            if sh {
                if let Some(pk) = self_pk() {
                    let _ = document::eval(&attach_js(&pk, "screenshare-self"));
                }
            }
            last.set(sh);
        }
    });

    if !sharing() {
        return rsx! { Fragment {} };
    }

    rsx! {
        // Move/resize tracking overlay — only present while dragging, so it
        // captures the mouse even over the video. Same model as ScreenWatchWindow.
        if drag().is_some() {
            div {
                class: "fixed inset-0 z-50",
                onmousemove: move |e| {
                    let c = e.client_coordinates();
                    match drag() {
                        Some(Drag::Move { dx, dy }) => { px.set(c.x - dx); py.set(c.y - dy); }
                        Some(Drag::Resize { px: spx, py: spy, w0, h0 }) => {
                            pw.set((w0 + (c.x - spx)).max(280.0));
                            ph.set((h0 + (c.y - spy)).max(180.0));
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
            // Header doubles as the drag handle.
            div {
                class: "h-8 px-2.5 flex items-center gap-1.5 border-b border-[var(--border)] shrink-0 cursor-move select-none",
                onmousedown: move |e| {
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Move { dx: c.x - px(), dy: c.y - py() }));
                },
                span { class: "w-2 h-2 rounded-full shrink-0", style: "background: var(--danger);" }
                span { class: "text-[11px] text-[var(--text)] truncate flex-1", "Sharing your screen" }
                button {
                    class: "text-[9px] uppercase tracking-wider text-[var(--danger)] hover:text-[var(--accent-strong)] font-semibold",
                    onmousedown: move |e| e.stop_propagation(),
                    onclick: move |_| {
                        let cid = state.read().voice.channel_id;
                        state.write().screen_sharing = false;
                        let _ = document::eval(&share_js(false));
                        if let Some(c) = cid {
                            gateway.send(ClientMessage::SetScreenShare { channel_id: c, sharing: false });
                        }
                    },
                    "Stop"
                }
            }
            div {
                id: "screenshare-self",
                class: "flex-1 min-h-0 bg-black flex items-center justify-center text-[var(--text-dim)] text-[10px]",
                "Starting…"
            }
            // Resize grip (bottom-right).
            div {
                class: "absolute bottom-0 right-0 w-4 h-4 cursor-nwse-resize",
                style: "background: linear-gradient(135deg, transparent 0 50%, var(--border-strong) 50% 100%);",
                onmousedown: move |e| {
                    e.stop_propagation();
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Resize { px: c.x, py: c.y, w0: pw(), h0: ph() }));
                },
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Move { dx: f64, dy: f64 },
    Resize { px: f64, py: f64, w0: f64, h0: f64 },
}

/// Large, draggable + resizable window for watching someone else's screen.
#[component]
pub fn ScreenWatchWindow() -> Element {
    let mut state = use_app_state();
    let viewing = use_memo(move || state.read().screen_viewing.clone());

    // Attach/detach the chosen participant as the selection changes.
    let mut last = use_signal(|| None::<String>);
    use_effect(move || {
        let v = viewing();
        if v != *last.peek() {
            match &v {
                Some(pk) => {
                    let _ = document::eval(&attach_js(pk, "screenshare-viewer"));
                }
                None => {
                    let _ = document::eval(&detach_js("screenshare-viewer"));
                }
            }
            last.set(v);
        }
    });

    let mut x = use_signal(|| 160.0_f64);
    let mut y = use_signal(|| 90.0_f64);
    let mut w = use_signal(|| 880.0_f64);
    let mut h = use_signal(|| 540.0_f64);
    let mut drag = use_signal(|| None::<Drag>);

    let Some(pk) = viewing() else {
        return rsx! { Fragment {} };
    };
    let name = state
        .read()
        .user_of(&pk)
        .map(|u| u.username.clone())
        .unwrap_or_else(|| crate::identity::truncate_pubkey(&pk));

    rsx! {
        // Move/resize tracking overlay — only present while dragging, so it
        // captures the mouse even over the video.
        if drag().is_some() {
            div {
                class: "fixed inset-0 z-50",
                onmousemove: move |e| {
                    let c = e.client_coordinates();
                    match drag() {
                        Some(Drag::Move { dx, dy }) => { x.set(c.x - dx); y.set(c.y - dy); }
                        Some(Drag::Resize { px, py, w0, h0 }) => {
                            w.set((w0 + (c.x - px)).max(320.0));
                            h.set((h0 + (c.y - py)).max(200.0));
                        }
                        None => {}
                    }
                },
                onmouseup: move |_| drag.set(None),
            }
        }
        div {
            class: "fixed z-40 flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-2xl overflow-hidden dxf-modal-in",
            style: "left: {x}px; top: {y}px; width: {w}px; height: {h}px;",
            // Header doubles as the drag handle.
            div {
                class: "h-9 px-3 flex items-center gap-2 border-b border-[var(--border)] shrink-0 cursor-move select-none",
                onmousedown: move |e| {
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Move { dx: c.x - x(), dy: c.y - y() }));
                },
                span { class: "w-2.5 h-2.5 rounded-full shrink-0", style: "background: var(--danger);" }
                span { class: "text-sm text-[var(--text)] font-medium truncate", "{name}'s screen" }
                span { class: "text-[10px] uppercase tracking-wider text-[var(--danger)] font-semibold", "Live" }
                div { class: "flex-1" }
                button {
                    class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                    onmousedown: move |e| e.stop_propagation(),
                    onclick: move |_| state.write().screen_viewing = None,
                    "✕"
                }
            }
            div {
                id: "screenshare-viewer",
                class: "flex-1 min-h-0 bg-black flex items-center justify-center text-[var(--text-dim)] text-sm",
                "Connecting to stream…"
            }
            // Resize grip (bottom-right).
            div {
                class: "absolute bottom-0 right-0 w-4 h-4 cursor-nwse-resize",
                style: "background: linear-gradient(135deg, transparent 0 50%, var(--border-strong) 50% 100%);",
                onmousedown: move |e| {
                    e.stop_propagation();
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Resize { px: c.x, py: c.y, w0: w(), h0: h() }));
                },
            }
        }
    }
}
