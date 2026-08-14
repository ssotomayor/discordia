//! Screen sharing via the LiveKit JS SDK running inside the webview.
//!
//! Audio stays on the Rust LiveKit SDK; screen *video* runs here because the
//! webview can capture (`getDisplayMedia`) and render (`<video>`) it. The JS
//! client joins a SEPARATE room (`screen-…`) from voice so peers never download
//! the video just to hear a call. Who's sharing is tracked via our own protocol
//! (`ScreenShareState`), not the JS layer.
//!
//! **`screen-…` is a misnomer now, and the name is load-bearing so it stays.**
//! That room carries webcams too (`features::camera`), published on this same
//! connection under the bare pubkey and told apart by `TrackSource`. Which is
//! why every video track here is keyed by identity *and* source: one
//! participant can be sending a screen and a face at once.
//!
//! Stream *audio* reaches the ear by one of two routes, and neither of them is
//! this file's `<audio>` element unless the server is too old to offer the
//! first. Either the sharer captured it natively (`sysaudio`) and published it
//! on the voice room, or the webview captured it and `features::voice` joins
//! this room audio-only to subscribe. Both land in the same cpal mixer, which
//! is what makes stream volume and the output-device choice work the same way
//! they do for voice.
//!
//! Two on-screen surfaces, each a container the JS attaches a `<video>` into:
//! - `#screenshare-self`   — small draggable self-preview for the sharer.
//! - `#screenshare-viewer` — large draggable/resizable window for watchers.

use dioxus::prelude::*;
use serde_json::Value;

use crate::features::voice::{VoiceCmd, use_voice_tx};
use crate::protocol::ClientMessage;
use crate::state::{use_app_state, use_gateway};

