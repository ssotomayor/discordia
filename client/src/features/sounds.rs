//! The sound bank, and the one sound in it that is about messages.

use dioxus::prelude::*;

use crate::state::use_app_state;

pub(crate) const SFX_JS: &str = r#"
window.dxSfx = window.dxSfx || (function () {
  let ctx = null;
  const lastAt = {};
  const COOLDOWN_MS = 250;
  let masterVolume = 0.7;
  function audio() {
    if (!ctx) { try { ctx = new (window.AudioContext || window.webkitAudioContext)(); } catch (e) { return null; } }
    if (ctx.state === 'suspended') { ctx.resume().catch(function () {}); }
    return ctx;
  }
  function tone(c, at, freq, dur, peak, type) {
    const o = c.createOscillator(); const g = c.createGain();
    o.type = type || 'sine'; o.frequency.setValueAtTime(freq, at);
    o.connect(g); g.connect(c.destination);
    const p = (peak * masterVolume) || 0.0001;
    g.gain.setValueAtTime(0.0001, at);
    g.gain.exponentialRampToValueAtTime(p, at + 0.012);
    g.gain.exponentialRampToValueAtTime(0.0001, at + dur);
    o.start(at); o.stop(at + dur + 0.02);
  }
  function sweep(c, at, f0, f1, dur, peak, type) {
    const o = c.createOscillator(); const g = c.createGain();
    o.type = type || 'sawtooth';
    o.frequency.setValueAtTime(f0, at);
    o.frequency.linearRampToValueAtTime(f1, at + dur);
    o.connect(g); g.connect(c.destination);
    const p = (peak * masterVolume) || 0.0001;
    g.gain.setValueAtTime(0.0001, at);
    g.gain.exponentialRampToValueAtTime(p, at + 0.015);
    g.gain.exponentialRampToValueAtTime(0.0001, at + dur);
    o.start(at); o.stop(at + dur + 0.02);
  }
  function play(name) {
    const now = Date.now();
    if (lastAt[name] && now - lastAt[name] < COOLDOWN_MS) return;
    lastAt[name] = now;
    const c = audio(); if (!c) return;
    const t = c.currentTime;
    switch (name) {
      case 'disconnect':
        tone(c, t, 520, 0.14, 0.13); tone(c, t + 0.10, 330, 0.22, 0.13);
        break;
      case 'watch-start':
        tone(c, t, 620, 0.09, 0.09, 'triangle'); tone(c, t + 0.07, 880, 0.14, 0.09, 'triangle');
        break;
      case 'watch-stop':
        tone(c, t, 720, 0.09, 0.07, 'triangle'); tone(c, t + 0.07, 480, 0.13, 0.07, 'triangle');
        break;
      case 'notify':
        tone(c, t, 660, 0.3, 0.14);
        break;
      case 'connect':
        tone(c, t, 440, 0.12, 0.12); tone(c, t + 0.09, 660, 0.18, 0.12);
        break;
      case 'server-disconnect':
        tone(c, t, 440, 0.15, 0.11); tone(c, t + 0.12, 220, 0.25, 0.11);
        break;
      case 'peer-join':
        tone(c, t, 523, 0.06, 0.08, 'triangle'); tone(c, t + 0.05, 659, 0.10, 0.08, 'triangle');
        break;
      case 'peer-leave':
        tone(c, t, 659, 0.06, 0.08, 'triangle'); tone(c, t + 0.05, 523, 0.10, 0.08, 'triangle');
        break;
      case 'stream-start':
        sweep(c, t, 300, 900, 0.18, 0.10, 'sawtooth');
        break;
      case 'stream-stop':
        sweep(c, t, 900, 300, 0.18, 0.10, 'sawtooth');
        break;
      case 'peer-stream-start':
        tone(c, t, 440, 0.07, 0.06, 'triangle'); tone(c, t + 0.06, 880, 0.12, 0.06, 'triangle');
        break;
      case 'peer-stream-stop':
        tone(c, t, 880, 0.07, 0.06, 'triangle'); tone(c, t + 0.06, 440, 0.12, 0.06, 'triangle');
        break;
      case 'mute':
        tone(c, t, 220, 0.06, 0.10, 'square');
        break;
      case 'unmute':
        tone(c, t, 440, 0.06, 0.10, 'square');
        break;
    }
  }
  function setVolume(v) { masterVolume = Math.max(0, Math.min(1, v)); }
  return { play: play, setVolume: setVolume };
})();
"#;

pub(crate) fn sfx(name: &str) {
    let name = crate::features::screenshare::js_str(name);
    let _ = document::eval(&format!("{SFX_JS}\nwindow.dxSfx.play({name});"));
}

/// Mounted on both screens on purpose. The notification used to live inside the
/// workspace, so a DM arriving on the home screen — the only place DMs work
/// without a server at all — had nothing listening for it.
#[component]
pub fn MessageSounds() -> Element {
    let state = use_app_state();
    let notify = use_memo(move || state.read().notify_tick);
    let mut last_notify = use_signal(|| 0u64);
    use_effect(move || {
        let now = notify();
        if now != 0 && now != *last_notify.peek() {
            sfx("notify");
        }
        last_notify.set(now);
    });

    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let sfx_vol = use_memo(move || settings.read().sfx_volume);
    use_effect(move || {
        let vol = sfx_vol();
        let v = vol as f32 / 100.0;
        let _ = document::eval(&format!("{SFX_JS}\nwindow.dxSfx.setVolume({v});"));
    });

    rsx! { Fragment {} }
}
