use dioxus::prelude::*;
use serde_json::Value;

use crate::features::voice::{VoiceCmd, use_voice_tx};
use crate::protocol::ClientMessage;
use crate::state::{use_app_state, use_gateway};

/// `screen-…` is a misnomer since the camera moved onto this same room and
/// connection. The name is load-bearing on the wire, so it stays.
pub(crate) const SCREEN_JS: &str = r#"
window.dxScreen = window.dxScreen || (function () {
  let room = null;
  let desiredRoom = null;
  let reconnectTimer = null;
  let reconnectAttempt = 0;
  let localShareAudio = null;
  let e2eeKey = null;
  let e2eeWorker = null;
  let e2eeOn = false;
  let e2eeProvider = null;
  let localCameraTrack = null;
  let localCameraStream = null;
  let lastCameraOpts = {};
  let cameraStarting = false;
  const tracks = {};
  const audioTracks = {};
  function trackKey(id, kind) { return id + '|' + kind; }
  function kindOf(pub, track) {
    const s = (pub && pub.source) || (track && track.source) || '';
    return s === 'camera' ? 'camera' : 'screen';
  }
  const attached = {};
  const LK = () => window.LivekitClient || window.LiveKitClient;

  let nativeStreamAudio = false;
  function setNativeStreamAudio(on) {
    const was = nativeStreamAudio;
    nativeStreamAudio = !!on;
    if (was === nativeStreamAudio) return;
    if (nativeStreamAudio) { detachAudio(); applyAudioSubscriptions(); }
    else { applyAudioSubscriptions(); attachWatched(); }
  }
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
  function attachWatched() {
    const c = document.getElementById('screenshare-viewer');
    const id = c && c.getAttribute('data-identity');
    if (id) attachAudio(id);
  }
  let audioEl = null;
  let audioIdentity = null;
  let pendingGain = 1;
  let sinkLabel = null;
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
    pendingGain = Math.max(0, Math.min(1, v));
    const t = audioIdentity ? audioTracks[audioIdentity] : null;
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
      audioEl.style.display = 'none';
      document.body.appendChild(audioEl);
      audioIdentity = identity;
      t.setVolume(pendingGain);
      applySink();
      if (room && room.canPlaybackAudio === false) {
        room.startAudio().catch(function (e) { console.warn('[dxScreen] startAudio blocked', e); });
      }
      report(identity, true);
    } catch (e) {
      console.warn('[dxScreen] stream audio attach failed', e);
      report(identity, false);
    }
  }
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
    el.style.objectFit = 'contain';
    c.appendChild(el);
    attached[cid] = { identity: identity, kind: kind, track: track, el: el };
  }
  const VIDEO_SUFFIX = '#video';
  function baseIdentity(id) {
    return id.endsWith(VIDEO_SUFFIX) ? id.slice(0, -VIDEO_SUFFIX.length) : id;
  }
  function videoTrackFor(id, kind) {
    if (kind === 'camera') return tracks[trackKey(id, 'camera')];
    return tracks[trackKey(id, 'screen')] || tracks[trackKey(id + VIDEO_SUFFIX, 'screen')];
  }
  function reattach(identity, kind) {
    const base = baseIdentity(identity);
    Object.keys(attached).forEach(function (cid) {
      const a = attached[cid];
      if (!a || a.identity !== base || a.kind !== kind) return;
      const c = document.getElementById(cid);
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
      if (a) { a.track = null; a.el = null; }
    });
  }
  function scheduleReconnect() {
    if (!desiredRoom || reconnectTimer) return;
    const delay = Math.min(1500 * Math.pow(2, reconnectAttempt), 15000);
    reconnectAttempt++;
    reconnectTimer = setTimeout(function () {
      reconnectTimer = null;
      if (desiredRoom && !room) connect(desiredRoom.url, desiredRoom.token, desiredRoom.key, desiredRoom.e2ee);
    }, delay);
  }
  async function connect(url, token, key, encrypt) {
    e2eeKey = key || null;
    e2eeOn = !!encrypt;
    const same = desiredRoom && desiredRoom.url === url && desiredRoom.token === token;
    desiredRoom = { url: url, token: token, key: key || null, e2ee: e2eeOn };
    if (room) {
      if (same) return;
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
    const opts = { adaptiveStream: true, dynacast: true };
    if (e2eeOn && !window.__dxfE2eeWorkerSrc) {
      console.error('[dxScreen] e2ee requested but the worker source is missing');
      post('e2ee-error', { detail: 'the encryption worker was not injected' });
    } else if (e2eeOn) {
      try {
        const provider = new lk.ExternalE2EEKeyProvider();
        e2eeWorker = new Worker(
          URL.createObjectURL(
            new Blob([window.__dxfE2eeWorkerSrc], { type: 'application/javascript' })
          )
        );
        opts.e2ee = { keyProvider: provider, worker: e2eeWorker };
        e2eeProvider = provider;
      } catch (e) {
        e2eeProvider = null;
        console.error('[dxScreen] could not set up e2ee', e);
        post('e2ee-error', { detail: String((e && e.message) || e) });
      }
    } else {
      e2eeProvider = null;
    }
    const thisRoom = new lk.Room(opts);
    room = thisRoom;
    thisRoom.on(lk.RoomEvent.EncryptionError, function (err) {
      console.error('[dxScreen] encryption error', err);
      post('e2ee-undecryptable', { detail: String((err && err.message) || err) });
    });
    if (e2eeProvider) {
      try {
        if (e2eeKey) {
          postRawKey(e2eeKey);
          await thisRoom.setE2EEEnabled(true);
        } else {
          await thisRoom.setE2EEEnabled(false);
        }
      } catch (e) {
        console.error('[dxScreen] enabling e2ee failed', e);
        post('e2ee-error', { detail: String((e && e.message) || e) });
      }
    }
    thisRoom.on(lk.RoomEvent.Disconnected, function (reason) {
      console.warn('[dxScreen] room disconnected', reason);
      if (room !== thisRoom) return;
      room = null;
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
    thisRoom.on(lk.RoomEvent.TrackPublished, function (pub) {
      applyAudioSubscription(pub);
    });
    thisRoom.on(lk.RoomEvent.TrackSubscribed, function (track, pub, participant) {
      if (track.kind === 'audio') {
        audioTracks[participant.identity] = track;
        if (nativeStreamAudio) return;
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
    applyAudioSubscriptions();
    if (localCameraTrack && localCameraTrack.readyState !== 'ended') {
      try {
        await thisRoom.localParticipant.publishTrack(localCameraTrack, cameraPublishOpts(lastCameraOpts));
      } catch (e) {
        console.warn('[dxScreen] camera republish failed', e);
        notifyCameraEnded();
      }
    }
  }
  function isUserCancel(e) {
    const n = e && e.name;
    return n === 'NotAllowedError' || n === 'AbortError' || n === 'SecurityError';
  }
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
      if ((tries || 0) < 20) setTimeout(function () { attach(identity, cid, kind, (tries || 0) + 1); }, 50);
      return;
    }
    c.setAttribute('data-identity', identity);
    attached[cid] = { identity: identity, kind: kind, track: null, el: null };
    const t = videoTrackFor(identity, kind);
    if (t) attachInto(t, c, cid, identity, kind); else c.querySelectorAll('video').forEach(function (e) { e.remove(); });
    if (!t && kind === 'screen') {
      setTimeout(function () {
        const current = document.getElementById(cid);
        if (current && current.getAttribute('data-identity') === identity && !videoTrackFor(identity, 'screen')) {
          reportRoomProblem('screen-track-timeout', identity);
        }
      }, 10000);
    }
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
    if (!(navigator && navigator.mediaDevices && navigator.mediaDevices.getDisplayMedia)) {
      console.warn('[dxScreen] navigator.mediaDevices.getDisplayMedia not available; falling back to startShare');
      try {
        window.postMessage(
          { __dxf: 'share-unavailable', secure: !!window.isSecureContext },
          '*'
        );
      } catch (e) {}
      for (let i = 0; i < 20; i++) { if (room) break; await new Promise(function (r) { setTimeout(r, 100); }); }
      try {
        await startShare();
      } catch (e) {
        console.warn('[dxScreen] startShare fallback failed', e);
        notifyShareEnded();
      }
      return;
    }

    const wantW = quality.width || 1920;
    const wantH = quality.height || 1080;
    const wantFps = quality.fps || 30;
    const wantVideo = { width: { ideal: wantW }, height: { ideal: wantH }, frameRate: { ideal: wantFps } };
    const richAudio = { suppressLocalAudioPlayback: false };
    const wantAudio = quality.audio !== false;
    const nativeMode = wantAudio ? (quality.nativeAudio || 'never') : 'never';
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

    for (let i = 0; i < 150; i++) {
      if (room) break;
      await new Promise(function (r) { setTimeout(r, 100); });
    }
    if (!room) {
      console.warn('[dxScreen] room not connected yet, cannot start share');
      abortShare(stream);
      return;
    }

    const lk = LK();
    const vt = stream.getVideoTracks()[0];
    if (!vt) {
      console.warn('[dxScreen] no video track in captured stream');
      abortShare(stream);
      return;
    }
    let surface = '';
    try { surface = (vt.getSettings() || {}).displaySurface || ''; } catch (e) {}
    const useNative = nativeMode === 'always' || (nativeMode === 'monitor' && surface === 'monitor');
    vt.addEventListener('ended', function () { notifyShareEnded(); });
    try { vt.contentHint = quality.hint || 'detail'; } catch (e) {}
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
        videoEncoding: { maxBitrate: quality.bitrate || 6000000, maxFramerate: wantFps },
        degradationPreference: quality.degradation || 'balanced',
        simulcast: false,
      });
      const at = stream.getAudioTracks()[0];
      if (useNative && at) {
        try { at.stop(); } catch (e2) {}
      }
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
      try { window.postMessage({ __dxf: 'share-started', nativeAudio: useNative }, '*'); } catch (e2) {}
      if (!useNative && wantAudio) {
        try {
          window.postMessage(
            { __dxf: 'share-audio', published: published, supported: audioAsked },
            '*'
          );
        } catch (e2) {}
      }
    } catch (e) {
      console.warn('[dxScreen] direct publishTrack failed, falling back to setScreenShareEnabled', e);
      try { stream.getTracks().forEach(function (t) { t.stop(); }); } catch (e2) {}
      try {
        await startShare();
      } catch (e2) {
        console.warn('[dxScreen] startShare fallback failed', e2);
        notifyShareEnded();
      }
    }
  }
  async function stopLocalShareAudio() {
    const at = localShareAudio;
    localShareAudio = null;
    if (!at) return;
    if (room) {
      try { await room.localParticipant.unpublishTrack(at, true); } catch (e) {}
    }
    try { at.stop(); } catch (e) {}
  }
  async function stopShare() {
    await stopLocalShareAudio();
    if (!room) return;
    try { await room.localParticipant.setScreenShareEnabled(false); } catch (e) {}
  }
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
      videoEncoding: { maxBitrate: o.bitrate || 1200000, maxFramerate: o.fps || 30 },
      degradationPreference: 'maintain-framerate',
      simulcast: true,
    };
  }
  function attachLocalCamera(cid) {
    const c = document.getElementById(cid || 'camera-self');
    if (!c || !localCameraStream) return;
    c.innerHTML = '';
    const el = document.createElement('video');
    el.srcObject = localCameraStream;
    el.muted = true; el.autoplay = true; el.playsInline = true;
    el.style.width = '100%'; el.style.height = '100%'; el.style.objectFit = 'cover'; el.style.background = '#000';
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
    attachLocalCamera('camera-self');
    vt.addEventListener('ended', function () { notifyCameraEnded(); });
    for (let i = 0; i < 150 && !room; i++) await new Promise(function (r) { setTimeout(r, 100); });
    if (!room) { await stopCamera(); post('camera-error', { detail: 'not connected to the stream room' }); return; }
    try {
      await withTimeout(
        room.localParticipant.publishTrack(vt, cameraPublishOpts(opts)),
        15000,
        'publishing the camera track timed out'
      );
      const s = (function () { try { return vt.getSettings(); } catch (e) { return {}; } })();
      post('camera-started', { deviceId: (s && s.deviceId) || '', label: vt.label || '' });
      listCameras();
    } catch (e) {
      await stopCamera();
      post('camera-error', { detail: String((e && e.message) || e) });
    }
  }
  // Not provider.setKey: that hands the worker a CryptoKey, which WebKit wraps
  // with a keychain-held master key and macOS prompts for. The shim appended
  // to the worker imports the text itself; see assets/e2ee-worker-shim.js.
  function postRawKey(key) {
    if (!e2eeWorker) throw new Error('no e2ee worker to key');
    e2eeWorker.postMessage({
      kind: 'setKeyRaw',
      data: { keyString: key, keyIndex: 0, updateCurrentKeyIndex: false },
    });
  }
  async function setE2eeKey(key) {
    e2eeKey = key || null;
    if (desiredRoom) desiredRoom.key = e2eeKey;
    if (!e2eeKey || !e2eeProvider) return;
    try {
      postRawKey(e2eeKey);
      if (room) await room.setE2EEEnabled(true);
    } catch (e) {
      console.error('[dxScreen] rekey failed', e);
      post('e2ee-error', { detail: String((e && e.message) || e) });
    }
  }
  async function stopCamera() {
    const vt = localCameraTrack;
    localCameraTrack = null; localCameraStream = null;
    detachLocalCamera('camera-self');
    if (!vt) return;
    if (room) { try { await room.localParticipant.unpublishTrack(vt, true); } catch (e) {} }
    try { vt.stop(); } catch (e) {}
  }
  async function listCameras() {
    if (!(navigator && navigator.mediaDevices && navigator.mediaDevices.enumerateDevices)) { post('camera-devices', { devices: [] }); return; }
    try {
      const devs = await navigator.mediaDevices.enumerateDevices();
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
    await stopLocalShareAudio();
    await stopCamera();
    const previous = room;
    room = null;
    if (previous) { try { await previous.disconnect(); } catch (e) {} }
    clearRemoteTracks();
    Object.keys(attached).forEach(detach);
  }
  return { connect: connect, attach: attach, detach: detach, requestAndStartShare: requestAndStartShare, stopShare: stopShare, disconnect: disconnect, setStreamVolume: setStreamVolume, setSink: setSink, setNativeStreamAudio: setNativeStreamAudio, startCamera: startCamera, stopCamera: stopCamera, listCameras: listCameras, attachLocalCamera: attachLocalCamera, setE2eeKey: setE2eeKey };
})();
"#;

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

