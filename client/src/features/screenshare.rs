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
use serde_json::Value;

use crate::features::voice::{use_voice_tx, VoiceCmd};
use crate::protocol::ClientMessage;
use crate::state::{use_app_state, use_gateway};

/// The JS controller. Idempotent so it's safe to prepend to every eval.
const SCREEN_JS: &str = r#"
window.dxScreen = window.dxScreen || (function () {
  let room = null;
  const tracks = {}; // identity -> video track
  const audioTracks = {}; // identity -> audio track (screen-share sound)
  const CONTAINERS = ['screenshare-self', 'screenshare-viewer'];
  const LK = () => window.LivekitClient || window.LiveKitClient;

  // --- Screen-share audio -------------------------------------------------
  // Playback goes through LiveKit's own attach(), never a hand-rolled Web Audio
  // graph: createMediaStreamSource() over a *remote* peer-connection stream
  // emits silence unless the same stream is also sunk into an HTMLMediaElement.
  // That was the original "screen share has no sound" bug — the nodes were
  // wired correctly and carried nothing.
  //
  // Volume is the element's own `volume`, so it is capped at 1.0. An earlier
  // version routed the whole Room through Web Audio (`webAudioMix`) to allow
  // boosting past 100%, and viewers started getting stuck on "Connecting to
  // stream…". Changing how the Room is constructed to make audio louder risks
  // the picture, which is the thing people actually came for.
  //
  // All listener-side: it scales what THIS machine plays. The broadcaster and
  // every other viewer are untouched.
  let audioEl = null;       // element the current stream is attached to
  let audioIdentity = null; // whose stream is currently wired up
  let pendingGain = 1;      // volume to apply when a stream does arrive (0..1)
  let sinkLabel = null;     // app's chosen output device, matched by label
  // Best-effort: follow the output device picked in audio settings. cpal names
  // devices by their OS label while the webview exposes opaque ids, so the two
  // are matched by label. setSinkId is Chromium-only and labels need mic
  // permission to be readable at all; elsewhere the stream plays on the system
  // default.
  function applySink() {
    if (!sinkLabel) return;
    try {
      navigator.mediaDevices.enumerateDevices().then(function (devs) {
        const m = devs.find(function (d) { return d.kind === 'audiooutput' && d.label === sinkLabel; });
        if (!m) return;
        const t = audioIdentity ? audioTracks[audioIdentity] : null;
        if (t && typeof t.setSinkId === 'function') t.setSinkId(m.deviceId).catch(function () {});
      }).catch(function () {});
    } catch (e) {}
  }
  function setSink(label) { sinkLabel = label || null; applySink(); }
  function setStreamVolume(v) {
    // 0..1: without Web Audio this lands on the element's `volume`, which the
    // spec caps at 1.0. The viewer's slider is capped to match.
    pendingGain = Math.max(0, Math.min(1, v));
    const t = audioIdentity ? audioTracks[audioIdentity] : null;
    // setVolume walks the track's attached elements, so it is a no-op until
    // attachAudio has run — pendingGain covers that ordering.
    if (t) { try { t.setVolume(pendingGain); } catch (e) {} }
  }
  function detachAudio() {
    const t = audioIdentity ? audioTracks[audioIdentity] : null;
    if (t) { try { t.detach(); } catch (e) {} }
    if (audioEl) { try { audioEl.remove(); } catch (e) {} audioEl = null; }
    audioIdentity = null;
  }
  function attachAudio(identity) {
    if (audioIdentity === identity && audioEl) return;
    detachAudio();
    const t = audioTracks[identity];
    if (!t) { report(identity, false); return; }
    try {
      audioEl = t.attach();
      // Kept out of view but in the DOM: a detached element is at the browser's
      // mercy as to whether it is allowed to play at all.
      audioEl.style.display = 'none';
      document.body.appendChild(audioEl);
      audioIdentity = identity;
      t.setVolume(pendingGain);
      applySink();
      // Autoplay policy can block the room's audio outright. startAudio() wants
      // a user gesture, and watching a share is one (the click that opened the
      // window), so this is as close to the gesture as we can get.
      if (room && room.canPlaybackAudio === false) {
        room.startAudio().catch(function (e) { console.warn('[dxScreen] startAudio blocked', e); });
      }
      report(identity, true);
    } catch (e) {
      console.warn('[dxScreen] stream audio attach failed', e);
      report(identity, false);
    }
  }
  // Tell Rust whether the stream we're watching actually carries audio, so the
  // viewer's volume control can say so instead of appearing to do nothing.
  function report(identity, present) {
    try { window.postMessage({ __dxf: 'stream-audio', identity: identity, present: !!present }, '*'); } catch (e) {}
  }
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
    // Deliberately the stock options. `webAudioMix` used to be set here to get
    // gain above 100% on stream audio, and viewers started getting stuck on
    // "Connecting to stream…" — the Room constructor governs subscription, so
    // an audio nicety has no business changing it. Volume is capped at 100%
    // via the element instead; a working picture beats a louder one.
    room = new lk.Room({ adaptiveStream: true, dynacast: true });
    room.on(lk.RoomEvent.Disconnected, function (reason) {
      console.warn('[dxScreen] room disconnected', reason);
    });
    room.on(lk.RoomEvent.ConnectionStateChanged, function (st) {
      console.log('[dxScreen] connection state', st);
    });
    room.on(lk.RoomEvent.TrackSubscribed, function (track, pub, participant) {
      if (track.kind === 'audio') {
        audioTracks[participant.identity] = track;
        // The viewer may already be watching this person — the audio track can
        // arrive after the video one.
        const c = document.getElementById('screenshare-viewer');
        if (c && c.getAttribute('data-identity') === participant.identity) attachAudio(participant.identity);
        return;
      }
      if (track.kind !== 'video') return;
      tracks[participant.identity] = track;
      reattach(participant.identity);
    });
    room.on(lk.RoomEvent.TrackUnsubscribed, function (track, pub, participant) {
      if (track.kind === 'audio') {
        if (audioIdentity === participant.identity) detachAudio();
        delete audioTracks[participant.identity];
        // The sharer stopped sending audio (or stopped sharing) — take the
        // viewer's volume control back out of service.
        report(participant.identity, false);
        return;
      }
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
      // The user may have stopped sharing from the browser's native "Stop
      // sharing" bar (which ends the track without going through our Stop
      // button). Notify Rust so it can flip screen_sharing off and tell the
      // server, otherwise the self-preview stays mounted showing nothing.
      notifyShareEnded();
    });
    try { await room.connect(url, token); } catch (e) { console.warn('[dxScreen] connect failed', e); room = null; }
  }
  // Notify the Rust side that local screen sharing has ended (track stopped
  // or unpublished by the browser). We must use window.postMessage (NOT
  // dioxus.send directly) because the ScreenShareBridge's use_future eval is
  // the one whose recv() is awaited — it listens via window.addEventListener
  // and re-sends into dioxus from that eval's context. Calling dioxus.send
  // from here (the SCREEN_JS controller, evaluated by a fire-and-forget eval)
  // goes nowhere because no one is recv-ing on that channel. Same chain as
  // the activities bridge: postMessage → addEventListener → dioxus.send → recv.
  // Did the user close the picker / deny permission, as opposed to the engine
  // rejecting the constraints? Retrying with different constraints cannot help
  // with a decision — it just asks again.
  function isUserCancel(e) {
    const n = e && e.name;
    return n === 'NotAllowedError' || n === 'AbortError' || n === 'SecurityError';
  }
  // Give up on a share attempt and put everything back to idle: release any
  // tracks we already acquired and tell Rust, which clears `screen_sharing`
  // and notifies the server. Without this the app went on believing it was
  // sharing after a cancel, and the self-preview sat on "Starting…" forever.
  function abortShare(stream) {
    if (stream) {
      try { stream.getTracks().forEach(function (t) { t.stop(); }); } catch (e) {}
    }
    notifyShareEnded();
  }
  function notifyShareEnded() {
    try { window.postMessage({ __dxf: 'screen-share-ended' }, '*'); } catch (e) { console.warn('[dxScreen] notifyShareEnded failed', e); }
  }
  function attach(identity, cid) {
    const c = document.getElementById(cid); if (!c) return;
    c.setAttribute('data-identity', identity);
    const t = tracks[identity];
    if (t) attachInto(t, c); else c.querySelectorAll('video').forEach(function (e) { e.remove(); });
    // Only the watch window plays sound; the self-preview must not, or the
    // sharer hears their own machine echoed back.
    if (cid === 'screenshare-viewer') attachAudio(identity);
  }
  function detach(cid) {
    const c = document.getElementById(cid); if (!c) return;
    c.removeAttribute('data-identity');
    c.querySelectorAll('video').forEach(function (e) { e.remove(); });
    if (cid === 'screenshare-viewer') detachAudio();
  }
  async function startShare() {
    if (!room) { console.warn('[dxScreen] not connected yet'); return; }
    try { await room.localParticipant.setScreenShareEnabled(true); } catch (e) { console.warn('[dxScreen] share failed', e); }
  }
  // Helper that runs getDisplayMedia inside the user's gesture, then publishes
  // the captured track directly via publishTrack. This avoids the previous bug
  // where the SDK's internal getDisplayMedia call (triggered by
  // setScreenShareEnabled, outside the user gesture after awaiting the room)
  // was blocked by the browser — which is why the first share attempt failed
  // and only the second (with the room already connected) worked.
  async function requestAndStartShare(quality) {
    if (window._dxf_share_starting) { console.warn('[dxScreen] already starting, ignoring duplicate call'); return; }
    window._dxf_share_starting = true;
    try {
      return await requestAndStartShareInner(quality || {});
    } finally {
      window._dxf_share_starting = false;
    }
  }
  async function requestAndStartShareInner(quality) {
    // If the browser doesn't expose navigator.mediaDevices, fall back to
    // directly calling startShare() and let the LiveKit SDK decide. This may
    // still be blocked if not run inside a user gesture, but it's a useful
    // fallback for embedded webviews without mediaDevices.
    if (!(navigator && navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia)) {
      console.warn('[dxScreen] navigator.mediaDevices.getDisplayMedia not available; falling back to startShare');
      // Wait a short moment for room to exist; otherwise call startShare
      // immediately (it may still prompt or fail depending on environment).
      for (let i = 0; i < 20; i++) { if (room) break; await new Promise(function (r) { setTimeout(r, 100); }); }
      try {
        await startShare();
      } catch (e) {
        console.warn('[dxScreen] startShare fallback failed', e);
        notifyShareEnded();
      }
      return;
    }

    // Prompt for screen capture inside the user gesture. The captured stream
    // is kept open and published directly below — we must NOT stop its tracks
    // here, or LiveKit would have to re-prompt getDisplayMedia (outside the
    // gesture, which browsers reject).
    // `{ video: true }` lets the capturer pick its own (low) defaults, which is
    // what made shares look pixelated. Ask for the preset explicitly. These are
    // `ideal`, not `exact`, so a smaller display still shares at its native
    // size instead of failing the constraint outright.
    const wantW = quality.width || 1920;
    const wantH = quality.height || 1080;
    const wantFps = quality.fps || 30;
    // Ask for the share's audio too. Whether any comes back is up to the
    // platform: Chromium offers tab/system audio and the user still has to tick
    // the box in the picker, while WebKit (macOS) offers none at all. So audio
    // is requested, never required.
    //
    // The fallback order matters. Two independent things can be rejected — the
    // video constraints (some embedded webviews refuse any) and the audio
    // request — so dropping audio is tried BEFORE dropping the constraints.
    // Collapsing those two, as an earlier version did, meant a platform that
    // merely dislikes `audio: true` silently lost the resolution/bitrate preset
    // and went back to shipping a pixelated capture.
    const wantVideo = { width: { ideal: wantW }, height: { ideal: wantH }, frameRate: { ideal: wantFps } };
    // `systemAudio: 'include'` is a TOP-LEVEL option of getDisplayMedia, not an
    // audio constraint — nested inside `audio` it is just an unknown (and
    // therefore ignored) constraint, which is where it was first put. Engines
    // that don't know it ignore it, so asking costs nothing.
    // `systemAudio: 'include'` asks for the whole machine's output when a
    // screen is picked; `windowAudio: 'system'` is the equivalent hint for a
    // *window* pick, which otherwise carries no audio at all. Both are hints —
    // engines that don't know them ignore them, so asking costs nothing.
    // `suppressLocalAudioPlayback: false` keeps the sharer hearing their own
    // audio while it is being shared.
    const richAudio = { suppressLocalAudioPlayback: false };
    // When Rust captures system audio natively there is nothing for the engine
    // to add — asking anyway would capture the machine twice and play it twice.
    const attempts = quality.nativeAudio
      ? [{ video: wantVideo, audio: false }, { video: true }]
      : [
          { video: wantVideo, audio: richAudio, systemAudio: 'include', windowAudio: 'system' },
          { video: wantVideo, audio: true, systemAudio: 'include' },
          { video: wantVideo, audio: true },
          { video: wantVideo, audio: false },
          { video: true, audio: true },
          { video: true },
        ];
    let stream = null;
    let audioAsked = false;
    for (let i = 0; i < attempts.length; i++) {
      try {
        stream = await navigator.mediaDevices.getDisplayMedia(attempts[i]);
        audioAsked = attempts[i].audio !== false && attempts[i].audio !== undefined;
        if (i > 0) console.warn('[dxScreen] getDisplayMedia fell back to attempt', i, attempts[i]);
        break;
      } catch (e) {
        // Closing the picker is a DECISION, not a constraint problem, and the
        // two arrive as the same rejected promise. Treating them alike is what
        // made cancelling take five or six goes: each cancel was read as "those
        // constraints failed" and immediately reopened the picker with the next
        // set. It also explains the disappearing "Share audio" checkbox, since
        // the fallbacks alternate between asking for audio and not
        // (true, true, false, true, none) — so cancel #2 landed on the
        // audio-less attempt, #3 on an audio one again, and so on.
        if (isUserCancel(e)) {
          console.log('[dxScreen] share cancelled by user');
          abortShare(null);
          return;
        }
        if (i === attempts.length - 1) {
          console.warn('[dxScreen] getDisplayMedia denied or failed', e);
          abortShare(null);
          return;
        }
      }
    }
    if (!stream) { abortShare(null); return; }

    // Wait briefly for the room to connect (Server should have provided a
    // token and ScreenShareBridge will call dxScreen.connect). If the room
    // isn't ready after a short timeout, abort and release the capture.
    for (let i = 0; i < 150; i++) {
      if (room) break;
      await new Promise(function (r) { setTimeout(r, 100); });
    }
    if (!room) {
      console.warn('[dxScreen] room not connected yet, cannot start share');
      abortShare(stream);
      return;
    }

    // Publish the already-captured track directly. publishTrack accepts a raw
    // MediaStreamTrack (verified against livekit-client@2.19.2 types) and does
    // NOT re-invoke getDisplayMedia, so the user-gesture grant carries through.
    // source: ScreenShare routes it correctly on the server and triggers the
    // LocalTrackPublished event wired above (which attaches the self-preview).
    const lk = LK();
    const vt = stream.getVideoTracks()[0];
    if (!vt) {
      console.warn('[dxScreen] no video track in captured stream');
      abortShare(stream);
      return;
    }
    // The browser fires `ended` on the MediaStreamTrack when the user closes
    // the shared tab/window or clicks the native "Stop sharing" bar. Wire it
    // so Rust learns immediately and can tear down the self-preview + notify
    // the server. LiveKit preserves this handler through publishTrack.
    vt.addEventListener('ended', function () { notifyShareEnded(); });
    // Tell the encoder this is detail-critical content. Without a contentHint
    // WebRTC assumes camera-style motion video and happily blurs fine detail —
    // text turns to mush the moment anything on screen moves.
    try { vt.contentHint = 'detail'; } catch (e) {}
    // Re-assert the resolution on the track itself. Chromium sometimes honours
    // applyConstraints here even when it ignored them on getDisplayMedia.
    try {
      await vt.applyConstraints({
        width: { ideal: wantW }, height: { ideal: wantH }, frameRate: { ideal: wantFps },
      });
    } catch (e) { console.warn('[dxScreen] applyConstraints ignored', e); }
    const settings = (function () { try { return vt.getSettings(); } catch (e) { return {}; } })();
    console.log('[dxScreen] capturing', settings.width + 'x' + settings.height, '@', settings.frameRate, 'fps');
    try {
      await room.localParticipant.publishTrack(vt, {
        source: lk.Track.Source.ScreenShare,
        // Without an explicit encoding LiveKit falls back to a conservative
        // default bitrate that starves a full-resolution desktop capture.
        videoEncoding: { maxBitrate: quality.bitrate || 6000000, maxFramerate: wantFps },
        // Under bandwidth pressure, drop frames rather than pixels — the
        // default ('maintain-framerate') scales the picture down, which is
        // exactly the pixelation being reported.
        degradationPreference: 'maintain-resolution',
        // Simulcast re-encodes at reduced sizes; for screen share it mostly
        // costs upload bandwidth that the full-resolution layer needs.
        simulcast: false,
      });
      // Publish the share's audio as its own track. LiveKit keeps it separate
      // from the microphone, which is what lets a viewer set the stream's
      // volume independently of the sharer's voice.
      //
      // Tell Rust either way. A share that is silently video-only is the single
      // most confusing outcome here — the sharer has no way to tell that
      // viewers hear nothing, and the viewer's volume slider looks broken.
      const at = stream.getAudioTracks()[0];
      let published = false;
      if (at) {
        try {
          await room.localParticipant.publishTrack(at, { source: lk.Track.Source.ScreenShareAudio });
          published = true;
          console.log('[dxScreen] publishing screen-share audio');
        } catch (e2) { console.warn('[dxScreen] screen-share audio publish failed', e2); }
      } else {
        console.warn('[dxScreen] platform returned no audio track for this share');
      }
      // `supported` = the engine accepted an audio request at all. That is the
      // difference between "your system can't do this" and "you didn't tick the
      // box / picked a window", which need different advice.
      // Report that sharing has genuinely begun. Rust used to assume it had
      // the moment the button was clicked, so the app — and everyone else in
      // the channel — saw "live" while the picker was still open, and had to
      // be walked back on every cancel.
      try { window.postMessage({ __dxf: 'share-started' }, '*'); } catch (e2) {}
      if (!quality.nativeAudio) {
        try {
          window.postMessage(
            { __dxf: 'share-audio', published: published, supported: audioAsked },
            '*'
          );
        } catch (e2) {}
      }
    } catch (e) {
      // Fallback: try the SDK's built-in path. It may re-prompt and fail on a
      // first gesture-less invocation, but costs nothing to attempt.
      console.warn('[dxScreen] direct publishTrack failed, falling back to setScreenShareEnabled', e);
      try { stream.getTracks().forEach(function (t) { t.stop(); }); } catch (e2) {}
      try {
        await startShare();
      } catch (e2) {
        console.warn('[dxScreen] startShare fallback failed', e2);
        // Both routes failed — don't leave the UI claiming to share.
        notifyShareEnded();
      }
    }
  }
  async function stopShare() {
    if (!room) return;
    try { await room.localParticipant.setScreenShareEnabled(false); } catch (e) {}
  }
  async function disconnect() {
    if (room) { try { await room.disconnect(); } catch (e) {} room = null; }
    for (const k in tracks) delete tracks[k];
    for (const k in audioTracks) delete audioTracks[k];
    detachAudio();
    CONTAINERS.forEach(detach);
  }
  return { connect: connect, attach: attach, detach: detach, startShare: startShare, requestAndStartShare: requestAndStartShare, stopShare: stopShare, disconnect: disconnect, setStreamVolume: setStreamVolume, setSink: setSink };
})();
"#;