/// The JS controller. Idempotent so it's safe to prepend to every eval.
///
/// `pub(crate)` because `features::camera` drives the *same* `window.dxScreen`:
/// the camera publishes on this connection, so it must not build a second one.
pub(crate) const SCREEN_JS: &str = r#"
window.dxScreen = window.dxScreen || (function () {
  let room = null;
  let desiredRoom = null;
  let reconnectTimer = null;
  let reconnectAttempt = 0;
  // Our own outgoing share audio, held because we published it by hand.
  // setScreenShareEnabled(false) retracts what the SDK created for itself; this
  // track is not that, so stopping the share has to stop it explicitly or the
  // machine's sound keeps going out over a share that visibly ended.
  let localShareAudio = null;
  // The webcam, held across room restarts on purpose. Republishing a track we
  // already hold calls no capture API, so it needs no user gesture — which is
  // the only reason an automatic reconnect can restore video at all.
  // The E2EE passphrase for this session, and the provider awaiting it while a
  // Room is being built. Held at module scope because `connect` runs again on
  // every reconnect and the key must survive that.
  let e2eeKey = null;
  let pendingKeyProvider = null;
  let localCameraTrack = null;
  let localCameraStream = null;
  let lastCameraOpts = {};
  let cameraStarting = false;
  // Video tracks are keyed by identity AND source, not identity alone. One
  // participant can publish a screen and a webcam at once, and on Windows both
  // come from the bare pubkey — so an identity-only map holds whichever arrived
  // last and shows the webcam in the screen tile, or the reverse, depending on
  // timing. `source` is a property of the publication, so it is read there
  // rather than guessed from the identity.
  const tracks = {}; // "identity\0kind" -> video track
  const audioTracks = {}; // identity -> audio track (screen-share sound)
  function trackKey(id, kind) { return id + '|' + kind; }
  // Anything that is not explicitly a camera renders as the screen surface —
  // including the empty source an older peer publishes, which used to be the
  // only kind there was.
  function kindOf(pub, track) {
    const s = (pub && pub.source) || (track && track.source) || '';
    return s === 'camera' ? 'camera' : 'screen';
  }
  // Which container is showing what. Replaces a fixed CONTAINERS list, because
  // camera tiles are created and destroyed as people toggle. Keeping the
  // element here is also what lets us `detach` it: `attachInto` used to drop
  // elements by clearing innerHTML, which leaves them in the track's
  // attachedElements forever, and adaptiveStream counts those when deciding
  // whether anyone is still watching.
  const attached = {}; // containerId -> { identity, kind, track, el }
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
  // Set by Rust when the native screen-audio room is actually joined — not
  // merely when a token for it exists. Everything this guards is the fallback
  // path: it runs against older servers, and again whenever the native join
  // fails or its room drops, so a failure degrades to playback here rather than
  // to nobody playing at all.
  let nativeStreamAudio = false;
  function setNativeStreamAudio(on) {
    const was = nativeStreamAudio;
    nativeStreamAudio = !!on;
    if (was === nativeStreamAudio) return;
    if (nativeStreamAudio) { detachAudio(); applyAudioSubscriptions(); }
    // Handing playback back: resubscribe, and pick up whoever is being watched
    // right now — the track may have arrived while we were standing down, in
    // which case no future event would attach it.
    else { applyAudioSubscriptions(); attachWatched(); }
  }
  // In native mode this room would still auto-subscribe to the screen-audio
  // track it no longer plays, so every viewer downloads that stream twice.
  // Per-publication because the room is shared with the video, which we do want.
  // Runs for tracks already present when the mode flips and for ones published
  // afterwards.
  function applyAudioSubscriptions() {
    if (!room || !room.remoteParticipants) return;
    room.remoteParticipants.forEach(function (p) {
      p.trackPublications.forEach(applyAudioSubscription);
    });
  }
  function applyAudioSubscription(pub) {
    if (!pub || pub.kind !== 'audio') return;
    try { pub.setSubscribed(!nativeStreamAudio); } catch (e) { console.warn('[dxScreen] audio subscribe toggle failed', e); }
  }
  // Attach the stream of whoever the watch window currently points at.
  function attachWatched() {
    const c = document.getElementById('screenshare-viewer');
    const id = c && c.getAttribute('data-identity');
    if (id) attachAudio(id);
  }
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
  function attachInto(track, c, cid, identity, kind) {
    const prev = attached[cid];
    if (prev && prev.track && prev.el) { try { prev.track.detach(prev.el); } catch (e) {} }
    c.innerHTML = '';
    const el = track.attach();
    el.muted = true; el.autoplay = true; el.playsInline = true;
    el.style.width = '100%'; el.style.height = '100%'; el.style.background = '#000';
    // A shared screen must not be cropped — losing an edge loses content. A face
    // in a fixed-ratio tile is the opposite: letterboxing it wastes the tile.
    el.style.objectFit = kind === 'camera' ? 'cover' : 'contain';
    c.appendChild(el);
    attached[cid] = { identity: identity, kind: kind, track: track, el: el };
  }
  // --- Identity suffixes ---------------------------------------------------
  // A share captured natively is published under `{pubkey}#video`, because the
  // webview already occupies the bare pubkey in this room and LiveKit allows one
  // connection per identity (see `server::livekit::screen_video_identity`).
  // Everything user-facing — our own protocol, `data-identity`, the watch
  // window — keys off the bare pubkey, so the suffix is resolved here and
  // nowhere else.
  const VIDEO_SUFFIX = '#video';
  function baseIdentity(id) {
    return id.endsWith(VIDEO_SUFFIX) ? id.slice(0, -VIDEO_SUFFIX.length) : id;
  }
  // The video track for a sharer, whichever path captured it. Webview captures
  // land on the bare pubkey, native ones on the suffixed identity; a given
  // sharer only ever has one of the two.
  //
  // `id` is always a BASE identity here. The suffix rule applies to screens
  // only: a camera is always captured by the webview, so it has no suffixed
  // form to look for.
  function videoTrackFor(id, kind) {
    if (kind === 'camera') return tracks[trackKey(id, 'camera')];
    return tracks[trackKey(id, 'screen')] || tracks[trackKey(id + VIDEO_SUFFIX, 'screen')];
  }
  // Re-render any container currently pointed at `identity` for this kind, which
  // arrives here as the *publisher's* identity and so may carry the suffix.
  function reattach(identity, kind) {
    const base = baseIdentity(identity);
    Object.keys(attached).forEach(function (cid) {
      const a = attached[cid];
      if (!a || a.identity !== base || a.kind !== kind) return;
      const c = document.getElementById(cid);
      // A camera tile can be unmounted by Dioxus without a detach call — the
      // registry has to be pruned here or it grows for the whole session.
      if (!c) { delete attached[cid]; return; }
      const t = videoTrackFor(base, kind);
      if (t) attachInto(t, c, cid, base, kind);
      else c.querySelectorAll('video').forEach(function (e) { e.remove(); });
    });
  }
  async function ensureLib() { for (let i = 0; i < 100 && !LK(); i++) { await new Promise(function (r) { setTimeout(r, 100); }); } return !!LK(); }
  function reportRoomProblem(kind, detail) {
    try { window.postMessage({ __dxf: kind, detail: String(detail || '') }, '*'); } catch (e) {}
  }
  function clearRemoteTracks() {
    for (const k in tracks) delete tracks[k];
    for (const k in audioTracks) delete audioTracks[k];
    detachAudio();
    Object.keys(attached).forEach(function (cid) {
      const a = attached[cid];
      if (a && a.track && a.el) { try { a.track.detach(a.el); } catch (e) {} }
      const c = document.getElementById(cid);
      if (c) c.querySelectorAll('video').forEach(function (e) { e.remove(); });
      // Keep the entry: the container is still pointed at this identity and
      // kind, so a reconnect that re-subscribes should refill it. Only the
      // track and element are gone.
      if (a) { a.track = null; a.el = null; }
    });
  }
  function scheduleReconnect() {
    if (!desiredRoom || reconnectTimer) return;
    const delay = Math.min(1500 * Math.pow(2, reconnectAttempt), 15000);
    reconnectAttempt++;
    reconnectTimer = setTimeout(function () {
      reconnectTimer = null;
      if (desiredRoom && !room) connect(desiredRoom.url, desiredRoom.token);
    }, delay);
  }
  async function connect(url, token, key) {
    e2eeKey = key || null;
    const same = desiredRoom && desiredRoom.url === url && desiredRoom.token === token;
    desiredRoom = { url: url, token: token };
    if (room) {
      if (same) return;
      // A DIFFERENT token means a different channel's room. `if (room) return;`
      // used to swallow that, so moving between voice channels left the webview
      // attached to the room you left. Invisible while this connection only
      // rendered other people's shares; now that it also publishes the camera,
      // it would put your face in the channel you walked out of.
      await stopLocalShareAudio();
      const previous = room;
      room = null;
      try { await previous.disconnect(); } catch (e) {}
      clearRemoteTracks();
    }
    if (!(await ensureLib())) {
      console.warn('[dxScreen] livekit lib not loaded');
      reportRoomProblem('screen-room-error', 'LiveKit did not load');
      scheduleReconnect();
      return;
    }
    const lk = LK();
    // Deliberately the stock options. `webAudioMix` used to be set here to get
    // gain above 100% on stream audio, and viewers started getting stuck on
    // "Connecting to stream…" — the Room constructor governs subscription, so
    // an audio nicety has no business changing it. Volume is capped at 100%
    // via the element instead; a working picture beats a louder one.
    // E2EE, when a key was handed down (roadmap Stage 3). The worker has to be
    // constructed here rather than reused: the SDK ties one to a Room, and this
    // function builds a fresh Room on every reconnect.
    //
    // `ExternalE2EEKeyProvider` with the same passphrase the native rooms use.
    // The salt and derivation are both SDKs' defaults, which is what lets a
    // frame published by the Rust side be decrypted by this one — the two never
    // negotiate, they only happen to agree, so a mismatch shows up as noise
    // rather than as an error.
    const opts = { adaptiveStream: true, dynacast: true };
    if (e2eeKey && !window.__dxfE2eeWorkerSrc) {
      console.error('[dxScreen] e2ee requested but the worker source is missing');
      post('e2ee-error', { detail: 'the encryption worker was not injected' });
    } else if (e2eeKey) {
      try {
        const provider = new lk.ExternalE2EEKeyProvider();
        opts.e2ee = {
          keyProvider: provider,
          worker: new Worker(
            URL.createObjectURL(
              new Blob([window.__dxfE2eeWorkerSrc], { type: 'application/javascript' })
            )
          ),
        };
        pendingKeyProvider = provider;
      } catch (e) {
        console.error('[dxScreen] could not set up e2ee', e);
        post('e2ee-error', { detail: String((e && e.message) || e) });
      }
    }
    const thisRoom = new lk.Room(opts);
    room = thisRoom;
    if (e2eeKey && pendingKeyProvider) {
      // setKey before enabling, so the first frames are not published under a
      // key nobody has.
      try {
        await pendingKeyProvider.setKey(e2eeKey);
        await thisRoom.setE2EEEnabled(true);
      } catch (e) {
        console.error('[dxScreen] enabling e2ee failed', e);
        post('e2ee-error', { detail: String((e && e.message) || e) });
      }
      pendingKeyProvider = null;
    }
    thisRoom.on(lk.RoomEvent.Disconnected, function (reason) {
      console.warn('[dxScreen] room disconnected', reason);
      // Ignore a late event from a room already replaced by a newer attempt.
      if (room !== thisRoom) return;
      room = null;
      // Our hand-published share audio belonged to the room that just went
      // away. Left alone, `localShareAudio` points into a dead room: the
      // capture keeps running with the OS indicator lit, and the next
      // stopShare aims its unpublish at whatever room exists by then. Called
      // after `room = null` on purpose, so it skips the unpublish — there is
      // nothing to unpublish from — and just releases the capture.
      stopLocalShareAudio();
      clearRemoteTracks();
      if (desiredRoom) {
        reportRoomProblem('screen-room-reconnecting', reason || 'disconnected');
        scheduleReconnect();
      }
    });
    thisRoom.on(lk.RoomEvent.ConnectionStateChanged, function (st) {
      console.log('[dxScreen] connection state', st);
    });
    // Fires before the SDK's own auto-subscribe settles, so a screen-audio
    // track published while native mode is on is dropped rather than downloaded.
    thisRoom.on(lk.RoomEvent.TrackPublished, function (pub) {
      applyAudioSubscription(pub);
    });
    thisRoom.on(lk.RoomEvent.TrackSubscribed, function (track, pub, participant) {
      if (track.kind === 'audio') {
        audioTracks[participant.identity] = track;
        if (nativeStreamAudio) return;
        // The viewer may already be watching this person — the audio track can
        // arrive after the video one.
        const c = document.getElementById('screenshare-viewer');
        if (c && c.getAttribute('data-identity') === participant.identity) attachAudio(participant.identity);
        return;
      }
      if (track.kind !== 'video') return;
      const kind = kindOf(pub, track);
      tracks[trackKey(participant.identity, kind)] = track;
      reattach(participant.identity, kind);
    });
    thisRoom.on(lk.RoomEvent.TrackUnsubscribed, function (track, pub, participant) {
      if (track.kind === 'audio') {
        if (audioIdentity === participant.identity) detachAudio();
        delete audioTracks[participant.identity];
        // The sharer stopped sending audio (or stopped sharing) — take the
        // viewer's volume control back out of service.
        //
        // Only when this room is the one playing it. In native mode the entry
        // belongs to the Rust side, and any resubscribe cycle here — a
        // livekit-client reconnect, say — would fire unsubscribe then subscribe,
        // clearing a flag the subscribe half no longer restores. The controls go
        // dead while the audio is still playing.
        if (!nativeStreamAudio) report(participant.identity, false);
        return;
      }
      if (track.kind !== 'video') return;
      const kind = kindOf(pub, track);
      delete tracks[trackKey(participant.identity, kind)];
      reattach(participant.identity, kind);
    });
    thisRoom.on(lk.RoomEvent.LocalTrackPublished, function (pub) {
      if (!pub.track || pub.track.kind !== 'video') return;
      const kind = kindOf(pub, pub.track);
      tracks[trackKey(thisRoom.localParticipant.identity, kind)] = pub.track;
      reattach(thisRoom.localParticipant.identity, kind);
    });
    thisRoom.on(lk.RoomEvent.LocalTrackUnpublished, function (pub) {
      if (!pub.track || pub.track.kind !== 'video') return;
      const kind = kindOf(pub, pub.track);
      delete tracks[trackKey(thisRoom.localParticipant.identity, kind)];
      reattach(thisRoom.localParticipant.identity, kind);
      // Branch on kind, or turning the camera off ends the screen share: this
      // arm fires for BOTH local video publications, and `notifyShareEnded`
      // makes Rust clear `screen_sharing` and announce it to the whole channel.
      //
      // Either way the point is the same — the user may have stopped from
      // outside our UI (the browser's own "Stop sharing" bar, or the OS taking
      // the camera), which ends the track without going through our button. Rust
      // has to hear about it or the preview stays mounted showing nothing.
      if (kind === 'camera') notifyCameraEnded(); else notifyShareEnded();
    });
    try {
      await thisRoom.connect(url, token);
      reconnectAttempt = 0;
    } catch (e) {
      console.warn('[dxScreen] connect failed', e);
      if (room === thisRoom) room = null;
      reportRoomProblem('screen-room-error', e && e.message ? e.message : e);
      scheduleReconnect();
      return;
    }
    // Native mode is usually decided before this room exists, so the sweep that
    // setNativeStreamAudio would have run found nothing to sweep. Anyone already
    // publishing when we arrive is caught here.
    applyAudioSubscriptions();
    // Restore the camera into the new room. This republishes a track we already
    // hold rather than re-acquiring it, which is what makes an automatic
    // reconnect able to bring video back without a user gesture.
    if (localCameraTrack && localCameraTrack.readyState !== 'ended') {
      try {
        await thisRoom.localParticipant.publishTrack(localCameraTrack, cameraPublishOpts(lastCameraOpts));
      } catch (e) {
        console.warn('[dxScreen] camera republish failed', e);
        notifyCameraEnded();
      }
    }
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
  function attach(identity, cid, kind, tries) {
    kind = kind || 'screen';
    const c = document.getElementById(cid);
    if (!c) {
      // Camera tiles are rendered by Dioxus in the same turn that asks for the
      // attach, so the container can legitimately be a frame late. The two fixed
      // screen containers never take this path.
      if ((tries || 0) < 20) setTimeout(function () { attach(identity, cid, kind, (tries || 0) + 1); }, 50);
      return;
    }
    c.setAttribute('data-identity', identity);
    attached[cid] = { identity: identity, kind: kind, track: null, el: null };
    const t = videoTrackFor(identity, kind);
    if (t) attachInto(t, c, cid, identity, kind); else c.querySelectorAll('video').forEach(function (e) { e.remove(); });
    if (!t && kind === 'screen') {
      // A publication can legitimately arrive after the watch window opens,
      // but waiting forever is not a state. Keep the identity check so an old
      // timeout cannot complain after the user switched to another stream.
      //
      // Screens only: this reports a problem whose advice is "the sharer's build
      // is too old to publish where you can see it", which is wrong for a camera
      // and would fire every time someone's camera is slow to arrive.
      setTimeout(function () {
        const current = document.getElementById(cid);
        if (current && current.getAttribute('data-identity') === identity && !videoTrackFor(identity, 'screen')) {
          reportRoomProblem('screen-track-timeout', identity);
        }
      }, 10000);
    }
    // Only the watch window plays sound; the self-preview must not, or the
    // sharer hears their own machine echoed back.
    //
    // And only when the native side isn't taking the audio. It normally is —
    // that is what lets stream sound follow the app's output device like voice
    // does, instead of being stuck on whatever the webview picked. Playing it
    // here as well would be the same stream heard twice.
    if (cid === 'screenshare-viewer' && !nativeStreamAudio) attachAudio(identity);
  }
  function detach(cid) {
    const a = attached[cid];
    if (a && a.track && a.el) { try { a.track.detach(a.el); } catch (e) {} }
    delete attached[cid];
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
      // Tell Rust too. This branch used to warn to a console nobody has open,
      // so pressing Share appeared to do nothing at all.
      try {
        window.postMessage(
          { __dxf: 'share-unavailable', secure: !!window.isSecureContext },
          '*'
        );
      } catch (e) {}
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
    // Scope matters more than getting audio at all costs.
    //
    // `windowAudio: 'window'` captures ONLY the selected window's audio. The
    // other legal value, 'system', captures the whole machine mix even for a
    // window pick — which drags this app's own output in with it, so everyone
    // in the call hears themselves come back. That is the echo, and it is what
    // application-scoped capture (what Discord does) avoids by construction.
    //
    // `systemAudio: 'include'` still applies to *screen* picks, where the whole
    // mix is the only thing on offer and the echo is unavoidable in the webview
    // — see the warning after capture.
    //
    // `suppressLocalAudioPlayback: false` keeps the sharer hearing their own
    // audio while it is being shared.
    const richAudio = { suppressLocalAudioPlayback: false };
    // Does the user want their machine heard at all? App-level setting, on by
    // default; the engine's own "Share audio" checkbox is a separate decision
    // we cannot make for them.
    const wantAudio = quality.audio !== false;
    // Where native (Rust-side) capture takes over: 'always', 'monitor' — only a
    // whole-screen pick — or 'never'. Which one applies to THIS share cannot be
    // settled here: it depends on what the user picks, and the picker hasn't
    // opened yet. So we only decide the constraints now, and settle the rest
    // below once `displaySurface` is readable.
    const nativeMode = wantAudio ? (quality.nativeAudio || 'never') : 'never';
    // Asking the engine for audio is pointless only where native capture is
    // certain to take over ('always') — there it would capture the machine
    // twice and play it twice. Under 'monitor' we must still ask, because a
    // window pick keeps the engine's window-scoped audio.
    const attempts = (!wantAudio || nativeMode === 'always')
      ? [{ video: wantVideo, audio: false }, { video: true }]
      : [
          { video: wantVideo, audio: richAudio, systemAudio: 'include', windowAudio: 'window' },
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
    // The picker has closed, so what was actually shared is finally knowable.
    // This is the only point at which the native-vs-engine question can be
    // answered: 'monitor' mode hinges on the surface, and the surface is a
    // property of the user's choice, not of the platform.
    let surface = '';
    try { surface = (vt.getSettings() || {}).displaySurface || ''; } catch (e) {}
    const useNative = nativeMode === 'always' || (nativeMode === 'monitor' && surface === 'monitor');
    // The browser fires `ended` on the MediaStreamTrack when the user closes
    // the shared tab/window or clicks the native "Stop sharing" bar. Wire it
    // so Rust learns immediately and can tear down the self-preview + notify
    // the server. LiveKit preserves this handler through publishTrack.
    vt.addEventListener('ended', function () { notifyShareEnded(); });
    // What the encoder should protect when it can't have everything. 'detail'
    // preserves fine text at the cost of smoothness; 'motion' does the reverse.
    // This follows the chosen preset rather than being fixed at 'detail', which
    // made every preset behave like the text-oriented one.
    try { vt.contentHint = quality.hint || 'detail'; } catch (e) {}
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
        // Which way to give under pressure, per preset. Fixed at
        // 'maintain-resolution' this dropped frames on every share, so a busy
        // screen turned into a slideshow no matter which preset was picked.
        degradationPreference: quality.degradation || 'balanced',
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
      // Native capture wins wherever it applies. Under 'monitor' the engine was
      // asked for audio before the surface was known, so a whole-screen pick can
      // arrive holding a track we now don't want: it is the same machine the
      // native path is about to capture, except it also contains this app's own
      // output. Publishing both would be the machine heard twice, echo included.
      if (useNative && at) {
        try { at.stop(); } catch (e2) {}
      }
      // Engine audio from a whole-screen pick is the system mix, which includes
      // this app's own playback — so other people's voices ride back out and
      // they hear themselves. A window capture is scoped to that window and is
      // safe. Nothing in getDisplayMedia can exclude our own process, so where
      // the engine is in charge the honest move is to say so. Where native
      // capture is in charge there is nothing to warn about: it excludes us at
      // the source.
      if (!useNative && at && surface === 'monitor') {
        try {
          window.postMessage({ __dxf: 'share-echo-risk' }, '*');
        } catch (e2) {}
      }
      let published = false;
      if (!useNative && at) {
        try {
          await room.localParticipant.publishTrack(at, { source: lk.Track.Source.ScreenShareAudio });
          published = true;
          localShareAudio = at;
          console.log('[dxScreen] publishing screen-share audio');
        } catch (e2) { console.warn('[dxScreen] screen-share audio publish failed', e2); }
      } else if (!useNative && wantAudio) {
        console.warn('[dxScreen] platform returned no audio track for this share');
      }
      // `supported` = the engine accepted an audio request at all. That is the
      // difference between "your system can't do this" and "you didn't tick the
      // box / picked a window", which need different advice.
      // Report that sharing has genuinely begun. Rust used to assume it had
      // the moment the button was clicked, so the app — and everyone else in
      // the channel — saw "live" while the picker was still open, and had to
      // be walked back on every cancel.
      // `nativeAudio` rides along because Rust cannot recompute it: the surface
      // it depends on is only visible here.
      try { window.postMessage({ __dxf: 'share-started', nativeAudio: useNative }, '*'); } catch (e2) {}
      // Only report on the engine's audio when the engine was the one asked.
      // Under native capture there is nothing to explain, and a user who turned
      // audio off does not need to be told their share is silent.
      if (!useNative && wantAudio) {
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
  async function stopLocalShareAudio() {
    const at = localShareAudio;
    localShareAudio = null;
    if (!at) return;
    // Unpublish before stopping. A track stopped while still published leaves
    // the publication standing, so viewers keep an audio element attached to a
    // dead stream and their volume control goes on acting over silence.
    if (room) {
      try { await room.localParticipant.unpublishTrack(at, true); } catch (e) {}
    }
    try { at.stop(); } catch (e) {}
  }
  async function stopShare() {
    // Ours first, and unconditionally: `setScreenShareEnabled(false)` retracts
    // what the SDK created inside `setScreenShareEnabled(true)`, and this track
    // is not that — it was published by hand, out of the same getDisplayMedia
    // stream, so nothing else knows to take it down. Stopping it even with the
    // room already gone matters: the capture is a machine-level grant, and one
    // left running keeps the OS "sharing" indicator lit over a finished share.
    await stopLocalShareAudio();
    if (!room) return;
    try { await room.localParticipant.setScreenShareEnabled(false); } catch (e) {}
  }
  // --- Camera ---------------------------------------------------------------
  // Unlike the screen, the camera is captured in the webview on BOTH platforms,
  // and it publishes on this same connection: the bare-pubkey identity already
  // holds `can_publish`, and LiveKit tells a camera from a screen by
  // `TrackSource`, so no fourth identity and no extra token were needed.
  // Reject a promise that has taken too long, so a caller can report something
  // rather than waiting forever. `Promise.race` and not `AbortController`: the
  // SDK calls being wrapped take no signal.
  function withTimeout(promise, ms, message) {
    return Promise.race([
      promise,
      new Promise(function (_, reject) {
        setTimeout(function () { reject(new Error(message)); }, ms);
      }),
    ]);
  }
  function post(kind, extra) {
    const m = Object.assign({ __dxf: kind }, extra || {});
    try { window.postMessage(m, '*'); } catch (e) {}
  }
  function notifyCameraEnded() { post('camera-ended', {}); }
  function cameraPublishOpts(o) {
    const lk = LK();
    return {
      source: lk.Track.Source.Camera,
      // Not the screen-share preset table: a face should stay fluid and may go
      // soft, a screen is the reverse.
      videoEncoding: { maxBitrate: o.bitrate || 1200000, maxFramerate: o.fps || 30 },
      degradationPreference: 'maintain-framerate',
      // On, unlike the screen path. A camera is watched in a small tile far more
      // often than full size, and the low layer is what keeps a grid affordable.
      simulcast: true,
    };
  }
  // The local preview is a plain element over the raw stream, NOT a LiveKit
  // attach: it has to work before the room exists and keep working if
  // publishing fails, and it never goes near the SFU.
  function attachLocalCamera(cid) {
    const c = document.getElementById(cid || 'camera-self');
    if (!c || !localCameraStream) return;
    c.innerHTML = '';
    const el = document.createElement('video');
    el.srcObject = localCameraStream;
    el.muted = true; el.autoplay = true; el.playsInline = true;
    el.style.width = '100%'; el.style.height = '100%'; el.style.objectFit = 'cover'; el.style.background = '#000';
    // Mirrored, and only here: you expect your own reflection, and nobody
    // expects a mirrored stranger. Remote tiles must not carry this.
    el.style.transform = 'scaleX(-1)';
    c.appendChild(el);
  }
  function detachLocalCamera(cid) {
    const c = document.getElementById(cid || 'camera-self');
    if (c) c.querySelectorAll('video').forEach(function (e) { e.srcObject = null; e.remove(); });
  }
  async function startCamera(opts) {
    if (cameraStarting) return;
    cameraStarting = true;
    try { return await startCameraInner(opts || {}); } finally { cameraStarting = false; }
  }
  async function startCameraInner(opts) {
    if (!(navigator && navigator.mediaDevices && navigator.mediaDevices.getUserMedia)) {
      post('camera-unavailable', {});
      return;
    }
    if (localCameraTrack) await stopCamera();
    lastCameraOpts = opts;
    const base = { width: { ideal: opts.width || 1280 }, height: { ideal: opts.height || 720 }, frameRate: { ideal: opts.fps || 30 } };
    // `exact` on the deviceId so a stale saved id fails loudly instead of
    // silently opening a different camera; the next attempt drops it.
    const attempts = opts.deviceId
      ? [{ video: Object.assign({ deviceId: { exact: opts.deviceId } }, base) }, { video: base }, { video: true }]
      : [{ video: base }, { video: true }];
    let stream = null;
    for (let i = 0; i < attempts.length; i++) {
      try { stream = await navigator.mediaDevices.getUserMedia(attempts[i]); break; }
      catch (e) {
        if (isUserCancel(e)) { post('camera-denied', { name: String((e && e.name) || '') }); return; }
        if (i === attempts.length - 1) { post('camera-error', { detail: String((e && e.message) || e) }); return; }
      }
    }
    const vt = stream && stream.getVideoTracks()[0];
    if (!vt) { post('camera-error', { detail: 'no video track' }); return; }
    localCameraTrack = vt; localCameraStream = stream;
    // Preview now, before the room and before publishing, so the picture is up
    // while the connection is still settling.
    attachLocalCamera('camera-self');
    // Unplugged, or taken by another app. Same wiring the screen path uses for
    // the browser's own "Stop sharing" bar.
    vt.addEventListener('ended', function () { notifyCameraEnded(); });
    for (let i = 0; i < 150 && !room; i++) await new Promise(function (r) { setTimeout(r, 100); });
    if (!room) { await stopCamera(); post('camera-error', { detail: 'not connected to the stream room' }); return; }
    try {
      // Bounded, because nothing else bounds it. A publish that never settles
      // leaves the button lit and the label reading "Starting your camera…"
      // with no error anywhere — the failure reports itself as nothing at all,
      // which is the hardest kind to act on. Fifteen seconds is far beyond a
      // healthy publish and well short of a person's patience.
      await withTimeout(
        room.localParticipant.publishTrack(vt, cameraPublishOpts(opts)),
        15000,
        'publishing the camera track timed out'
      );
      const s = (function () { try { return vt.getSettings(); } catch (e) { return {}; } })();
      // Report what actually opened, not what was asked for, so a fallback does
      // not get persisted as the user's choice.
      post('camera-started', { deviceId: (s && s.deviceId) || '', label: vt.label || '' });
      // Labels only become readable once a grant exists, and this is the moment
      // one first does.
      listCameras();
    } catch (e) {
      await stopCamera();
      post('camera-error', { detail: String((e && e.message) || e) });
    }
  }
  async function stopCamera() {
    const vt = localCameraTrack;
    localCameraTrack = null; localCameraStream = null;
    detachLocalCamera('camera-self');
    if (!vt) return;
    // Unpublish before stopping, for the same reason stopLocalShareAudio does:
    // a stopped-but-published track leaves viewers attached to a dead stream.
    if (room) { try { await room.localParticipant.unpublishTrack(vt, true); } catch (e) {} }
    try { vt.stop(); } catch (e) {}
  }
  async function listCameras() {
    if (!(navigator && navigator.mediaDevices && navigator.mediaDevices.enumerateDevices)) { post('camera-devices', { devices: [] }); return; }
    try {
      const devs = await navigator.mediaDevices.enumerateDevices();
      // `label` is empty until a camera grant exists for this origin. The ids
      // still work, so an unlabelled list is offered rather than withheld and
      // the UI names them positionally until a grant arrives.
      post('camera-devices', {
        devices: devs.filter(function (d) { return d.kind === 'videoinput'; })
                     .map(function (d) { return { id: d.deviceId, label: d.label || '' }; }),
      });
    } catch (e) { post('camera-devices', { devices: [] }); }
  }
  try {
    navigator.mediaDevices.addEventListener('devicechange', function () { listCameras(); });
  } catch (e) {}

  async function disconnect() {
    desiredRoom = null;
    reconnectAttempt = 0;
    if (reconnectTimer) { clearTimeout(reconnectTimer); reconnectTimer = null; }
    // Before `room` is cleared, so the unpublish still has somewhere to go.
    await stopLocalShareAudio();
    // Leaving voice must put the camera light out. The capture is an OS-level
    // grant that outlives the room, so dropping the room is not enough.
    await stopCamera();
    const previous = room;
    room = null;
    if (previous) { try { await previous.disconnect(); } catch (e) {} }
    clearRemoteTracks();
    Object.keys(attached).forEach(detach);
  }
  return { connect: connect, attach: attach, detach: detach, requestAndStartShare: requestAndStartShare, stopShare: stopShare, disconnect: disconnect, setStreamVolume: setStreamVolume, setSink: setSink, setNativeStreamAudio: setNativeStreamAudio, startCamera: startCamera, stopCamera: stopCamera, listCameras: listCameras, attachLocalCamera: attachLocalCamera };
})();
"#;

/// Start/stop sharing — call from a click handler so `getDisplayMedia` runs in
/// a user gesture.
/// Screen-share quality presets, in the order the settings menu shows them.
/// `(id, label, hint)` — the hint is the one-line explanation under the select.
pub const QUALITY_PRESETS: &[(&str, &str, &str)] = &[
    (
        "smooth",
        "Smooth — 720p60",
        "Video and animation: stays fluid, softens when busy",
    ),
    (
        "balanced",
        "Balanced — 1080p30",
        "Good default for most sharing",
    ),
    (
        "crisp",
        "Crisp — 1080p15",
        "Sharpest text; drops frames when the screen is busy",
    ),
    (
        "ultra",
        "Ultra — 1440p30",
        "High detail; needs strong upload",
    ),
];

/// Screen content is mostly static text, so resolution matters far more than
/// framerate for legibility — the "crisp" preset deliberately trades fps for
/// pixels. Bitrates are well above LiveKit's screen-share defaults because the
/// default is tuned for slide decks, not code editors.
/// `(width, height, fps, max_bitrate, content_hint, degradation_preference)`.
///
/// The last two are the ones that decide what gives way when the scene gets
/// busy, and they must follow the preset's promise. Every preset used to get
/// `detail` + `maintain-resolution`, which means "keep the pixels, drop the
/// frames" — so "Smooth — best for video and animation" stuttered exactly like
/// "Crisp — sharpest text" did. The labels were describing intentions the
/// encoder was never told about.
///
/// - `maintain-framerate` scales the picture down and keeps motion fluid.
/// - `maintain-resolution` keeps every pixel and sacrifices frames.
/// - `balanced` lets WebRTC give a little of each.
///
/// Bitrates are ceilings, not targets: congestion control still limits what a
/// weak uplink actually sends, so a higher cap only helps connections that can
/// use it — and headroom is what stops a busy scene having to degrade at all.
fn quality_preset(id: &str) -> (u32, u32, u32, u32, &'static str, &'static str) {
    match id {
        // Video and animation: fluid motion is the whole point.
        "smooth" => (1280, 720, 60, 6_000_000, "motion", "maintain-framerate"),
        // Text: every pixel matters, frames do not.
        "crisp" => (1920, 1080, 15, 6_000_000, "detail", "maintain-resolution"),
        // Lots of detail, but not at the cost of collapsing to a slideshow.
        "ultra" => (2560, 1440, 30, 14_000_000, "detail", "balanced"),
        // "balanced" and anything unrecognised (older config, typo).
        _ => (1920, 1080, 30, 9_000_000, "motion", "balanced"),
    }
}

/// How far native (Rust-side) system-audio capture reaches on this platform,
/// as the tag the JS controller understands.
///
/// Deliberately *not* a decision — only the JS side can make one, because
/// `MonitorOnly` turns on which surface the user picked and nothing knows that
/// until the picker closes. See `sysaudio::NativeScope`.
fn native_audio_mode() -> &'static str {
    match crate::sysaudio::scope() {
        crate::sysaudio::NativeScope::Always => "always",
        crate::sysaudio::NativeScope::MonitorOnly => "monitor",
        crate::sysaudio::NativeScope::Never => "never",
    }
}

/// The capture settings a quality preset means, for the *native* path.
///
/// Reads the same preset table as the webview path so "Crisp" is the same
/// promise on both. Framerate/bitrate carry over directly; the content hint and
/// degradation preference do not, because native publishing expresses that
/// through libwebrtc's `is_screencast` flag rather than per-track WebRTC hints.
pub fn native_settings(quality: &str) -> crate::sysvideo::Settings {
    let (width, height, fps, bitrate, _hint, _degradation) = quality_preset(quality);
    crate::sysvideo::Settings {
        width,
        height,
        fps,
        max_bitrate: bitrate as u64,
    }
}

/// `quality` and `audio` apply to starting a share; stopping ignores both.
pub fn share_js(on: bool, quality: &str, audio: bool) -> String {
    // When starting, call the user-gesture variant that prompts getDisplayMedia
    // before delegating to the LiveKit flow. When stopping, just stopShare.
    if !on {
        return format!("{SCREEN_JS}\nwindow.dxScreen.stopShare();");
    }
    let (w, h, fps, bitrate, hint, degradation) = quality_preset(quality);
    let mode = native_audio_mode();
    format!(
        "{SCREEN_JS}\nwindow.dxScreen.requestAndStartShare({{width:{w},height:{h},fps:{fps},\
         bitrate:{bitrate},hint:'{hint}',degradation:'{degradation}',audio:{audio},nativeAudio:'{mode}'}});"
    )
}

/// Point a container at a publisher's video. `kind` is `"screen"` or
/// `"camera"` — the same participant can be publishing both, so the container
/// has to say which one it wants.
pub(crate) fn attach_js(identity: &str, container: &str, kind: &str) -> String {
    format!("{SCREEN_JS}\nwindow.dxScreen.attach('{identity}','{container}','{kind}');")
}

pub(crate) fn detach_js(container: &str) -> String {
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
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let token = use_memo(move || state.read().screen_token.clone());
    let mut last = use_signal(|| None::<(String, String)>);

    use_effect(move || {
        let t = token();
        if t != *last.peek() {
            match &t {
                Some((url, tok)) => {
                    // The same passphrase the native rooms use. Quoted through
                    // serde rather than interpolated: it is user-supplied and
                    // has no business being pasted into a JS literal.
                    let key = serde_json::to_string(&crate::e2ee::shared_key())
                        .unwrap_or_else(|_| "null".into());
                    let _ = document::eval(&format!(
                        "{SCREEN_JS}\nwindow.dxScreen.connect('{url}','{tok}',{key});"
                    ));
                }
                None => {
                    let _ = document::eval(&format!("{SCREEN_JS}\nwindow.dxScreen.disconnect();"));
                }
            }
            last.set(t);
        }
    });

    // Who plays stream audio: the native mixer (so it follows the output device
    // like voice) or the webview.
    //
    // Keyed on the room being *joined*, not on a token existing. Standing the
    // webview down on the token alone meant a join that failed — or a room that
    // later dropped — left nobody playing: silence, with a volume slider that
    // still looked live and no way back short of rejoining voice. On this, the
    // webview picks the audio back up, on the system default device. That is
    // the behaviour this feature replaces, and it beats silence.
    let native_audio_live = use_memo(move || state.read().screen_audio_joined);
    use_effect(move || {
        let on = native_audio_live();
        // `true` here means the webview has been told to stop playing stream
        // audio and unsubscribe from it, leaving the native mixer in charge.
        crate::dlog!("screen setNativeStreamAudio({on})");
        let _ = document::eval(&format!(
            "{SCREEN_JS}\nwindow.dxScreen.setNativeStreamAudio({on});"
        ));
    });

    // Join the screen room's audio as soon as somebody *else* is sharing in our
    // channel, rather than waiting for the watch window to open: connecting a
    // room costs about a second, and paying it when the user clicks means the
    // first second of every stream is silent.
    //
    // Our own share is excluded from the trigger — there would be nothing to
    // subscribe to, since the room skips our own publications.
    // Keyed on the voice session as well as the token, not the token alone. A
    // device change tears `ActiveVoice` down and rebuilds it, and the rebuilt
    // session starts with no screen-audio room; keyed only on the token, this
    // guard would see an unchanged value and stay silent, so stream audio would
    // die for the rest of the share — with the webview fallback already
    // disabled and the volume slider still looking live.
    //
    // The same restart drops our own system-audio publication, which has
    // nothing else to re-trigger it, so that is re-issued here too. Only when
    // this share is actually the native-capture kind: on Windows a window pick
    // deliberately leaves its audio to the engine, and re-publishing blindly
    // would send the whole machine for a share scoped to one window.
    // `joined` is in the key so a room that drops re-fires this exactly once:
    // the flag flips false, the tuple changes, one rejoin is attempted. If that
    // rejoin fails the flag is already false, the tuple no longer changes, and
    // nothing repeats — which is why the guard keys on the flag rather than
    // being skipped on failure. This effect runs on *every* `AppState` change,
    // so "retry until it works" would mean a fresh connection attempt per
    // arriving message against an SFU that is already down.
    // Natively captured *video* is re-issued from here for exactly the same
    // reason, and it matters more: a voice-session restart drops the publisher
    // room along with everything else on `ActiveVoice`, and where the native
    // path is the only capture path there is no webview track still running
    // underneath. Left to the button alone, changing your microphone mid-share
    // would end the share and light the button as if it hadn't.
    let voice_screen_audio = use_voice_tx();
    #[allow(clippy::type_complexity)]
    let mut last_sent = use_signal(|| {
        None::<(
            u64,
            Option<(String, String)>,
            bool,
            bool,
            Option<(String, String)>,
            Option<crate::sysvideo::Target>,
        )>
    });
    use_effect(move || {
        let s = state.read();
        let self_pk = s.self_user.as_ref().map(|u| u.pubkey.as_str());
        let others_sharing = s
            .voice
            .channel_id
            .and_then(|cid| s.screen_shares.get(&cid))
            .is_some_and(|sharers| sharers.iter().any(|pk| Some(pk.as_str()) != self_pk));
        let want = if others_sharing {
            s.screen_audio_token.clone()
        } else {
            None
        };
        let publish_system = s.screen_sharing && s.screen_native_audio;
        let epoch = s.voice_session_epoch;
        let joined = s.screen_audio_joined;
        // Only where the native path is the capture path, and only once a
        // surface has been chosen. On Windows the webview holds the video track
        // and this must stay None, or the same screen would be published twice
        // under two identities.
        let target = s.screen_share_target;
        let want_video = match (s.screen_sharing && crate::sysvideo::supported(), target) {
            (true, Some(_)) => s.screen_video_token.clone(),
            _ => None,
        };
        drop(s);
        // The preset only matters on the transition that starts a capture; a
        // change while sharing is picked up by the next share, same as the
        // webview path (which bakes it into the `getDisplayMedia` constraints).
        let quality = settings.read().screenshare_quality.clone();

        // The target is in the key so switching surface mid-share re-fires this
        // exactly once, and the publisher rebuilds against the new one.
        let now = (epoch, want, publish_system, joined, want_video, target);
        if last_sent.peek().as_ref() != Some(&now) {
            voice_screen_audio.send(VoiceCmd::SetScreenAudio {
                room: now.1.clone(),
            });
            voice_screen_audio.send(VoiceCmd::SetSystemAudio {
                enabled: now.2,
                target,
            });
            voice_screen_audio.send(VoiceCmd::SetScreenVideo {
                room: now.4.clone(),
                // Stopping ignores the target, so the fallback is never used for
                // anything — it just avoids making the command's type optional
                // for a field only the start path reads.
                target: target.unwrap_or(crate::sysvideo::Target::Display(0)),
                settings: native_settings(&quality),
            });
            last_sent.set(Some(now));
        }
    });

    // Can this build capture a screen at all, and by which path?
    //
    // Where `sysvideo` has a native backend the webview is not consulted: on
    // macOS it has no `navigator.mediaDevices` whatsoever, so probing it would
    // only ever produce a false negative and a toast recommending a Windows fix
    // to a Mac user. That is exactly what it used to do.
    use_future(move || {
        let mut s = state;
        async move {
            if crate::sysvideo::supported() {
                s.write().screen_capture_available = true;
                return;
            }
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
                        "Screen sharing isn't available in this build's webview. On Windows, \
                         installing the WebView2 runtime enables it."
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
    use_future(move || {
        let mut state = state;
        let gateway = gateway_end.clone();
        async move {
            // Sink reassigned per eval, listener registered once — see the
            // matching comment in `features::camera`. The screen pump has the
            // same remount hazard and would have gone deaf in the same way,
            // silently, after the first reconnect.
            let bridge_js = r#"
            window.__dxfShareSink = function (m) { try { dioxus.send(m); } catch (err) {} };
            if (!window.__dxfShareEndWired) {
              window.__dxfShareEndWired = true;
              window.addEventListener('message', function (e) {
                var d = e.data;
                if (d && (d.__dxf === 'screen-share-ended' || d.__dxf === 'share-started' || d.__dxf === 'share-audio' || d.__dxf === 'share-unavailable' || d.__dxf === 'share-echo-risk' || d.__dxf === 'stream-audio' || d.__dxf === 'screen-room-error' || d.__dxf === 'screen-room-reconnecting' || d.__dxf === 'screen-track-timeout' || d.__dxf === 'e2ee-error') && window.__dxfShareSink) {
                  window.__dxfShareSink(d);
                }
              });
            }
            "#;
            let mut eval = document::eval(bridge_js);
            while let Ok(msg) = eval.recv::<Value>().await {
                match msg.get("__dxf").and_then(|v| v.as_str()) {
                    // Publishing succeeded — only now is this a share.
                    Some("share-started") => {
                        let cid = state.read().voice.channel_id;
                        // Where we capture system audio ourselves, it rides
                        // along on the voice room as a second track. Whether
                        // that applies to *this* share was decided after the
                        // picker, so it arrives on the message rather than
                        // being recomputed from the platform here — and it is
                        // recorded rather than acted on, because a voice
                        // reconnect later has to know whether to re-publish.
                        {
                            let mut w = state.write();
                            w.screen_sharing = true;
                            w.screen_native_audio =
                                msg.get("nativeAudio").and_then(|v| v.as_bool()) == Some(true);
                        }
                        if let Some(c) = cid {
                            gateway.send(ClientMessage::SetScreenShare {
                                channel_id: c,
                                sharing: true,
                            });
                        }
                    }
                    // The click found no capture API at all.
                    Some("share-unavailable") => {
                        let secure = msg.get("secure").and_then(|v| v.as_bool()).unwrap_or(false);
                        eprintln!("[screen] share unavailable (secure_context={secure})");
                        let mut w = state.write();
                        w.screen_capture_available = false;
                        w.screen_sharing = false;
                        w.error_toast = Some(if secure {
                            "This webview has no screen-capture support.".into()
                        } else {
                            "Screen sharing can't start: this window isn't a secure context, \
                                 so the capture API is hidden. Please report this."
                                .into()
                        });
                    }
                    // Whole-screen capture with sound: the mix includes us.
                    Some("share-echo-risk") => {
                        eprintln!("[screen] whole-screen audio capture — echo risk");
                        state.write().error_toast = Some(
                            "Sharing a whole screen with sound also captures this call, so \
                                 others may hear themselves echo. Share a single window instead \
                                 to send only that app's audio."
                                .into(),
                        );
                    }
                    Some("screen-share-ended") => {
                        let cid = state.read().voice.channel_id;
                        {
                            let mut w = state.write();
                            w.screen_sharing = false;
                            w.screen_native_audio = false;
                        }
                        if let Some(c) = cid {
                            gateway.send(ClientMessage::SetScreenShare {
                                channel_id: c,
                                sharing: false,
                            });
                        }
                        // Stopping the publication is left to the effect
                        // above, which owns that decision for both the start
                        // and the restart cases — two senders racing over one
                        // track is how the state they disagree about gets
                        // decided by arrival order.
                    }
                    Some("screen-room-error") => {
                        let detail = msg
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown connection error");
                        eprintln!("[screen] screen room connection failed: {detail}");
                        state.write().error_toast = Some(format!(
                            "Couldn't connect to the screen stream: {detail}. Retrying…"
                        ));
                    }
                    Some("screen-room-reconnecting") => {
                        let detail = msg
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .unwrap_or("disconnected");
                        eprintln!("[screen] screen room disconnected: {detail}; reconnecting");
                    }
                    Some("screen-track-timeout") => {
                        eprintln!("[screen] no video track arrived within 10 seconds");
                        state.write().error_toast = Some(
                                "Connected to the stream room, but no video arrived. Make sure the sharer is running the latest Discordia build and restart the share."
                                    .into(),
                            );
                    }
                    // Encryption was asked for and could not be set up. Loud,
                    // because the alternative is a call that connects and
                    // carries nothing anyone can decode — the failure mode
                    // end-to-end encryption has instead of an error.
                    Some("e2ee-error") => {
                        let detail = msg
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown error");
                        eprintln!("[screen] e2ee setup failed: {detail}");
                        state.write().error_toast = Some(format!(
                            "Encryption could not be enabled for this call ({detail}). \
                             Others will not be able to hear or see you."
                        ));
                    }
                    // Our own share: did the platform give us any audio to
                    // send? Silence here is a platform limit, not a bug we
                    // can fix client-side, so say so rather than let the
                    // sharer assume viewers can hear their machine.
                    Some("share-audio") => {
                        let published = msg
                            .get("published")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        // Whether the platform *can* do it at all, versus
                        // whether this particular pick included it. Saying
                        // "your platform can't" when the user simply left
                        // the checkbox unticked — or picked a window, which
                        // never carries audio — is just wrong.
                        let supported = msg
                            .get("supported")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        eprintln!(
                            "[screen] share audio published={published} supported={supported}"
                        );
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
                        let present = msg
                            .get("present")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if let Some(id) = msg.get("identity").and_then(|v| v.as_str()) {
                            crate::dlog!(
                                "[screen] watching {}: audio={present}",
                                &id[..id.len().min(8)]
                            );
                            let mut s = state.write();
                            if present {
                                s.stream_has_audio.insert(id.to_string());
                            } else {
                                s.stream_has_audio.remove(id);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    });

    rsx! { Fragment {} }
}

/// Picker for the native capture path: choose a screen, an app, or one window.
///
/// The webview path never needed this — Chromium's `getDisplayMedia` opens the
/// OS picker itself. ScreenCaptureKit has no picker, only an enumeration API, so
/// this is the UI half of what Chromium was providing. (Discord solves the same
/// problem the same way: Electron's `desktopCapturer` enumerates, and the grid
/// you see is Discord's own.)
///
/// Driven by `AppState.screen_picker`: `None` closed, `Some(Ok(..))` a list,
/// `Some(Err(..))` the reason there isn't one — usually Screen Recording
/// permission having been refused, which is worth saying rather than showing an
/// empty list.
#[component]
pub fn ScreenSourcePicker() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let picker = use_memo(move || state.read().screen_picker.clone());
    let Some(result) = picker() else {
        return rsx! {};
    };

    let close = move |_| {
        state.write().screen_picker = None;
    };

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: close,
            div {
                class: "dxf-modal-in w-[30rem] max-h-[80vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Share your screen" }
                    button {
                        class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        onclick: close,
                        "✕"
                    }
                }
                match result {
                    Err(e) => rsx! {
                        div { class: "p-4 space-y-2",
                            div { class: "text-xs text-[var(--danger)]", "{e}" }
                            div { class: "text-[11px] text-[var(--text-muted)] leading-relaxed",
                                "Screen sharing needs the Screen Recording permission. Grant it in \
                                 System Settings › Privacy & Security › Screen & System Audio \
                                 Recording, then quit and reopen Discordia — macOS only re-reads it \
                                 on launch. If macOS offers to \"Quit & Reopen\" and Discordia does \
                                 not come back, that is Gatekeeper refusing the relaunch, not a \
                                 failed grant: just launch it again yourself."
                            }
                        }
                    },
                    Ok(sources) if sources.is_empty() => rsx! {
                        div { class: "p-4 text-xs text-[var(--text-muted)]", "Looking for screens…" }
                    },
                    Ok(sources) => {
                        // Displays and app entries carry no `app`, individual
                        // windows do — which is exactly the split the two
                        // headings want, and it preserves the order the backend
                        // already chose.
                        let (surfaces, windows): (Vec<_>, Vec<_>) =
                            sources.into_iter().partition(|s| s.app.is_none());
                        rsx! {
                            div { class: "flex-1 overflow-y-auto p-3 space-y-3",
                                if !surfaces.is_empty() {
                                    div {
                                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                                            "Screens & apps"
                                        }
                                        div { class: "space-y-1",
                                            for s in surfaces.into_iter() {
                                                {
                                                    let target = s.target;
                                                    let g = gateway.clone();
                                                    let dims = (s.width > 0).then(|| format!("{}×{}", s.width, s.height));
                                                    rsx! {
                                                        button {
                                                            key: "{s.title}",
                                                            class: "w-full text-left px-2 py-1.5 rounded border border-[var(--border)] hover:border-[var(--accent)] hover:bg-[var(--accent-soft)] transition-colors flex items-baseline gap-2",
                                                            onclick: move |_| choose_source(state, g.clone(), settings, target),
                                                            span { class: "text-xs text-[var(--text)] flex-1 truncate", "{s.title}" }
                                                            if let Some(d) = dims {
                                                                span { class: "text-[10px] text-[var(--text-dim)] font-mono shrink-0", "{d}" }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                if !windows.is_empty() {
                                    div {
                                        div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5",
                                            "Windows"
                                        }
                                        div { class: "space-y-1",
                                            for s in windows.into_iter() {
                                                {
                                                    let target = s.target;
                                                    let g = gateway.clone();
                                                    let app = s.app.clone().unwrap_or_default();
                                                    rsx! {
                                                        button {
                                                            key: "{app}/{s.title}",
                                                            class: "w-full text-left px-2 py-1.5 rounded border border-[var(--border)] hover:border-[var(--accent)] hover:bg-[var(--accent-soft)] transition-colors flex items-baseline gap-2",
                                                            onclick: move |_| choose_source(state, g.clone(), settings, target),
                                                            span { class: "text-[10px] text-[var(--accent)] shrink-0 max-w-[7rem] truncate", "{app}" }
                                                            span { class: "text-xs text-[var(--text)] flex-1 truncate", "{s.title}" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Commit to sharing `target`.
///
/// A free function rather than a closure in the picker because every row needs
/// its own handler and a closure capturing `GatewayTx` is not `Copy`.
///
/// Starting the share is the picker's job, not the button's: the button opens
/// the picker, and until something is chosen there is nothing to announce. Same
/// reason the webview path announces only once a track exists — a share
/// cancelled at the picker never happened.
fn choose_source(
    mut state: Signal<crate::state::AppState>,
    gateway: crate::state::GatewayTx,
    settings: Signal<crate::settings::ClientSettings>,
    target: crate::sysvideo::Target,
) {
    let with_audio = settings.read().screenshare_audio;
    let (channel, already_sharing) = {
        let s = state.read();
        (s.voice.channel_id, s.screen_sharing)
    };
    {
        let mut s = state.write();
        s.screen_share_target = Some(target);
        s.screen_sharing = true;
        s.screen_native_audio = with_audio;
        s.screen_picker = None;
    }
    // Only on the transition into sharing. Re-announcing while already live
    // (switching surface mid-share) would tell the channel about a share it
    // already knows about.
    if !already_sharing && let Some(cid) = channel {
        gateway.send(ClientMessage::SetScreenShare {
            channel_id: cid,
            sharing: true,
        });
    }
}

/// Enumerate shareable surfaces and open the picker.
///
/// The query is blocking and can sit behind the Screen Recording prompt for as
/// long as the user takes, so it runs on a blocking thread rather than stalling
/// the UI. The picker opens immediately in a loading state so the click feels
/// answered.
pub fn open_screen_picker(mut state: Signal<crate::state::AppState>) {
    state.write().screen_picker = Some(Ok(Vec::new()));
    dioxus::prelude::spawn(async move {
        let found = tokio::task::spawn_blocking(crate::sysvideo::sources)
            .await
            .unwrap_or_else(|e| Err(format!("listing screens failed: {e}")));
        // Only if the picker is still open. A user who closed it in the second
        // the query took should not have it reappear underneath them.
        if state.peek().screen_picker.is_some() {
            state.write().screen_picker = Some(found);
        }
    });
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

    // Native path only: what is being shared, and proof that it is moving.
    let native_capture = crate::sysvideo::supported();
    let share_label = use_memo(move || match state.read().screen_share_target {
        Some(crate::sysvideo::Target::Display(_)) => "Sharing your screen",
        Some(crate::sysvideo::Target::Window(_)) => "Sharing one window",
        Some(crate::sysvideo::Target::Application(_)) => "Sharing an app",
        None => "Sharing your screen",
    });
    // Polled rather than pushed: the counter is bumped on the OS capture thread
    // at up to 60 Hz, and turning each of those into a state write would re-render
    // the whole workspace per frame. Twice a second is plenty to show "moving".
    let mut frames = use_signal(|| 0_u64);
    use_future(move || async move {
        loop {
            frames.set(crate::sysvideo::frames_captured());
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    });

    // Attach our local preview when sharing turns on.
    let mut last = use_signal(|| false);
    use_effect(move || {
        let sh = sharing();
        if sh != *last.peek() {
            if sh && let Some(pk) = self_pk() {
                let _ = document::eval(&attach_js(&pk, "screenshare-self", "screen"));
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
                        {
                            let mut w = state.write();
                            w.screen_sharing = false;
                            // Forget the surface too, so the next share opens the
                            // picker instead of silently resuming the last one.
                            // The effect that owns publishing keys on this, so
                            // clearing it is also what stops the capture.
                            w.screen_share_target = None;
                            w.screen_native_audio = false;
                        }
                        // Only the webview path has a JS track to stop; on the
                        // native path this is a no-op against a controller that
                        // never started a capture.
                        if !crate::sysvideo::supported() {
                            // Quality and audio only matter when starting a share.
                            let _ = document::eval(&share_js(false, "", true));
                        }
                        if let Some(c) = cid {
                            gateway.send(ClientMessage::SetScreenShare { channel_id: c, sharing: false });
                        }
                    },
                    "Stop"
                }
            }
            // On the native macOS path the publisher is `{pubkey}#video`, while
            // this webview is the distinct bare `{pubkey}` participant. It can
            // therefore subscribe exactly like a remote viewer, and the JS
            // controller already resolves that suffix. This preview shows what
            // actually crossed the wire rather than only the local source.
            div {
                id: "screenshare-self",
                class: "relative flex-1 min-h-0 bg-black flex items-center justify-center text-[var(--text-dim)] text-[10px]",
                "Starting…"
                if native_capture {
                    div {
                        class: "absolute left-2 bottom-2 px-1.5 py-0.5 rounded bg-black/70 text-[9px] pointer-events-none",
                        if frames() > 0 {
                            span { class: "text-[var(--up)]", "{share_label} · {frames()} frames sent" }
                        } else {
                            span { class: "text-[var(--warn)]", "{share_label} · waiting for first frame…" }
                        }
                    }
                }
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
/// Drag state for the floating windows. `pub(crate)` so `features::camera`'s
/// two windows reuse it rather than declaring a third identical copy.
pub(crate) enum Drag {
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
                    //
                    // Deafen is read here and not left to the effect below,
                    // which cannot cover this: switching between two sharers
                    // who are both at the default level leaves its memo at the
                    // same value, so it never re-runs and this eval is the last
                    // word. That is the common case, not a corner.
                    let s = state.read();
                    let gain = if s.voice.deafened {
                        0.0
                    } else {
                        s.stream_gain_of(pk)
                    };
                    let js = format!(
                        "{}\n{}",
                        stream_volume_js(gain),
                        attach_js(pk, "screenshare-viewer", "screen"),
                    );
                    drop(s);
                    let _ = document::eval(&js);
                }
                None => {
                    // The webview half of the teardown. `detach` also runs
                    // `detachAudio`, so if this line is present and audio still
                    // plays, the webview element is not the one playing it.
                    crate::dlog!("watch detach (viewer closed)");
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
    // What was last actually sent, so an unchanged answer costs nothing.
    //
    // This effect reads `AppState`, so it re-runs on *every* mutation — every
    // arriving message and every 150ms `mic_level` tick. Without this guard it
    // re-sent identical gains thousands of times per call, and each send takes
    // `stream_gains.lock()` — the same mutex the realtime cpal output callback
    // locks in `refresh_gains`. That contention made the callback miss its
    // deadline, so call audio audibly degraded for as long as anyone was
    // sharing. Recomputing is cheap; *sending* is what had to stop.
    let mut last_gains = use_signal(Vec::<(String, f32)>::new);
    use_effect(move || {
        let watched = watching();
        let _ = stream_levels();
        let s = state.read();
        // Every known sharer gets an explicit gain: whoever we're watching at
        // their chosen level, everyone else at zero.
        let mut seen: Vec<String> = s.screen_shares.values().flatten().cloned().collect();
        if let Some(w) = watched.clone()
            && !seen.contains(&w)
        {
            seen.push(w);
        }
        let mut desired: Vec<(String, f32)> = seen
            .into_iter()
            .map(|pk| {
                let gain = if Some(&pk) == watched.as_ref() {
                    s.stream_gain_of(&pk)
                } else {
                    0.0
                };
                (pk, gain)
            })
            .collect();
        drop(s);
        // Sorted so a reordering of the underlying map can't read as a change.
        desired.sort_by(|a, b| a.0.cmp(&b.0));
        if *last_gains.peek() == desired {
            return;
        }
        crate::dlog!(
            "watch gains changed watched={:?} gains={:?}",
            watched.as_ref().map(|w| &w[..w.len().min(8)]),
            desired
                .iter()
                .map(|(p, g)| (&p[..p.len().min(8)], g))
                .collect::<Vec<_>>()
        );
        for (pk, gain) in desired.iter() {
            voice_for_stream.send(VoiceCmd::SetStreamVolume {
                pubkey: pk.clone(),
                gain: *gain,
            });
        }
        last_gains.set(desired);
    });

    // Follow the output device chosen in audio settings.
    let output_device = use_memo(move || state.read().selected_output_device.clone());
    use_effect(move || {
        let _ = document::eval(&stream_sink_js(output_device().as_deref()));
    });

    // Deafen has to reach the webview too. When the native `#audio` join isn't
    // live — old server, failed join, dropped room — stream audio plays through
    // an HTMLMediaElement instead of our mixer, where the gate in
    // `refresh_gains` cannot reach it, and a deafen that doesn't deafen is the
    // worst way for this to fail.
    //
    // This covers toggling deafen while already watching. Changing *who* you
    // watch is the attach effect's, which re-sends the volume in the same eval
    // as the attach and so has to apply the flag itself — an unchanged `Memo`
    // does not wake its subscribers, and that is also what keeps this effect
    // off the `last_gains` treadmill above rather than firing on every
    // `AppState` mutation.
    let deafened = use_memo(move || state.read().voice.deafened);
    let watched_gain = use_memo(move || {
        let s = state.read();
        s.screen_viewing
            .as_ref()
            .map(|pk| s.stream_gain_of(pk))
            .unwrap_or(0.0)
    });
    use_effect(move || {
        let gain = if deafened() { 0.0 } else { watched_gain() };
        let _ = document::eval(&stream_volume_js(gain));
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
    // Reported by whichever side is playing the audio — normally the voice
    // service, which subscribes to every screen-audio track.
    let has_audio = state.read().stream_has_audio.contains(&pk);
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
