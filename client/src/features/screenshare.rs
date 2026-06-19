//! Screen sharing via the LiveKit JS SDK running inside the webview.
//!
//! Native audio stays on the Rust LiveKit SDK; screen *video* is handled here
//! because the webview can capture (`getDisplayMedia`) and render (`<video>`)
//! it natively. The JS client joins a SEPARATE room (`screen-…`) from voice so
//! native-audio peers never download the video.
//!
//! Who is sharing is tracked via our OWN protocol (`ScreenShareState`), not the
//! JS layer — reliable and testable. The JS only: connects, registers incoming
//! video tracks by participant identity (= pubkey), and attaches a chosen one
//! into the big `#screenshare-viewer` on demand.

use dioxus::prelude::*;

use crate::state::use_app_state;

/// The JS controller. Idempotent so it's safe to prepend to every eval.
const SCREEN_JS: &str = r#"
window.dxScreen = window.dxScreen || (function () {
  let room = null;
  const tracks = {}; // identity -> video track
  const LK = () => window.LivekitClient || window.LiveKitClient;
  function viewer() { return document.getElementById('screenshare-viewer'); }
  function attachInto(track, c) {
    c.innerHTML = '';
    const el = track.attach();
    el.muted = true; el.autoplay = true; el.playsInline = true;
    el.style.width = '100%'; el.style.height = '100%'; el.style.objectFit = 'contain'; el.style.background = '#000';
    c.appendChild(el);
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
      const v = viewer();
      if (v && v.getAttribute('data-identity') === participant.identity) attachInto(track, v);
    });
    room.on(lk.RoomEvent.TrackUnsubscribed, function (track, pub, participant) {
      if (track.kind !== 'video') return;
      delete tracks[participant.identity];
      const v = viewer();
      if (v && v.getAttribute('data-identity') === participant.identity) v.querySelectorAll('video').forEach(function (e) { e.remove(); });
    });
    try { await room.connect(url, token); } catch (e) { console.warn('[dxScreen] connect failed', e); room = null; }
  }
  function attach(identity) {
    const v = viewer(); if (!v) return;
    v.setAttribute('data-identity', identity);
    const t = tracks[identity];
    if (t) attachInto(t, v);
    else v.querySelectorAll('video').forEach(function (e) { e.remove(); });
  }
  function detach() {
    const v = viewer(); if (!v) return;
    v.removeAttribute('data-identity');
    v.querySelectorAll('video').forEach(function (e) { e.remove(); });
  }
  async function startShare() {
    if (!room) { console.warn('[dxScreen] not connected yet'); return; }
    try { await room.localParticipant.setScreenShareEnabled(true); } catch (e) { console.warn('[dxScreen] share failed', e); }
  }
  async function stopShare() {
    if (!room) return;
    try { await room.localParticipant.setScreenShareEnabled(false); } catch (e) {}
  }
  async function disconnect() {
    if (room) { try { await room.disconnect(); } catch (e) {} room = null; }
    for (const k in tracks) delete tracks[k];
    detach();
  }
  return { connect: connect, attach: attach, detach: detach, startShare: startShare, stopShare: stopShare, disconnect: disconnect };
})();
"#;

/// Start/stop sharing — call from a click handler so `getDisplayMedia` runs in
/// a user gesture.
pub fn share_js(on: bool) -> String {
    let call = if on { "startShare" } else { "stopShare" };
    format!("{SCREEN_JS}\nwindow.dxScreen.{call}();")
}

/// Connects/disconnects the JS screen room as the token comes and goes. Renders
/// nothing itself; the viewer dialog is separate.
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

    rsx! { Fragment {} }
}

/// Large dialog that plays the screen of `state.screen_viewing` (a pubkey).
#[component]
pub fn ScreenViewer() -> Element {
    let mut state = use_app_state();
    let viewing = use_memo(move || state.read().screen_viewing.clone());
    let mut last = use_signal(|| None::<String>);

    // Attach/detach the chosen participant's track as the selection changes.
    use_effect(move || {
        let v = viewing();
        if v != *last.peek() {
            match &v {
                Some(pk) => {
                    let _ = document::eval(&format!(
                        "{SCREEN_JS}\nwindow.dxScreen.attach('{pk}');"
                    ));
                }
                None => {
                    let _ = document::eval(&format!("{SCREEN_JS}\nwindow.dxScreen.detach();"));
                }
            }
            last.set(v);
        }
    });

    let Some(pk) = viewing() else {
        return rsx! { Fragment {} };
    };
    let name = state
        .read()
        .user_of(&pk)
        .map(|u| u.username.clone())
        .unwrap_or_else(|| crate::identity::truncate_pubkey(&pk));

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/70",
            onclick: move |_| state.write().screen_viewing = None,
            div {
                class: "dxf-modal-in flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-2xl overflow-hidden",
                style: "width: 86vw; height: 84vh;",
                onclick: move |e| e.stop_propagation(),
                div { class: "h-10 px-4 flex items-center gap-2 border-b border-[var(--border)] shrink-0",
                    span {
                        class: "w-2.5 h-2.5 rounded-full dxf-dot-pulse",
                        style: "background: var(--danger); color: var(--danger);",
                    }
                    span { class: "text-sm text-[var(--text)] font-medium", "{name}'s screen" }
                    span { class: "text-[10px] uppercase tracking-wider text-[var(--danger)] font-semibold", "Live" }
                    div { class: "flex-1" }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        onclick: move |_| state.write().screen_viewing = None,
                        "✕"
                    }
                }
                // JS attaches the <video> here; falls back to a hint when empty.
                div {
                    id: "screenshare-viewer",
                    class: "flex-1 min-h-0 bg-black flex items-center justify-center text-[var(--text-dim)] text-sm",
                    "Connecting to stream…"
                }
            }
        }
    }
}