/// Start/stop sharing — call from a click handler so `getDisplayMedia` runs in
/// a user gesture.
/// Screen-share quality presets, in the order the settings menu shows them.
/// `(id, label, hint)` — the hint is the one-line explanation under the select.
pub const QUALITY_PRESETS: &[(&str, &str, &str)] = &[
    ("smooth", "Smooth — 720p60", "Best for video and animation"),
    ("balanced", "Balanced — 1080p30", "Good default for most sharing"),
    ("crisp", "Crisp — 1080p15", "Sharpest text, lower framerate"),
    ("ultra", "Ultra — 1440p30", "High detail; needs strong upload"),
];

/// Resolve a preset id to `(width, height, fps, max_bitrate_bps)`.
///
/// Screen content is mostly static text, so resolution matters far more than
/// framerate for legibility — the "crisp" preset deliberately trades fps for
/// pixels. Bitrates are well above LiveKit's screen-share defaults because the
/// default is tuned for slide decks, not code editors.
fn quality_preset(id: &str) -> (u32, u32, u32, u32) {
    match id {
        "smooth" => (1280, 720, 60, 4_000_000),
        "crisp" => (1920, 1080, 15, 5_000_000),
        "ultra" => (2560, 1440, 30, 10_000_000),
        // "balanced" and anything unrecognised (older config, typo).
        _ => (1920, 1080, 30, 6_000_000),
    }
}