fn quality_preset(id: &str) -> (u32, u32, u32, u32, &'static str, &'static str) {
    match id {
        "smooth" => (1280, 720, 60, 6_000_000, "motion", "maintain-framerate"),
        "crisp" => (1920, 1080, 15, 6_000_000, "detail", "maintain-resolution"),
        "ultra" => (2560, 1440, 30, 14_000_000, "detail", "balanced"),
        _ => (1920, 1080, 30, 9_000_000, "motion", "balanced"),
    }
}

fn native_audio_mode() -> &'static str {
    match crate::sysaudio::scope() {
        crate::sysaudio::NativeScope::Always => "always",
        crate::sysaudio::NativeScope::MonitorOnly => "monitor",
        crate::sysaudio::NativeScope::Never => "never",
    }
}

pub fn native_settings(quality: &str) -> crate::sysvideo::Settings {
    let (width, height, fps, bitrate, _hint, _degradation) = quality_preset(quality);
    crate::sysvideo::Settings {
        width,
        height,
        fps,
        max_bitrate: bitrate as u64,
    }
}

pub(crate) fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

pub fn share_js(on: bool, quality: &str, audio: bool) -> String {
    if !on {
        return format!("{SCREEN_JS}\nwindow.dxScreen.stopShare();");
    }
    let (w, h, fps, bitrate, hint, degradation) = quality_preset(quality);
    let mode = native_audio_mode();
    let (hint, degradation, mode) = (js_str(hint), js_str(degradation), js_str(mode));
    format!(
        "{SCREEN_JS}\nwindow.dxScreen.requestAndStartShare({{width:{w},height:{h},fps:{fps},\
         bitrate:{bitrate},hint:{hint},degradation:{degradation},audio:{audio},nativeAudio:{mode}}});"
    )
}

