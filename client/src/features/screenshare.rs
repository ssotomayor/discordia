//! Screen sharing via the LiveKit JS SDK running inside the webview.
//!
//! Native audio stays on the Rust LiveKit SDK; screen *video* is handled here
//! because the webview can capture (`getDisplayMedia`) and render (`<video>`)
//! it natively, which the Rust-SDK + webview-UI split can't do cleanly. The JS
//! client joins a SEPARATE room (`screen-…`) from voice so native-audio peers
//! never download the video.
//!
//! Rust drives a small JS controller (`window.dxScreen`) via `document::eval`:
//! - `connect(url, token)` — join the screen room, render remote video into
//!   `#screenshare-stage`.
//! - `startShare()` / `stopShare()` — publish/unpublish the local screen.
//!   `startShare` MUST be called from a user-gesture handler (getDisplayMedia).
//! - `disconnect()` — leave + clear the stage.

use dioxus::prelude::*;

use crate::state::use_app_state;

/// The JS controller. Idempotent (`window.dxScreen ||= …`) so it's safe to
/// prepend to every eval. Waits for the UMD lib (loaded via a CDN <script>).
const SCREEN_JS: &str = r#"
window.dxScreen = window.dxScreen || (function () {
  let room = null;
  const LK = () => window.LivekitClient || window.LiveKitClient;
  function stage() { return document.getElementById('screenshare-stage'); }
  function refresh() { const s = stage(); if (s) s.style.display = s.querySelector('video') ? 'flex' : 'none'; }
  async function ensureLib() { for (let i = 0; i < 100 && !LK(); i++) { await new Promise(r => setTimeout(r, 100)); } return !!LK(); }
  async function connect(url, token) {
    if (room) return;
    if (!(await ensureLib())) { console.warn('[dxScreen] livekit lib not loaded'); return; }
    const lk = LK();
    room = new lk.Room({ adaptiveStream: true, dynacast: true });
    room.on(lk.RoomEvent.TrackSubscribed, function (track, pub) {
      if (track.kind !== 'video') return;
      const s = stage(); if (!s) return;
      const el = track.attach();
      el.setAttribute('data-sid', pub.trackSid);
      el.muted = true; el.autoplay = true; el.playsInline = true;
      el.style.maxWidth = '100%'; el.style.maxHeight = '48vh';
      el.style.borderRadius = '8px'; el.style.background = '#000';
      s.appendChild(el);
      refresh();
    });
    room.on(lk.RoomEvent.TrackUnsubscribed, function (track) {
      if (track.kind === 'video') { track.detach().forEach(function (e) { e.remove(); }); refresh(); }
    });
    room.on(lk.RoomEvent.Disconnected, function () {
      const s = stage(); if (s) { s.querySelectorAll('video').forEach(function (v) { v.remove(); }); refresh(); }
    });
    try { await room.connect(url, token); } catch (e) { console.warn('[dxScreen] connect failed', e); room = null; }
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
    const s = stage(); if (s) { s.querySelectorAll('video').forEach(function (v) { v.remove(); }); refresh(); }
  }
  return { connect: connect, startShare: startShare, stopShare: stopShare, disconnect: disconnect };
})();
"#;

/// JS to start/stop sharing — call from a click handler so `getDisplayMedia`
/// runs inside a user gesture.
pub fn share_js(on: bool) -> String {
    if on {
        format!("{SCREEN_JS}\nwindow.dxScreen.startShare();")
    } else {
        format!("{SCREEN_JS}\nwindow.dxScreen.stopShare();")
    }
}

/// Mounted for the lifetime of the workspace: connects/disconnects the JS
/// screen room as the screen token comes and goes, and hosts the video stage.
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

    rsx! {
        div {
            id: "screenshare-stage",
            // JS toggles display between none/flex as video tracks come and go.
            style: "display:none; position:fixed; bottom:3.25rem; left:50%; transform:translateX(-50%); z-index:30; max-width:74vw; max-height:52vh; gap:8px; padding:8px; background:var(--panel-solid); border:1px solid var(--border); border-radius:12px; box-shadow:0 12px 48px rgba(0,0,0,0.55); align-items:center; justify-content:center; flex-wrap:wrap;",
        }
    }
}