/// Whether the app captures system audio itself on this platform. When it
/// does, the webview is asked for video only — otherwise the machine's audio
/// would be captured twice and viewers would hear it doubled.
pub fn native_system_audio() -> bool {
    crate::sysaudio::supported()
}

pub fn share_js(on: bool, quality: &str) -> String {
    // When starting, call the user-gesture variant that prompts getDisplayMedia
    // before delegating to the LiveKit flow. When stopping, just stopShare.
    if !on {
        return format!("{SCREEN_JS}\nwindow.dxScreen.stopShare();");
    }
    let (w, h, fps, bitrate) = quality_preset(quality);
    let native_audio = native_system_audio();
    format!(
        "{SCREEN_JS}\nwindow.dxScreen.requestAndStartShare({{width:{w},height:{h},fps:{fps},bitrate:{bitrate},nativeAudio:{native_audio}}});"
    )
}

fn attach_js(identity: &str, container: &str) -> String {
    format!("{SCREEN_JS}\nwindow.dxScreen.attach('{identity}','{container}');")
}

fn detach_js(container: &str) -> String {
    format!("{SCREEN_JS}\nwindow.dxScreen.detach('{container}');")
}

/// Set the local playback gain for the screen-share stream we're watching.
/// `gain` is a linear multiplier (1.0 = as broadcast, 0.0 = muted).
fn stream_volume_js(gain: f32) -> String {
    format!("{SCREEN_JS}\nwindow.dxScreen.setStreamVolume({gain});")
}