pub(crate) fn attach_js(identity: &str, container: &str, kind: &str) -> String {
    let (identity, container, kind) = (js_str(identity), js_str(container), js_str(kind));
    format!("{SCREEN_JS}\nwindow.dxScreen.attach({identity},{container},{kind});")
}

pub(crate) fn detach_js(container: &str) -> String {
    let container = js_str(container);
    format!("{SCREEN_JS}\nwindow.dxScreen.detach({container});")
}

fn stream_volume_js(gain: f32) -> String {
    format!("{SCREEN_JS}\nwindow.dxScreen.setStreamVolume({gain});")
}

pub fn stream_sink_js(device: Option<&str>) -> String {
    let arg = serde_json::to_string(&device).unwrap_or_else(|_| "null".into());
    format!("{SCREEN_JS}\nwindow.dxScreen.setSink({arg});")
}

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
                    let (url, tok) = (js_str(url), js_str(tok));
                    let key = crate::e2ee::current_key()
                        .map(|k| js_str(&k))
                        .unwrap_or_else(|| "null".into());
                    let encrypt = crate::e2ee::enabled();
                    let _ = document::eval(&format!(
                        "{SCREEN_JS}\nwindow.dxScreen.connect({url},{tok},{key},{encrypt});"
                    ));
                }
                None => {
                    let _ = document::eval(&format!("{SCREEN_JS}\nwindow.dxScreen.disconnect();"));
                }
            }
            last.set(t);
        }
    });

    let native_audio_live = use_memo(move || state.read().screen_audio_joined);
    use_effect(move || {
        let on = native_audio_live();
        crate::dlog!("screen setNativeStreamAudio({on})");
        let _ = document::eval(&format!(
            "{SCREEN_JS}\nwindow.dxScreen.setNativeStreamAudio({on});"
        ));
    });

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
        let target = s.screen_share_target;
        let want_video = match (s.screen_sharing && crate::sysvideo::supported(), target) {
            (true, Some(_)) => s.screen_video_token.clone(),
            _ => None,
        };
        drop(s);
        let quality = settings.read().screenshare_quality.clone();

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
                target: target.unwrap_or(crate::sysvideo::Target::Display(0)),
                settings: native_settings(&quality),
            });
            last_sent.set(Some(now));
        }
    });

    use_future(move || {
        let mut s = state;
        async move {
            if crate::sysvideo::supported() {
                s.write().screen_capture_available = true;
                return;
            }
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
                Err(e) => {
                    eprintln!("[screen] capture probe failed, assuming available: {e:?}");
                    s.write().screen_capture_available = true;
                }
            }
        }
    });

    let gateway_end = gateway.clone();
    use_future(move || {
        let mut state = state;
        let gateway = gateway_end.clone();
        async move {
            let bridge_js = r#"
            window.__dxfShareSink = function (m) { try { dioxus.send(m); } catch (err) {} };
            if (!window.__dxfShareEndWired) {
              window.__dxfShareEndWired = true;
              window.addEventListener('message', function (e) {
                var d = e.data;
                if (d && (d.__dxf === 'screen-share-ended' || d.__dxf === 'share-started' || d.__dxf === 'share-audio' || d.__dxf === 'share-unavailable' || d.__dxf === 'share-echo-risk' || d.__dxf === 'stream-audio' || d.__dxf === 'screen-room-error' || d.__dxf === 'screen-room-reconnecting' || d.__dxf === 'screen-track-timeout' || d.__dxf === 'e2ee-error' || d.__dxf === 'e2ee-undecryptable') && window.__dxfShareSink) {
                  window.__dxfShareSink(d);
                }
              });
            }
            "#;
            let mut eval = document::eval(bridge_js);
            while let Ok(msg) = eval.recv::<Value>().await {
                match msg.get("__dxf").and_then(|v| v.as_str()) {
                    Some("share-started") => {
                        let cid = state.read().voice.channel_id;
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
                    Some("e2ee-undecryptable") => {
                        let detail = msg
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown error");
                        tracing::warn!(detail, "media arrived that we cannot decrypt");
                        state.write().media_undecryptable = true;
                    }
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
                    Some("share-audio") => {
                        let published = msg
                            .get("published")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
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
    if !already_sharing && let Some(cid) = channel {
        gateway.send(ClientMessage::SetScreenShare {
            channel_id: cid,
            sharing: true,
        });
    }
}

pub fn open_screen_picker(mut state: Signal<crate::state::AppState>) {
    state.write().screen_picker = Some(Ok(Vec::new()));
    dioxus::prelude::spawn(async move {
        let found = tokio::task::spawn_blocking(crate::sysvideo::sources)
            .await
            .unwrap_or_else(|e| Err(format!("listing screens failed: {e}")));
        if state.peek().screen_picker.is_some() {
            state.write().screen_picker = Some(found);
        }
    });
}

#[component]
pub fn ScreenSelfPreview() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();

    let mut px = use_signal(|| 968.0_f64);
    let mut py = use_signal(|| 56.0_f64);
    let mut pw = use_signal(|| 300.0_f64);
    let mut ph = use_signal(|| 208.0_f64);
    let mut drag = use_signal(|| None::<Drag>);

    let sharing = use_memo(move || state.read().screen_sharing);
    let self_pk = use_memo(move || state.read().self_user.as_ref().map(|u| u.pubkey.clone()));

    let native_capture = crate::sysvideo::supported();
    let share_label = use_memo(move || match state.read().screen_share_target {
        Some(crate::sysvideo::Target::Display(_)) => "Sharing your screen",
        Some(crate::sysvideo::Target::Window(_)) => "Sharing one window",
        Some(crate::sysvideo::Target::Application(_)) => "Sharing an app",
        None => "Sharing your screen",
    });
    let mut frames = use_signal(|| 0_u64);
    use_future(move || async move {
        loop {
            frames.set(crate::sysvideo::frames_captured());
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    });

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
                    onmousedown: move |e| {
                        e.stop_propagation();
                        let cid = state.read().voice.channel_id;
                        {
                            let mut w = state.write();
                            w.screen_sharing = false;
                            w.screen_share_target = None;
                            w.screen_native_audio = false;
                        }
                        if !crate::sysvideo::supported() {
                            let _ = document::eval(&share_js(false, "", true));
                        }
                        if let Some(c) = cid {
                            gateway.send(ClientMessage::SetScreenShare { channel_id: c, sharing: false });
                        }
                    },
                    "Stop"
                }
            }
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
pub(crate) enum Drag {
    Move { dx: f64, dy: f64 },
    Resize { px: f64, py: f64, w0: f64, h0: f64 },
}

#[component]
pub fn ScreenWatchWindow() -> Element {
    let mut state = use_app_state();
    let viewing = use_memo(move || state.read().screen_viewing.clone());

    let mut last = use_signal(|| None::<String>);
    use_effect(move || {
        let v = viewing();
        if v != *last.peek() {
            match &v {
                Some(pk) => {
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
                    crate::dlog!("watch detach (viewer closed)");
                    let _ = document::eval(&detach_js("screenshare-viewer"));
                }
            }
            last.set(v);
        }
    });

    let watching = use_memo(move || state.read().screen_viewing.clone());
    let stream_levels = use_memo(move || {
        let s = state.read();
        (s.stream_volumes.clone(), s.stream_muted.clone())
    });
    let voice_for_stream = use_voice_tx();
    let mut last_gains = use_signal(Vec::<(String, f32)>::new);
    use_effect(move || {
        let watched = watching();
        let _ = stream_levels();
        let s = state.read();
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

    let output_device = use_memo(move || state.read().selected_output_device.clone());
    use_effect(move || {
        let _ = document::eval(&stream_sink_js(output_device().as_deref()));
    });

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
    let name = state.read().display_name(&pk);

    let stream_volume = state.read().stream_volumes.get(&pk).copied().unwrap_or(100);
    let stream_muted = state.read().stream_muted.contains(&pk);
    let has_audio = state.read().stream_has_audio.contains(&pk);
    let pk_vol = pk.clone();
    let pk_mute = pk.clone();
    let apply_stream = move |vol: u32, muted: bool| {
        let gain = if muted { 0.0 } else { vol as f32 / 100.0 };
        let _ = document::eval(&stream_volume_js(gain));
    };
    let apply_from_slider = apply_stream;

    rsx! {
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

#[cfg(test)]
mod js_escaping_tests {
    use super::{attach_js, js_str, share_js};

    #[test]
    fn a_quote_in_a_server_string_cannot_close_the_literal() {
        for hostile in [
            "ws://x');alert(1);//",    // breaks the old single-quoted form
            "ws://x\");alert(1);//",   // and the double-quoted one
            "ws://x\\\");alert(1);//", // and a pre-escaped attempt at it
        ] {
            let quoted = js_str(hostile);

            assert!(
                quoted.starts_with('"') && quoted.ends_with('"'),
                "one complete JS string literal: {quoted}"
            );
            let inner = &quoted[1..quoted.len() - 1];
            let mut chars = inner.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    chars.next();
                } else {
                    assert_ne!(c, '"', "a bare quote escaped the literal: {quoted}");
                }
            }
            let back: String = serde_json::from_str(&quoted).expect("valid literal");
            assert_eq!(back, hostile);
        }
    }

    #[test]
    fn escaping_preserves_the_value() {
        for raw in [
            "wss://sfu.example.com:7880",
            "a'b",
            "a\"b",
            "back\\slash",
            "new\nline",
            "</script>",
            "🙂",
        ] {
            let parsed: String =
                serde_json::from_str(&js_str(raw)).expect("still valid JSON/JS string");
            assert_eq!(parsed, raw, "value survived escaping unchanged");
        }
    }

    #[test]
    fn the_call_sites_emit_quoted_arguments() {
        let js = attach_js("pk#video", "screen-tile", "screen");
        assert!(
            js.contains(r#"attach("pk#video","screen-tile","screen")"#),
            "attach passes JSON-quoted arguments: {}",
            js.lines().last().unwrap_or_default()
        );

        let js = share_js(true, "high", true);
        assert!(
            !js.contains("hint:'"),
            "no hand-built single-quoted literals remain in share_js"
        );
    }
}