/// Point the stream's audio at the output device chosen in audio settings,
/// matched by name. Best-effort — see `applySink` in the JS controller.
pub fn stream_sink_js(device: Option<&str>) -> String {
    // Device names come from the OS and can contain quotes or other characters
    // that would break out of a hand-built JS string literal — let serde_json
    // do the escaping. `null` when no device is chosen (system default).
    let arg = serde_json::to_string(&device).unwrap_or_else(|_| "null".into());
    format!("{SCREEN_JS}\nwindow.dxScreen.setSink({arg});")
}

/// Connects/disconnects the JS screen room as the token comes and goes.
#[component]
pub fn ScreenShareBridge() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
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
            // Report the probe explicitly with `dioxus.send`. Relying on the
            // script's completion value meant `recv::<bool>()` never resolved,
            // the error branch ran, and capture was marked unavailable on
            // machines where sharing works perfectly well — the bogus "install
            // WebView2" tooltip on Windows.
            let mut eval = document::eval(
                "dioxus.send(!!(navigator && navigator.mediaDevices                  && navigator.mediaDevices.getDisplayMedia));",
            );
            match eval.recv::<bool>().await {
                Ok(true) => s.write().screen_capture_available = true,
                Ok(false) => {
                    s.write().screen_capture_available = false;
                    s.write().error_toast = Some(
                        "Screen capture isn't available in this webview. On Windows,                          installing the WebView2 runtime enables it."
                            .into(),
                    );
                }
                // Fail open. A false negative disables a working feature and
                // tells the user something untrue; a false positive costs at
                // most one failed share attempt, which reports its own reason.
                Err(e) => {
                    eprintln!("[screen] capture probe failed, assuming available: {e:?}");
                    s.write().screen_capture_available = true;
                }
            }
        }
    });

    // There is deliberately no effect here re-triggering the share when the
    // token arrives. The share button already calls `requestAndStartShare`
    // inside the click (which it must, for the user-gesture grant) and that
    // call waits for the room itself. A second trigger from here raced the
    // first, dropped the quality preset (it passed no argument), and gave the
    // flow a way to reopen the picker on its own.

    // Listen for the JS-side `screen-share-ended` signal. The browser fires
    // `ended` on the captured MediaStreamTrack when the user closes the shared
    // tab/window or clicks the native "Stop sharing" bar; the JS controller
    // forwards that via `dioxus.send({ __dxf: 'screen-share-ended' })`. Without
    // this, the self-preview would stay mounted showing a frozen/black frame.
    // Guards against double-wiring across hot reloads (same pattern as the
    // activities bridge).
    let gateway_end = gateway.clone();
    let voice = use_voice_tx();
    use_future(move || {
        let mut state = state.clone();
        let gateway = gateway_end.clone();
        let voice = voice.clone();
        async move {
            let bridge_js = r#"
            if (!window.__dxfShareEndWired) {
              window.__dxfShareEndWired = true;
              window.addEventListener('message', function (e) {
                var d = e.data;
                if (d && (d.__dxf === 'screen-share-ended' || d.__dxf === 'share-started' || d.__dxf === 'share-audio' || d.__dxf === 'stream-audio')) {
                  try { dioxus.send(d); } catch (err) {}
                }
              });
            }
            "#;
            let mut eval = document::eval(bridge_js);
            loop {
                match eval.recv::<Value>().await {
                    Ok(msg) => match msg.get("__dxf").and_then(|v| v.as_str()) {
                        // Publishing succeeded — only now is this a share.
                        Some("share-started") => {
                            let cid = state.read().voice.channel_id;
                            state.write().screen_sharing = true;
                            if let Some(c) = cid {
                                gateway.send(ClientMessage::SetScreenShare {
                                    channel_id: c,
                                    sharing: true,
                                });
                            }
                            // Where we capture system audio ourselves, it rides
                            // along on the voice room as a second track.
                            if native_system_audio() {
                                voice.send(VoiceCmd::SetSystemAudio { enabled: true });
                            }
                        }
                        Some("screen-share-ended") => {
                            let cid = state.read().voice.channel_id;
                            state.write().screen_sharing = false;
                            if let Some(c) = cid {
                                gateway.send(ClientMessage::SetScreenShare {
                                    channel_id: c,
                                    sharing: false,
                                });
                            }
                            voice.send(VoiceCmd::SetSystemAudio { enabled: false });
                        }
                        // Our own share: did the platform give us any audio to
                        // send? Silence here is a platform limit, not a bug we
                        // can fix client-side, so say so rather than let the
                        // sharer assume viewers can hear their machine.
                        Some("share-audio") => {
                            let published =
                                msg.get("published").and_then(|v| v.as_bool()).unwrap_or(false);
                            // Whether the platform *can* do it at all, versus
                            // whether this particular pick included it. Saying
                            // "your platform can't" when the user simply left
                            // the checkbox unticked — or picked a window, which
                            // never carries audio — is just wrong.
                            let supported = msg
                                .get("supported")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            eprintln!("[screen] share audio published={published} supported={supported}");
                            if !published {
                                state.write().error_toast = Some(if supported {
                                    "Sharing video only. To include sound, re-share and tick \
                                     \"Share audio\" in the picker — choose a tab or your whole \
                                     screen, as single windows can't carry audio."
                                        .into()
                                } else {
                                    "Sharing video only — this system doesn't let the app capture \
                                     audio from a screen share, so viewers won't hear your machine."
                                        .into()
                                });
                            }
                        }
                        // A share we're watching: whether it carries audio at
                        // all, so the volume control can be honest about it.
                        Some("stream-audio") => {
                            let present =
                                msg.get("present").and_then(|v| v.as_bool()).unwrap_or(false);
                            if let Some(id) = msg.get("identity").and_then(|v| v.as_str()) {
                                eprintln!("[screen] watching {}: audio={present}", &id[..id.len().min(8)]);
                                let mut s = state.write();
                                if present {
                                    s.stream_has_audio.insert(id.to_string());
                                } else {
                                    s.stream_has_audio.remove(id);
                                }
                            }
                        }
                        _ => {}
                    },
                    Err(_) => break,
                }
            }
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
                    // Run stop on mousedown (not click) and before the header's
                    // drag handler can fire — stop_propagation alone wasn't
                    // reliable in wry/WebView2, leaving the drag overlay z-50
                    // to swallow the subsequent click. Acting on mousedown is
                    // strictly safer: the window tears down before any drag
                    // interaction can start.
                    onmousedown: move |e| {
                        e.stop_propagation();
                        let cid = state.read().voice.channel_id;
                        state.write().screen_sharing = false;
                        // Quality only matters when starting a share.
                        let _ = document::eval(&share_js(false, ""));
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
                    // Volume first: the gain node has to be at the right level
                    // before the stream is wired into it, or a stream the user
                    // had turned down blasts at full volume for a moment.
                    let s = state.read();
                    let js = format!(
                        "{}\n{}",
                        stream_volume_js(s.stream_gain_of(pk)),
                        attach_js(pk, "screenshare-viewer"),
                    );
                    drop(s);
                    let _ = document::eval(&js);
                }
                None => {
                    let _ = document::eval(&detach_js("screenshare-viewer"));
                }
            }
            last.set(v);
        }
    });

    // Stream audio follows the watch window. The mixer defaults these tracks
    // to silent, so this is what turns them on — and it re-runs when the
    // volume or mute changes, keeping one rule for "how loud is this stream".
    let watching = use_memo(move || state.read().screen_viewing.clone());
    let stream_levels = use_memo(move || {
        let s = state.read();
        (s.stream_volumes.clone(), s.stream_muted.clone())
    });
    let voice_for_stream = use_voice_tx();
    use_effect(move || {
        let watched = watching();
        let _ = stream_levels();
        let s = state.read();
        // Every known sharer gets an explicit gain: whoever we're watching at
        // their chosen level, everyone else at zero.
        let mut seen: Vec<String> = s.screen_shares.values().flatten().cloned().collect();
        if let Some(w) = watched.clone() {
            if !seen.contains(&w) {
                seen.push(w);
            }
        }
        for pk in seen {
            let gain = if Some(&pk) == watched.as_ref() { s.stream_gain_of(&pk) } else { 0.0 };
            voice_for_stream.send(VoiceCmd::SetStreamVolume { pubkey: pk, gain });
        }
    });

    // Follow the output device chosen in audio settings.
    let output_device = use_memo(move || state.read().selected_output_device.clone());
    use_effect(move || {
        let _ = document::eval(&stream_sink_js(output_device().as_deref()));
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

    // Stream audio controls. Deliberately separate from the participant's voice
    // volume in the channel roster: someone's game audio and their microphone
    // are two different things to want quieter. Both are listener-side only.
    let stream_volume = state.read().stream_volumes.get(&pk).copied().unwrap_or(100);
    let stream_muted = state.read().stream_muted.contains(&pk);
    // A video-only share has nothing for these to act on. Showing a live-looking
    // slider over silence is how "the volume control does nothing" starts.
    //
    // Two ways a stream can have sound: the webview reported an audio track, or
    // the sharer is capturing natively and it arrives on the voice room instead
    // — where the JS never sees it, so it can't report anything.
    let has_audio = {
        let s = state.read();
        s.stream_has_audio.contains(&pk) || s.stream_native_audio.contains(&pk)
    };
    let pk_vol = pk.clone();
    let pk_mute = pk.clone();
    let apply_stream = move |vol: u32, muted: bool| {
        let gain = if muted { 0.0 } else { vol as f32 / 100.0 };
        let _ = document::eval(&stream_volume_js(gain));
    };
    let apply_from_slider = apply_stream;

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
                // Stream audio: mute + level. Affects only our own playback —
                // the broadcaster and every other viewer are unaffected.
                div {
                    class: "flex items-center gap-1.5 mr-2",
                    onmousedown: move |e| e.stop_propagation(),
                    if !has_audio {
                        span {
                            class: "text-[10px] text-[var(--text-dim)] italic",
                            title: "The sharer's platform didn't provide system audio for this stream, so there is nothing to play.",
                            "no stream audio"
                        }
                    }
                    button {
                        class: if stream_muted {
                            "w-6 h-6 flex items-center justify-center rounded text-[var(--danger)] disabled:opacity-40"
                        } else {
                            "w-6 h-6 flex items-center justify-center rounded text-[var(--text-dim)] hover:text-[var(--text)] disabled:opacity-40"
                        },
                        disabled: !has_audio,
                        title: if stream_muted { "Unmute stream audio" } else { "Mute stream audio" },
                        onclick: move |_| {
                            let now = !stream_muted;
                            {
                                let mut s = state.write();
                                if now { s.stream_muted.insert(pk_mute.clone()); } else { s.stream_muted.remove(&pk_mute); }
                            }
                            apply_stream(stream_volume, now);
                        },
                        dangerous_inner_html: if stream_muted {
                            crate::features::icons::SPEAKER_OFF
                        } else {
                            crate::features::icons::SPEAKER
                        },
                    }
                    input {
                        r#type: "range",
                        min: "0",
                        // 100 not 200: without Web Audio the gain is the media
                        // element's `volume`, which the spec caps at 1.0.
                        max: "100",
                        value: "{stream_volume}",
                        disabled: stream_muted || !has_audio,
                        class: "w-24 accent-[var(--accent)] disabled:opacity-40",
                        title: "Stream volume (yours only)",
                        oninput: move |e| {
                            let val: u32 = e.value().parse().unwrap_or(100).clamp(0, 200);
                            state.write().stream_volumes.insert(pk_vol.clone(), val);
                            apply_from_slider(val, stream_muted);
                        },
                    }
                    span { class: "text-[10px] text-[var(--text-dim)] w-8 text-right", "{stream_volume}%" }
                }
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
