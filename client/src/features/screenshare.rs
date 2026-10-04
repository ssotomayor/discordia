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
  let connectionRequest = null;
  let localShareAudio = null;
  let localShareVideoTrack = null;
  let screenCaptureTrack = null;
  let remoteShareVideoTrack = null;
  let selfPreviewIdentity = null;
  let selfPreviewEnabled = true;
  let screenStatsEnabled = false;
  let screenStatsTimer = null;
  let screenStatsInFlight = false;
  let remoteStatsInFlight = false;
  let statsEpoch = 0;
  let nextStatsTrackId = 0;
  const statsTrackIds = new WeakMap();
  function statsSampleKey(track, entry) {
    if (!statsTrackIds.has(track)) statsTrackIds.set(track, ++nextStatsTrackId);
    return statsEpoch + '/' + statsTrackIds.get(track) + '/' + (entry.id || 'video');
  }
  const videoReports = new WeakMap();
  function videoStatsReport(track) {
    const now = Date.now();
    const sample = videoReports.get(track);
    if (sample && (sample.pending || (now >= sample.at && now - sample.at < 500))) return sample.report;
    const next = { at: now, pending: true, report: null };
    next.report = Promise.resolve().then(function () { return track.getRTCStatsReport(); }).finally(function () {
      next.pending = false;
    });
    videoReports.set(track, next);
    return next.report;
  }
  let e2eeKey = null;
  let e2eeWorker = null;
  let e2eeWorkerUrl = null;
  let e2eeOn = false;
  let e2eeProvider = null;
  let localCameraTrack = null;
  let localCameraStream = null;
  let lastCameraOpts = {};
  let cameraStarting = false;
  let cameraGen = 0;
  const tracks = {};
  const audioTracks = {};
  function trackKey(id, kind) { return id + '|' + kind; }
  function kindOf(pub, track) {
    const s = (pub && pub.source) || (track && track.source) || '';
    return s === 'camera' ? 'camera' : 'screen';
  }
  const attached = {};
  let viewerTargets = null;
  let detachedScreens = [];
  function applyViewerSubscription(pub, participant) {
    if (!pub || !participant || pub.kind !== 'video') return;
    const identity = baseIdentity(participant.identity);
    const kind = kindOf(pub, pub.track);
    if (viewerTargets === null && (kind !== 'screen' || !detachedScreens.includes(identity))) return;
    const enabled = viewerTargets === null
      ? !detachedScreens.includes(identity)
      : kind === 'screen' && viewerTargets.includes(identity);
    try { pub.setSubscribed(enabled); } catch (e) { console.warn('[dxScreen] viewer subscribe toggle failed', e); }
  }
  function refreshViewerSubscriptions() {
    if (!room || !room.remoteParticipants) return;
    room.remoteParticipants.forEach(function (p) {
      p.trackPublications.forEach(function (pub) {
        if (pub.kind !== 'video') return;
        if (viewerTargets === null && kindOf(pub, pub.track) === 'screen') {
          try { pub.setSubscribed(!detachedScreens.includes(baseIdentity(p.identity))); } catch (e) {}
        } else applyViewerSubscription(pub, p);
        applySelfPreviewSubscription(pub, p);
      });
    });
  }
  function setViewerTargets(identities) { viewerTargets = identities; refreshViewerSubscriptions(); }
  function setDetachedScreens(identities) { detachedScreens = identities; refreshViewerSubscriptions(); }
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
    Object.keys(attached).forEach(function (cid) {
      if (cid.startsWith('screenshare-viewer-')) attachAudio(attached[cid].identity);
    });
  }
  const audioElements = {};
  const audioGains = {};
  let sinkLabel = null;
  function applySink() {
    if (!sinkLabel || !navigator.mediaDevices || !navigator.mediaDevices.enumerateDevices) return;
    navigator.mediaDevices.enumerateDevices().then(function (devs) {
      const device = devs.find(function (d) { return d.kind === 'audiooutput' && d.label === sinkLabel; });
      if (!device) return;
      Object.keys(audioElements).forEach(function (identity) {
        const t = audioTracks[identity];
        if (t && typeof t.setSinkId === 'function') t.setSinkId(device.deviceId).catch(function () {});
      });
    }).catch(function () {});
  }
  function setSink(label) { sinkLabel = label || null; applySink(); }
  function setStreamVolume(v, identity) {
    if (!identity) return;
    identity = baseIdentity(identity);
    audioGains[identity] = Math.max(0, Math.min(1, v));
    const t = audioTracks[identity];
    if (t) { try { t.setVolume(audioGains[identity]); } catch (e) {} }
  }
  function detachAudio(identity) {
    if (identity) identity = baseIdentity(identity);
    const identities = identity ? [identity] : Object.keys(audioElements);
    identities.forEach(function (id) {
      const el = audioElements[id];
      const t = audioTracks[id];
      if (t && el) { try { t.detach(el); } catch (e) {} }
      if (el) el.remove();
      delete audioElements[id];
    });
  }
  function attachAudio(identity) {
    identity = baseIdentity(identity);
    if (audioElements[identity]) return;
    const t = audioTracks[identity];
    if (!t) { report(identity, false); return; }
    try {
      const el = t.attach();
      el.style.display = 'none';
      document.body.appendChild(el);
      audioElements[identity] = el;
      t.setVolume(audioGains[identity] === undefined ? 0 : audioGains[identity]);
      applySink();
      if (room && room.canPlaybackAudio === false) room.startAudio().catch(function () {});
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
    if (kind === 'camera' && cid === 'camera-self') el.style.transform = 'scaleX(-1)';
    c.appendChild(el);
    attached[cid] = { identity: identity, kind: kind, track: track, el: el };
  }
  const VIDEO_SUFFIX = '#video';
  function baseIdentity(id) {
    return id.endsWith(VIDEO_SUFFIX) ? id.slice(0, -VIDEO_SUFFIX.length) : (id.endsWith('#audio') ? id.slice(0, -6) : id);
  }
  function applySelfPreviewSubscription(pub, participant, previousIdentity) {
    if (!pub || !participant || pub.kind !== 'video' || kindOf(pub, pub.track) !== 'screen') return;
    const identity = baseIdentity(participant.identity);
    if (viewerTargets !== null || detachedScreens.includes(identity)) return;
    if (identity !== selfPreviewIdentity && identity !== previousIdentity) return;
    try { pub.setSubscribed(identity === selfPreviewIdentity ? selfPreviewEnabled : true); }
    catch (e) { console.warn('[dxScreen] self preview subscribe toggle failed', e); }
  }
  function setSelfPreview(identity, enabled) {
    const previousIdentity = selfPreviewIdentity;
    selfPreviewIdentity = identity;
    selfPreviewEnabled = !!enabled;
    if (!selfPreviewEnabled || !identity) detach('screenshare-self');
    if (room && room.remoteParticipants) room.remoteParticipants.forEach(function (participant) {
      participant.trackPublications.forEach(function (pub) {
        applySelfPreviewSubscription(pub, participant, previousIdentity);
      });
    });
    if (selfPreviewEnabled && identity) attach(identity, 'screenshare-self', 'screen');
  }
  function videoTrackFor(id, kind) {
    if (kind === 'camera') return tracks[trackKey(id, 'camera')] || tracks[trackKey(id + VIDEO_SUFFIX, 'camera')];
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
  async function ensureLib(current) { for (let i = 0; i < 100 && !LK() && (!current || current()); i++) { await new Promise(function (r) { setTimeout(r, 100); }); } return !!LK(); }
  function reportRoomProblem(kind, detail) {
    try { window.postMessage({ __dxf: kind, detail: String(detail || '') }, '*'); } catch (e) {}
  }
  function clearRemoteTracks() {
    remoteShareVideoTrack = null;
    post('screen-stats-in', { active: false });
    detachAudio();
    for (const k in tracks) delete tracks[k];
    for (const k in audioTracks) delete audioTracks[k];
    Object.keys(attached).forEach(function (cid) {
      const a = attached[cid];
      if (a && a.track && a.el) { try { a.track.detach(a.el); } catch (e) {} }
      const c = document.getElementById(cid);
      if (c) c.querySelectorAll('video').forEach(function (e) { e.remove(); });
      if (a) { a.track = null; a.el = null; }
    });
  }
  async function connect(url, token, key, encrypt, generation) {
    const request = { generation: generation };
    connectionRequest = request;
    const current = () => connectionRequest === request;
    const reportState = (status) => { if (current()) post('screen-room-state', { generation: generation, status: status }); };
    e2eeKey = key || null;
    e2eeOn = !!encrypt;
    if (room) {
      const previous = room;
      await stopLocalShareAudio();
      if (!current()) return;
      await stopCamera();
      if (!current()) return;
      try { await previous.localParticipant.setScreenShareEnabled(false); } catch (e) {}
      if (!current()) return;
      room = null;
      clearRemoteTracks();
      try { await previous.disconnect(); } catch (e) {}
      if (!current()) return;
    }
    if (!(await ensureLib(current))) {
      if (!current()) return;
      console.warn('[dxScreen] livekit lib not loaded');
      reportRoomProblem('screen-room-error', 'LiveKit did not load');
      reportState('retry');
      return;
    }
    if (!current()) return;
    const lk = LK();
    const opts = { adaptiveStream: true, dynacast: true };
    dropE2eeWorker();
    e2eeProvider = null;
    // Encryption that cannot be set up is a room that is not joined: joining
    // anyway would publish in the clear to the SFU, which is the one thing
    // the key exists to prevent.
    if (e2eeOn) {
      let failure = null;
      if (!window.__dxfE2eeWorkerSrc) {
        failure = 'the encryption worker was not injected';
      } else {
        try {
          const provider = new lk.ExternalE2EEKeyProvider();
          e2eeWorkerUrl = URL.createObjectURL(
            new Blob([window.__dxfE2eeWorkerSrc], { type: 'application/javascript' })
          );
          e2eeWorker = new Worker(e2eeWorkerUrl);
          opts.e2ee = { keyProvider: provider, worker: e2eeWorker };
          e2eeProvider = provider;
        } catch (e) {
          failure = String((e && e.message) || e);
        }
      }
      if (failure) {
        dropE2eeWorker();
        e2eeProvider = null;
        console.error('[dxScreen] could not set up e2ee; not joining', failure);
        post('e2ee-error', { detail: failure });
        reportState('blocked');
        return;
      }
    }
    let thisRoom;
    try { thisRoom = new lk.Room(opts); } catch (e) {
      dropE2eeWorker();
      e2eeProvider = null;
      reportRoomProblem('screen-room-error', e && e.message ? e.message : e);
      reportState('retry');
      return;
    }
    room = thisRoom;
    thisRoom.on(lk.RoomEvent.EncryptionError, function (err) {
      if (!current() || room !== thisRoom) return;
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
        if (!current()) { try { await thisRoom.disconnect(); } catch (ignored) {} return; }
        room = null;
        try { await thisRoom.disconnect(); } catch (ignored) {}
        if (!current()) return;
        dropE2eeWorker();
        e2eeProvider = null;
        console.error('[dxScreen] enabling e2ee failed; not joining', e);
        post('e2ee-error', { detail: String((e && e.message) || e) });
        reportState('blocked');
        return;
      }
    }
    if (!current()) { try { await thisRoom.disconnect(); } catch (e) {} return; }
    thisRoom.on(lk.RoomEvent.Disconnected, function (reason) {
      console.warn('[dxScreen] room disconnected', reason);
      if (!current() || room !== thisRoom) return;
      room = null;
      stopLocalShareAudio();
      clearRemoteTracks();
      reportRoomProblem('screen-room-reconnecting', reason || 'disconnected');
      reportState('retry');
    });
    thisRoom.on(lk.RoomEvent.ConnectionStateChanged, function (st) {
      console.log('[dxScreen] connection state', st);
    });
    thisRoom.on(lk.RoomEvent.TrackPublished, function (pub, participant) {
      if (room !== thisRoom) return;
      applyAudioSubscription(pub);
      applySelfPreviewSubscription(pub, participant);
      applyViewerSubscription(pub, participant);
    });
    thisRoom.on(lk.RoomEvent.TrackSubscribed, function (track, pub, participant) {
      if (room !== thisRoom) return;
      if (track.kind === 'audio') {
        const identity = baseIdentity(participant.identity);
        if (audioTracks[identity] !== track) detachAudio(identity);
        audioTracks[identity] = track;
        if (nativeStreamAudio) return;
        attachWatched();
        return;
      }
      if (track.kind !== 'video') return;
      const kind = kindOf(pub, track);
      tracks[trackKey(participant.identity, kind)] = track;
      if (kind === 'screen') {
        remoteShareVideoTrack = track;
        if (screenStatsEnabled) pollRemoteScreenStats();
      }
      reattach(participant.identity, kind);
    });
    thisRoom.on(lk.RoomEvent.TrackUnsubscribed, function (track, pub, participant) {
      if (room !== thisRoom) return;
      if (track.kind === 'audio') {
        if (audioTracks[baseIdentity(participant.identity)] !== track) return;
        detachAudio(baseIdentity(participant.identity));
        delete audioTracks[baseIdentity(participant.identity)];
        if (!nativeStreamAudio) report(participant.identity, false);
        return;
      }
      if (track.kind !== 'video') return;
      const kind = kindOf(pub, track);
      if (tracks[trackKey(participant.identity, kind)] !== track) return;
      if (kind === 'screen' && remoteShareVideoTrack === track) {
        remoteShareVideoTrack = null;
        post('screen-stats-in', { active: false });
      }
      delete tracks[trackKey(participant.identity, kind)];
      reattach(participant.identity, kind);
    });
    thisRoom.on(lk.RoomEvent.LocalTrackPublished, function (pub) {
      if (room !== thisRoom) return;
      if (!pub.track || pub.track.kind !== 'video') return;
      const kind = kindOf(pub, pub.track);
      tracks[trackKey(thisRoom.localParticipant.identity, kind)] = pub.track;
      reattach(thisRoom.localParticipant.identity, kind);
    });
    thisRoom.on(lk.RoomEvent.LocalTrackUnpublished, function (pub) {
      if (room !== thisRoom) return;
      if (!pub.track || pub.track.kind !== 'video') return;
      if (pub.track === localShareVideoTrack) {
        localShareVideoTrack = null;
        screenCaptureTrack = null;
        post('screen-stats', { active: false });
      }
      const kind = kindOf(pub, pub.track);
      delete tracks[trackKey(thisRoom.localParticipant.identity, kind)];
      reattach(thisRoom.localParticipant.identity, kind);
      if (kind === 'camera') notifyCameraEnded(); else notifyShareEnded();
    });
    try {
      await thisRoom.connect(url, token, { autoSubscribe: viewerTargets === null });
      if (!current() || room !== thisRoom) { try { await thisRoom.disconnect(); } catch (e) {} return; }
      reportState('connected');
    } catch (e) {
      if (!current()) { try { await thisRoom.disconnect(); } catch (ignored) {} return; }
      console.warn('[dxScreen] connect failed', e);
      if (room === thisRoom) room = null;
      try { await thisRoom.disconnect(); } catch (ignored) {}
      if (!current()) return;
      reportRoomProblem('screen-room-error', e && e.message ? e.message : e);
      reportState('retry');
      return;
    }
    applyAudioSubscriptions();
    if (room && room.remoteParticipants) room.remoteParticipants.forEach(function (participant) {
      participant.trackPublications.forEach(function (pub) {
        applySelfPreviewSubscription(pub, participant);
        applyViewerSubscription(pub, participant);
      });
    });
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
  function releaseStream(stream) {
    if (stream) {
      try { stream.getTracks().forEach(function (t) { t.stop(); }); } catch (e) {}
    }
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
  async function pollScreenStats() {
    if (!screenStatsEnabled || screenStatsInFlight) return;
    if (!localShareVideoTrack || !screenCaptureTrack) {
      post('screen-stats', { active: false });
      return;
    }
    screenStatsInFlight = true;
    try {
      const track = localShareVideoTrack;
      const epoch = statsEpoch;
      const stats = await track.getRTCStatsReport();
      if (!screenStatsEnabled || !stats || track !== localShareVideoTrack || epoch !== statsEpoch) return;
      let outbound = null;
      stats.forEach(function (entry) {
        if (entry.type === 'outbound-rtp') outbound = entry;
      });
      if (!outbound) return;
      const capture = (function () { try { return screenCaptureTrack.getSettings(); } catch (e) { return {}; } })();
      const codec = outbound.codecId ? stats.get(outbound.codecId) : null;
      const remote = outbound.remoteId ? stats.get(outbound.remoteId) : null;
      post('screen-stats', {
        active: true,
        captureWidth: capture.width || null,
        captureHeight: capture.height || null,
        captureFps: capture.frameRate || null,
        encodedWidth: outbound.frameWidth || null,
        encodedHeight: outbound.frameHeight || null,
        encodedFps: outbound.framesPerSecond,
        sampleKey: statsSampleKey(track, outbound),
        timestampMs: outbound.timestamp,
        bytes: outbound.bytesSent,
        targetBitrate: outbound.targetBitrate,
        codec: codec && codec.mimeType || null,
        codecImplementation: outbound.encoderImplementation || null,
        powerEfficient: typeof outbound.powerEfficientEncoder === 'boolean' ? outbound.powerEfficientEncoder : null,
        qualityLimitationReason: outbound.qualityLimitationReason || null,
        frames: Number.isFinite(outbound.framesEncoded) ? outbound.framesEncoded : null,
        packets: Number.isFinite(outbound.packetsSent) ? outbound.packetsSent : null,
        packetsLost: remote && Number.isFinite(remote.packetsLost) ? remote.packetsLost : null,
        jitterSeconds: remote && remote.jitter,
      });
    } catch (e) {
      if (screenStatsEnabled) post('screen-stats', { active: true, error: String((e && e.message) || e) });
    } finally {
      screenStatsInFlight = false;
    }
  }
  async function pollRemoteScreenStats() {
    if (!screenStatsEnabled || remoteStatsInFlight) return;
    if (!remoteShareVideoTrack) {
      for (const key of Object.keys(tracks)) {
        const track = tracks[key];
        if (key.endsWith('|screen') && track !== localShareVideoTrack && typeof track.getRTCStatsReport === 'function') {
          remoteShareVideoTrack = track;
          break;
        }
      }
    }
    if (!remoteShareVideoTrack) {
      post('screen-stats-in', { active: false });
      return;
    }
    remoteStatsInFlight = true;
    const track = remoteShareVideoTrack;
    const epoch = statsEpoch;
    try {
      const stats = await videoStatsReport(track);
      if (!screenStatsEnabled || track !== remoteShareVideoTrack || epoch !== statsEpoch) return;
      let inbound = null;
      if (stats) stats.forEach(function (entry) {
        if (entry.type === 'inbound-rtp' && (entry.kind === 'video' || entry.mediaType === 'video')) inbound = entry;
      });
      if (!inbound) {
        post('screen-stats-in', { active: true, trackSid: track.sid || null, error: 'Video is subscribed; waiting for receiver statistics.' });
        return;
      }
      const codec = inbound.codecId ? stats.get(inbound.codecId) : null;
      post('screen-stats-in', {
        active: true,
        trackSid: track.sid || null,
        freezeCount: Number.isFinite(inbound.freezeCount) ? inbound.freezeCount : null,
        freezeDurationSeconds: Number.isFinite(inbound.totalFreezesDuration) ? inbound.totalFreezesDuration : null,
        framesDropped: Number.isFinite(inbound.framesDropped) ? inbound.framesDropped : null,
        nackCount: Number.isFinite(inbound.nackCount) ? inbound.nackCount : null,
        pliCount: Number.isFinite(inbound.pliCount) ? inbound.pliCount : null,
        keyFramesDecoded: Number.isFinite(inbound.keyFramesDecoded) ? inbound.keyFramesDecoded : null,
        encodedWidth: inbound.frameWidth || null,
        encodedHeight: inbound.frameHeight || null,
        encodedFps: inbound.framesPerSecond,
        sampleKey: statsSampleKey(track, inbound),
        timestampMs: inbound.timestamp,
        bytes: inbound.bytesReceived,
        codec: codec && codec.mimeType || null,
        codecImplementation: inbound.decoderImplementation || null,
        powerEfficient: typeof inbound.powerEfficientDecoder === 'boolean' ? inbound.powerEfficientDecoder : null,
        frames: Number.isFinite(inbound.framesDecoded) ? inbound.framesDecoded : null,
        packets: Number.isFinite(inbound.packetsReceived) ? inbound.packetsReceived : null,
        packetsLost: Number.isFinite(inbound.packetsLost) ? inbound.packetsLost : null,
        jitterSeconds: inbound.jitter,
      });
    } catch (e) {
      if (screenStatsEnabled && track === remoteShareVideoTrack && epoch === statsEpoch) {
        post('screen-stats-in', { active: true, trackSid: track.sid || null, error: String((e && e.message) || e) });
      }
    } finally {
      remoteStatsInFlight = false;
    }
  }
  function setStatsEnabled(enabled) {
    screenStatsEnabled = !!enabled;
    window.__dxfScreenStatsEnabled = screenStatsEnabled;
    statsEpoch++;
    if (screenStatsTimer) clearInterval(screenStatsTimer);
    screenStatsTimer = null;
    if (!screenStatsEnabled) {
      post('screen-stats', { active: false });
      post('screen-stats-in', { active: false });
      return;
    }
    pollScreenStats();
    pollRemoteScreenStats();
    screenStatsTimer = setInterval(function () {
      pollScreenStats();
      pollRemoteScreenStats();
    }, 1000);
  }
  function attach(identity, cid, kind, tries) {
    if (cid === 'screenshare-self' && !selfPreviewEnabled) return;
    kind = kind || 'screen';
    const c = document.getElementById(cid);
    if (!c) {
      if ((tries || 0) < 20) setTimeout(function () { attach(identity, cid, kind, (tries || 0) + 1); }, 50);
      return;
    }
    const previous = attached[cid];
    if (previous && previous.identity !== identity) detach(cid);
    c.setAttribute('data-identity', identity);
    if (!attached[cid]) attached[cid] = { identity: identity, kind: kind, track: null, el: null };
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
    if (cid.startsWith('screenshare-viewer-') && !nativeStreamAudio) attachAudio(identity);
  }
  function detach(cid) {
    const a = attached[cid];
    if (a && a.track && a.el) { try { a.track.detach(a.el); } catch (e) {} }
    delete attached[cid];
    if (cid.startsWith('screenshare-viewer-') && a) detachAudio(a.identity);
    const c = document.getElementById(cid); if (!c) return;
    c.removeAttribute('data-identity');
    c.querySelectorAll('video').forEach(function (e) { e.remove(); });
  }
  async function previewStats(identity) {
    const track = videoTrackFor(identity, 'screen');
    if (!track) return null;
    try {
      const report = await videoStatsReport(track);
      let result = null;
      if (report) report.forEach(function (entry) {
        if (entry.type === 'inbound-rtp' && (entry.kind === 'video' || entry.mediaType === 'video')) {
          result = { width: entry.frameWidth || null, height: entry.frameHeight || null, fps: Number.isFinite(entry.framesPerSecond) ? entry.framesPerSecond : null };
        }
      });
      return result;
    } catch (e) { return null; }
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
    try { vt.contentHint = quality.hint || 'motion'; } catch (e) {}
    try {
      await vt.applyConstraints({
        width: { ideal: wantW }, height: { ideal: wantH }, frameRate: { ideal: wantFps },
      });
    } catch (e) { console.warn('[dxScreen] applyConstraints ignored', e); }
    const settings = (function () { try { return vt.getSettings(); } catch (e) { return {}; } })();
    console.log('[dxScreen] capturing', settings.width + 'x' + settings.height, '@', settings.frameRate, 'fps');
    try {
      const publication = await room.localParticipant.publishTrack(vt, {
        source: lk.Track.Source.ScreenShare,
        screenShareEncoding: { maxBitrate: quality.bitrate || 6000000, maxFramerate: wantFps },
        videoCodec: 'h264',
        degradationPreference: quality.degradation || 'balanced',
        simulcast: false,
      });
      localShareVideoTrack = publication.track;
      screenCaptureTrack = vt;
      pollScreenStats();
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
    // A stop during any wait below moves the generation on; the start then
    // releases what it opened and publishes nothing.
    const gen = cameraGen;
    const cancelled = function () { return gen !== cameraGen; };
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
    if (cancelled()) { releaseStream(stream); return; }
    const vt = stream && stream.getVideoTracks()[0];
    if (!vt) { post('camera-error', { detail: 'no video track' }); return; }
    localCameraTrack = vt; localCameraStream = stream;
    attachLocalCamera('camera-self');
    vt.addEventListener('ended', function () { notifyCameraEnded(); });
    for (let i = 0; i < 150 && !room && !cancelled(); i++) await new Promise(function (r) { setTimeout(r, 100); });
    if (cancelled()) { releaseStream(stream); return; }
    if (!room) { await stopCamera(); post('camera-error', { detail: 'not connected to the stream room' }); return; }
    try {
      await withTimeout(
        room.localParticipant.publishTrack(vt, cameraPublishOpts(opts)),
        15000,
        'publishing the camera track timed out'
      );
      if (cancelled()) {
        try { await room.localParticipant.unpublishTrack(vt, true); } catch (e) {}
        releaseStream(stream);
        return;
      }
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
  // updateCurrentKeyIndex stays true, as provider.setKey left it: that is what
  // makes the worker forget a key it had given up on, so a rekey recovers a
  // receiver that failed too often during the transition.
  function postRawKey(key) {
    if (!e2eeWorker) throw new Error('no e2ee worker to key');
    e2eeWorker.postMessage({
      kind: 'setKeyRaw',
      data: { keyString: key, keyIndex: 0, updateCurrentKeyIndex: true },
    });
  }
  function dropE2eeWorker() {
    if (e2eeWorker) { try { e2eeWorker.terminate(); } catch (e) {} }
    if (e2eeWorkerUrl) { try { URL.revokeObjectURL(e2eeWorkerUrl); } catch (e) {} }
    e2eeWorker = null;
    e2eeWorkerUrl = null;
  }
  async function setE2eeKey(key) {
    e2eeKey = key || null;
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
    cameraGen++;
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
    const request = {};
    connectionRequest = request;
    const previous = room;
    const audioStopped = stopLocalShareAudio();
    const cameraStopped = stopCamera();
    room = null;
    clearRemoteTracks();
    Object.keys(attached).forEach(detach);
    await Promise.all([audioStopped, cameraStopped]);
    if (previous) { try { await previous.disconnect(); } catch (e) {} }
    if (connectionRequest !== request) return;
    connectionRequest = null;
    dropE2eeWorker();
    e2eeProvider = null;
  }
  if (window.__dxfScreenStatsEnabled) setStatsEnabled(true);
  return { connect: connect, setViewerTargets: setViewerTargets, setDetachedScreens: setDetachedScreens, setSelfPreview: setSelfPreview, attach: attach, detach: detach, previewStats: previewStats, requestAndStartShare: requestAndStartShare, stopShare: stopShare, disconnect: disconnect, setStreamVolume: setStreamVolume, setSink: setSink, setNativeStreamAudio: setNativeStreamAudio, setStatsEnabled: setStatsEnabled, startCamera: startCamera, stopCamera: stopCamera, listCameras: listCameras, attachLocalCamera: attachLocalCamera, setE2eeKey: setE2eeKey };
})();
"#;

pub const QUALITY_PRESETS: &[(&str, &str, &str)] = &[
    (
        "720",
        "720p — 30 FPS",
        "Lower upload and processing requirements",
    ),
    (
        "4k",
        "4K — 2160p30",
        "Maximum detail; needs very strong upload",
    ),
    (
        "smooth",
        "Smooth — 1080p60",
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
        "Ultra — 1440p60",
        "High detail; needs strong upload",
    ),
];

fn quality_preset(id: &str) -> (u32, u32, u32, u32, &'static str, &'static str) {
    let (width, height, fps, hint, degradation) = match id {
        "720" => (1280, 720, 30, "motion", "balanced"),
        "4k" => (3840, 2160, 30, "detail", "maintain-resolution"),
        "smooth" => (1920, 1080, 60, "motion", "maintain-framerate"),
        "crisp" => (1920, 1080, 15, "detail", "maintain-resolution"),
        "ultra" => (2560, 1440, 60, "detail", "balanced"),
        _ => (1920, 1080, 30, "motion", "balanced"),
    };
    (
        width,
        height,
        fps,
        upload_budget(height, fps),
        hint,
        degradation,
    )
}

fn upload_budget(height: u32, fps: u32) -> u32 {
    match (height, fps) {
        (720, _) => 8_000_000,
        (1440, 60) => 34_000_000,
        (1440, _) => 21_000_000,
        (2160, 60) => 50_000_000,
        (2160, _) => 42_000_000,
        (_, 60) => 17_000_000,
        _ => 14_000_000,
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
    let (width, height, fps, bitrate, _hint, degradation) = quality_preset(quality);
    crate::sysvideo::Settings {
        width,
        height,
        fps,
        max_bitrate: bitrate as u64,
        priority: match degradation {
            "maintain-framerate" => crate::sysvideo::Priority::Motion,
            "maintain-resolution" => crate::sysvideo::Priority::Detail,
            _ => crate::sysvideo::Priority::Balanced,
        },
        codec: crate::sysvideo::Codec::default(),
        encoder: crate::sysvideo::Encoder::Auto,
    }
}

fn selected_capture_settings(
    settings: &crate::settings::ClientSettings,
) -> crate::sysvideo::Settings {
    let mut capture = native_settings(&settings.screenshare_quality);
    capture.codec = settings.screenshare_codec;
    capture.encoder = settings.screenshare_encoder;
    if let Some(fps @ (15 | 30 | 60)) = settings.screenshare_fps {
        capture.fps = fps;
    }
    capture.max_bitrate = u64::from(upload_budget(capture.height, capture.fps));
    if capture.fps == 60 && capture.priority == crate::sysvideo::Priority::Balanced {
        capture.priority = crate::sysvideo::Priority::Motion;
    }
    capture
}

fn picker_quality(saved: &str) -> String {
    if QUALITY_PRESETS.iter().any(|(id, _, _)| *id == saved) {
        saved.to_owned()
    } else {
        "balanced".into()
    }
}

pub(crate) fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

pub fn share_js(on: bool, quality: &str, audio: bool) -> String {
    if !on {
        return "window.dxScreen.stopShare();".into();
    }
    let (w, h, fps, bitrate, hint, degradation) = quality_preset(quality);
    let mode = native_audio_mode();
    let (hint, degradation, mode) = (js_str(hint), js_str(degradation), js_str(mode));
    format!(
        "window.dxScreen.requestAndStartShare({{width:{w},height:{h},fps:{fps},\
         bitrate:{bitrate},hint:{hint},degradation:{degradation},audio:{audio},nativeAudio:{mode}}});"
    )
}

pub(crate) fn screen_stats_js(enabled: bool) -> String {
    format!("window.__dxfScreenStatsEnabled={enabled};window.dxScreen?.setStatsEnabled({enabled});")
}

pub(crate) fn attach_js(identity: &str, container: &str, kind: &str) -> String {
    let (identity, container, kind) = (js_str(identity), js_str(container), js_str(kind));
    format!("window.dxScreen.attach({identity},{container},{kind});")
}

pub(crate) fn detach_js(container: &str) -> String {
    let container = js_str(container);
    format!("window.dxScreen.detach({container});")
}

pub fn stream_sink_js(device: Option<&str>) -> String {
    let arg = serde_json::to_string(&device).unwrap_or_else(|_| "null".into());
    format!("window.dxScreen.setSink({arg});")
}

#[component]
pub fn ScreenShareBridge() -> Element {
    let state = use_app_state();
    let gateway = use_gateway();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let sharing = use_memo(move || {
        let s = state.read();
        (s.screen_sharing, s.voice.channel_id)
    });
    let mut shared_channel = use_signal(|| None::<crate::protocol::Id>);
    let gateway_for_capture = gateway.clone();
    use_effect(move || {
        let (active, channel) = sharing();
        let previous = *shared_channel.peek();
        if active {
            shared_channel.set(channel);
        } else if let Some(channel_id) = previous {
            gateway_for_capture.send(ClientMessage::SetScreenShare {
                channel_id,
                sharing: false,
            });
            shared_channel.set(None);
        }
    });
    let lifecycle = use_hook(|| {
        let (targets, target_rx) =
            tokio::sync::watch::channel(None::<super::video_lifecycle::Target>);
        let (events, event_rx) =
            tokio::sync::mpsc::unbounded_channel::<super::video_lifecycle::Event>();
        (
            targets,
            target_rx,
            events,
            std::rc::Rc::new(std::cell::RefCell::new(Some(event_rx))),
        )
    });
    let token = use_memo(move || {
        let s = state.read();
        s.screen_token
            .as_ref()
            .map(|(url, token)| super::video_lifecycle::Target {
                url: url.clone(),
                token: token.clone(),
                voice_epoch: s.voice_session_epoch,
            })
    });
    let targets = lifecycle.0.clone();
    use_effect(move || {
        targets.send_replace(token());
    });
    let target_rx = lifecycle.1.clone();
    let event_rx = lifecycle.3.clone();
    use_future(move || {
        let targets = target_rx.clone();
        let events = event_rx.borrow_mut().take();
        async move {
            if let Some(events) = events {
                super::video_lifecycle::run(targets, events, |action| {
                    let js = match action {
                        super::video_lifecycle::Action::Connect { target, generation } => {
                            let (url, token) = (js_str(&target.url), js_str(&target.token));
                            let key = crate::e2ee::current_key().map(|k| js_str(&k)).unwrap_or_else(|| "null".into());
                            let encrypt = crate::e2ee::enabled();
                            format!("window.dxScreen.connect({url},{token},{key},{encrypt},{generation});")
                        }
                        super::video_lifecycle::Action::Disconnect => "window.dxScreen.disconnect();".into(),
                    };
                    let _ = document::eval(&js);
                }).await;
            }
        }
    });
    use_drop(move || {
        let _ = document::eval("window.dxScreen.disconnect();");
    });
    let lifecycle_events = lifecycle.2.clone();

    let native_audio_live = use_memo(move || state.read().screen_audio_joined);
    use_effect(move || {
        let on = native_audio_live();
        crate::dlog!("screen setNativeStreamAudio({on})");
        let _ = document::eval(&format!("window.dxScreen.setNativeStreamAudio({on});"));
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
            crate::sysvideo::Settings,
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
        let capture_settings = selected_capture_settings(&settings.read());

        let now = (
            epoch,
            want,
            publish_system,
            joined,
            want_video,
            target,
            capture_settings,
        );
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
                settings: capture_settings,
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
        let lifecycle_events = lifecycle_events.clone();
        async move {
            let bridge_js = r#"
            window.__dxfShareSink = function (m) { try { dioxus.send(m); } catch (err) {} };
            if (!window.__dxfShareEndWired) {
              window.__dxfShareEndWired = true;
              window.addEventListener('message', function (e) {
                var d = e.data;
                if (d && (d.__dxf === 'screen-share-ended' || d.__dxf === 'share-started' || d.__dxf === 'share-audio' || d.__dxf === 'share-unavailable' || d.__dxf === 'share-echo-risk' || d.__dxf === 'stream-audio' || d.__dxf === 'screen-stats' || d.__dxf === 'screen-stats-in' || d.__dxf === 'screen-room-state' || d.__dxf === 'screen-room-error' || d.__dxf === 'screen-room-reconnecting' || d.__dxf === 'screen-track-timeout' || d.__dxf === 'e2ee-error' || d.__dxf === 'e2ee-undecryptable') && window.__dxfShareSink) {
                  window.__dxfShareSink(d);
                }
              });
            }
            "#;
            let mut eval = document::eval(bridge_js);
            let mut stats = super::screen_stats::WebViewStats::default();
            while let Ok(msg) = eval.recv::<Value>().await {
                match msg.get("__dxf").and_then(|v| v.as_str()) {
                    Some("screen-room-state") => {
                        if let Ok(event) =
                            serde_json::from_value::<super::video_lifecycle::Event>(msg.clone())
                            && lifecycle_events.send(event).is_err()
                        {
                            tracing::debug!("video lifecycle owner stopped");
                        }
                    }
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
                    Some("screen-stats") | Some("screen-stats-in") => {
                        let outbound =
                            msg.get("__dxf").and_then(Value::as_str) == Some("screen-stats");
                        let screen_stats = stats.update(&msg, outbound);
                        tracing::debug!(
                            ?screen_stats,
                            track_sid = msg.get("trackSid").and_then(serde_json::Value::as_str),
                            freeze_count =
                                msg.get("freezeCount").and_then(serde_json::Value::as_u64),
                            freeze_duration_secs = msg
                                .get("freezeDurationSeconds")
                                .and_then(serde_json::Value::as_f64),
                            frames_dropped =
                                msg.get("framesDropped").and_then(serde_json::Value::as_u64),
                            nack_count = msg.get("nackCount").and_then(serde_json::Value::as_u64),
                            pli_count = msg.get("pliCount").and_then(serde_json::Value::as_u64),
                            key_frames_decoded = msg
                                .get("keyFramesDecoded")
                                .and_then(serde_json::Value::as_u64),
                            "webview screen share stats"
                        );
                        let mut s = state.write();
                        if outbound {
                            if !(crate::sysvideo::supported() && s.screen_sharing) {
                                s.screen_share_stats = screen_stats;
                            }
                        } else {
                            s.screen_share_in_stats = screen_stats;
                        }
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
                             The stream room was not joined, so nothing was sent in the clear; \
                             leave and rejoin the channel to try again."
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
                            s.stream_has_audio.set(
                                crate::stream_audio::Source::WebView,
                                id,
                                present,
                            );
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
    let state = use_app_state();
    let picker = use_memo(move || state.read().screen_picker.clone());
    match picker() {
        Some(result) => rsx! { ScreenShareDialog { result } },
        None => rsx! {},
    }
}

#[component]
fn ScreenShareDialog(result: Result<Vec<crate::sysvideo::Source>, String>) -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let mut settings = use_context::<Signal<crate::settings::ClientSettings>>();
    let mut target = use_signal(|| None::<crate::sysvideo::Target>);
    let mut preview_revision = use_signal(|| 0_u64);
    let mut codec = use_signal(move || settings.read().screenshare_codec);
    let mut encoder = use_signal(move || settings.read().screenshare_encoder);
    let mut quality = use_signal(move || picker_quality(&settings.read().screenshare_quality));
    let mut fps = use_signal(move || selected_capture_settings(&settings.read()).fps);
    let mut audio =
        use_signal(move || settings.read().screenshare_audio && crate::sysaudio::supported());
    let close = move |_| {
        state.write().screen_picker = None;
    };
    let sources = result.as_ref().ok().cloned().unwrap_or_default();
    let selected_source = sources
        .iter()
        .find(|source| Some(source.target) == target())
        .cloned();
    let can_share = selected_source.is_some() && state.read().voice.channel_id.is_some();

    rsx! {
        div {
            class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/50",
            onclick: close,
            div {
                class: "dxf-modal-in max-h-[80vh] flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                style: "width: min(52rem, calc(100vw - 2rem));",
                onclick: move |e| e.stop_propagation(),
                div { class: "px-4 py-3 border-b border-[var(--border)] flex items-center",
                    h3 { class: "text-sm font-medium text-[var(--accent)] flex-1", "Share your screen" }
                    button { class: "text-xs text-[var(--text-dim)] hover:text-[var(--text)] mr-2",
                        onclick: move |_| preview_revision += 1,
                        "Refresh previews"
                    }
                    button { class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                        onclick: close, "✕"
                    }
                }
                div { class: "flex-1 overflow-y-auto p-3 space-y-3",
                    if let Err(error) = &result {
                        div { class: "text-xs text-[var(--danger)]", "{error}" }
                        if cfg!(target_os = "macos") {
                            p { class: "text-xs text-[var(--text-muted)]",
                                "Allow Screen Recording in System Settings › Privacy & Security, then reopen Discordia."
                            }
                        }
                    } else if sources.is_empty() {
                        div { class: "text-xs text-[var(--text-muted)]", "No screens or windows available yet." }
                    }
                    for (heading, windows) in [("Screens & apps", false), ("Windows", true)] {
                        if sources.iter().any(|source| source.app.is_some() == windows) {
                            div { class: "space-y-1",
                                div { class: "text-[10px] font-semibold uppercase tracking-wider text-[var(--text-muted)] mb-1.5", "{heading}" }
                                div { style: "display: grid; grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); gap: 12px;",
                                for source in sources.iter().filter(|source| source.app.is_some() == windows) {
                                    {
                                        let selected = target() == Some(source.target);
                                        let source_target = source.target;
                                        let source_key = format!("{:?}-{}", source.target, preview_revision());
                                        rsx! {
                                            ScreenSourceTile {
                                                key: "{source_key}",
                                                source: source.clone(),
                                                selected,
                                                onselect: move |_| target.set(Some(source_target)),
                                            }
                                        }
                                    }
                                } }
                            }
                        }
                    }
                }
                div { class: "px-4 py-3 border-b border-[var(--border)] space-y-3",
                    div { style: "display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px;",
                        div { class: "min-w-0 space-y-1",
                            label { r#for: "share-quality", class: "text-xs text-[var(--text-muted)]", "Resolution" }
                            select {
                                id: "share-quality",
                                class: "w-full bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                                onchange: move |e| quality.set(e.value()),
                                for (id, label) in [("720", "720p"), ("balanced", "1080p"), ("ultra", "1440p"), ("4k", "4K")] {
                                    option { value: "{id}", selected: native_settings(&quality()).width == native_settings(id).width, "{label}" }
                                }
                            }
                        }
                        div { class: "min-w-0 space-y-1",
                            label { r#for: "share-fps", class: "text-xs text-[var(--text-muted)]", "Frames per second" }
                            select {
                                id: "share-fps",
                                class: "w-full bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                                onchange: move |e| { if let Ok(value @ (15 | 30 | 60)) = e.value().parse::<u32>() { fps.set(value); } },
                                for value in [15, 30, 60] {
                                    option { value: "{value}", selected: fps() == value, "{value} FPS" }
                                }
                            }
                        }
                        div { class: "min-w-0 space-y-1",
                            label { r#for: "share-codec", class: "text-xs text-[var(--text-muted)]", "Video codec" }
                            select {
                                id: "share-codec",
                                class: "w-full bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                                onchange: move |e| codec.set(if e.value() == "vp8" { crate::sysvideo::Codec::Vp8 } else { crate::sysvideo::Codec::H264 }),
                                option { value: "h264", selected: codec() == crate::sysvideo::Codec::H264, "H.264" }
                                option { value: "vp8", selected: codec() == crate::sysvideo::Codec::Vp8, disabled: encoder() == crate::sysvideo::Encoder::Gpu, "VP8 — compatibility" }
                            }
                        }
                        div { class: "min-w-0 space-y-1",
                            label { r#for: "share-encoder", class: "text-xs text-[var(--text-muted)]", "Video encoding" }
                            select {
                                id: "share-encoder",
                                class: "w-full bg-[var(--panel-solid)] text-[var(--text)] border border-[var(--border)] rounded px-2 py-1 text-sm",
                                onchange: move |e| {
                                    let selected = match e.value().as_str() {
                                        "gpu" => crate::sysvideo::Encoder::Gpu,
                                        "cpu" => crate::sysvideo::Encoder::Cpu,
                                        _ => crate::sysvideo::Encoder::Auto,
                                    };
                                    if selected == crate::sysvideo::Encoder::Gpu {
                                        codec.set(crate::sysvideo::Codec::H264);
                                    }
                                    encoder.set(selected);
                                },
                                option { value: "auto", selected: encoder() == crate::sysvideo::Encoder::Auto, "Automatic — prefer {crate::sysvideo::hardware_encoder_label()}" }
                                option { value: "gpu", selected: encoder() == crate::sysvideo::Encoder::Gpu,
                                    if cfg!(target_os = "macos") { "Hardware" } else { "GPU — hardware" }
                                }
                                option { value: "cpu", selected: encoder() == crate::sysvideo::Encoder::Cpu, "CPU — software" }
                            }
                        }
                    }
                    label { class: "flex items-center gap-2 cursor-pointer select-none",
                        input { r#type: "checkbox", checked: audio(), disabled: !crate::sysaudio::supported(),
                            onchange: move |e| audio.set(e.checked()),
                        }
                        span { class: "text-xs text-[var(--text-muted)]",
                            if crate::sysaudio::captures_application(target()) { "Share application audio" }
                            else { "Share computer audio" }
                        }
                    }
                    if audio() {
                        p { class: "text-[10px] text-[var(--text-dim)]",
                            if crate::sysaudio::captures_application(target()) {
                                "Shares audio from the selected application. Other windows or tabs of that application may be included. Discordia audio is excluded."
                            } else {
                                "Shares computer audio from other applications. Discordia audio is excluded."
                            }
                        }
                    }
                    p { class: "text-[10px] text-[var(--text-dim)]",
                        "Higher resolution and FPS require more upload bandwidth."
                    }
                }
                div { class: "px-4 py-3 flex items-center gap-2",
                    span { class: "text-xs text-[var(--text-muted)] flex-1 truncate",
                        if let Some(source) = selected_source { "{source.title}" } else { "Select a screen or window" }
                    }
                    button { class: "px-3 py-2 rounded border border-[var(--border)] text-[var(--text-muted)] text-sm",
                        onclick: close, "Cancel"
                    }
                    button {
                        class: "px-3 py-2 rounded border border-[var(--accent)] bg-[var(--accent-soft)] text-[var(--accent)] text-sm disabled:opacity-40",
                        disabled: !can_share,
                        onclick: move |_| {
                            if let Some(chosen) = target() {
                                let mut next = settings.read().clone();
                                next.screenshare_quality = quality();
                                next.screenshare_fps = Some(fps());
                                next.screenshare_codec = codec();
                                next.screenshare_encoder = encoder();
                                next.screenshare_audio = audio();
                                settings.set(next.clone());
                                crate::settings::save(&next);
                                choose_source(state, gateway.clone(), settings, chosen);
                            }
                        },
                        "Share"
                    }
                }
            }
        }
    }
}

#[component]
fn ScreenSourceTile(
    source: crate::sysvideo::Source,
    selected: bool,
    onselect: EventHandler<()>,
) -> Element {
    let target = source.target;
    let preview = use_resource(move || async move { crate::sysvideo::thumbnail(target).await });
    let image = preview.read().clone();
    rsx! {
        button {
            class: "w-full text-left rounded border overflow-hidden transition-colors",
            style: if selected { "border-color: var(--accent); background-color: var(--accent-soft);" } else { "border-color: var(--border);" },
            aria_pressed: selected,
            title: "{source.title}",
            onclick: move |_| onselect.call(()),
            div {
                style: "aspect-ratio: 16 / 9; background-color: #000; display: flex; align-items: center; justify-content: center;",
                match image {
                    Some(Ok(data)) => rsx! { img { src: data, alt: "Preview of {source.title}",
                        style: "width: 100%; height: 100%; object-fit: contain;",
                    } },
                    Some(Err(_)) => rsx! { span { class: "text-xs text-[var(--text-dim)]", "Preview unavailable" } },
                    None => rsx! { span { class: "text-xs text-[var(--text-dim)]", "Loading preview…" } },
                }
            }
            div { class: "px-2 py-1.5",
                if let Some(app) = &source.app {
                    div { class: "text-[10px] text-[var(--accent)] truncate", "{app}" }
                }
                div { class: "text-xs text-[var(--text)] truncate", "{source.title}" }
                if source.width > 0 {
                    div { class: "text-[10px] text-[var(--text-dim)] font-mono", "{source.width}×{source.height}" }
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
    if !crate::sysvideo::supported() {
        return;
    }
    dioxus::prelude::spawn(async move {
        let found = tokio::task::spawn_blocking(crate::sysvideo::sources)
            .await
            .unwrap_or_else(|e| Err(format!("listing screens failed: {e}")));
        if state.peek().screen_picker.is_some() {
            state.write().screen_picker = Some(found);
        }
    });
}

fn screen_encoder_label(implementation: Option<&str>) -> String {
    let Some(name) = implementation
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return "Encoder: unknown".into();
    };
    let normalized = name.to_ascii_lowercase();
    if normalized.contains("openh264") || normalized.contains("libvpx") {
        format!("CPU · {name}")
    } else if normalized.starts_with("nvidia ")
        || normalized.starts_with("media foundation h264 encoder")
        || normalized.starts_with("vaapi ")
        || normalized.starts_with("jetson mmapi ")
        || normalized.contains("videotoolbox")
    {
        format!("{} · {name}", crate::sysvideo::hardware_encoder_label())
    } else {
        format!("Encoder: {name}")
    }
}

#[component]
pub fn ScreenSelfPreview() -> Element {
    let mut state = use_app_state();
    let gateway = use_gateway();
    let settings = use_context::<Signal<crate::settings::ClientSettings>>();

    let mut px = use_signal(|| 968.0_f64);
    let mut py = use_signal(|| 56.0_f64);
    let mut pw = use_signal(|| 300.0_f64);
    let mut ph = use_signal(|| 208.0_f64);
    let mut drag = use_stream_drag();

    let sharing = use_memo(move || state.read().screen_sharing);
    let self_pk = use_memo(move || state.read().self_user.as_ref().map(|u| u.pubkey.clone()));

    let native_capture = crate::sysvideo::supported();
    let share_label = use_memo(move || match state.read().screen_share_target {
        Some(crate::sysvideo::Target::Display(_)) => "Sharing your screen",
        Some(crate::sysvideo::Target::Window(_)) => "Sharing one window",
        Some(crate::sysvideo::Target::Application(_)) => "Sharing an app",
        #[cfg(target_os = "windows")]
        Some(crate::sysvideo::Target::WindowsMonitor(_)) => "Sharing your screen",
        #[cfg(target_os = "windows")]
        Some(crate::sysvideo::Target::WindowsWindow(_)) => "Sharing one window",
        None => "Sharing your screen",
    });
    let frames = use_memo(move || {
        state
            .read()
            .screen_share_stats
            .as_ref()
            .and_then(|stats| stats.capture_width)
            .unwrap_or(0)
    });

    let window = dioxus::desktop::use_window();
    let mut preview_active = use_signal(|| {
        window.window.is_focused() && window.window.is_visible() && !window.window.is_minimized()
    });
    let window_id = window.window.id();
    dioxus::desktop::use_wry_event_handler(move |event, _| {
        use dioxus::desktop::tao::event::{Event, WindowEvent};
        if let Event::WindowEvent {
            window_id: id,
            event,
            ..
        } = event
            && *id == window_id
        {
            let active = match event {
                WindowEvent::Focused(focused) => {
                    Some(*focused && window.window.is_visible() && !window.window.is_minimized())
                }
                WindowEvent::Resized(_) => Some(
                    window.window.is_focused()
                        && window.window.is_visible()
                        && !window.window.is_minimized(),
                ),
                _ => None,
            };
            if let Some(active) = active
                && active != *preview_active.peek()
            {
                preview_active.set(active);
            }
        }
    });
    let mut preview_task = use_signal(|| None::<dioxus::core::Task>);
    let mut applied_preview = use_signal(|| None::<(Option<String>, bool)>);
    use_effect(move || {
        let identity = if sharing() { self_pk() } else { None };
        let enabled = preview_active();
        if !enabled {
            drag.set(None);
        }
        if let Some(task) = preview_task.write().take() {
            task.cancel();
        }
        let desired = (identity, enabled);
        let previous = applied_preview.peek().clone();
        let Some(delay) = preview_subscription_delay(previous.as_ref(), &desired) else {
            return;
        };
        preview_task.set(Some(spawn(async move {
            // Brief focus changes must not resubscribe the preview and request another key frame.
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            let (identity, enabled) = &desired;
            tracing::debug!(
                enabled,
                sharing = identity.is_some(),
                "self screen preview state"
            );
            let _ = document::eval(&format!(
                "window.dxScreen.setSelfPreview({},{enabled});",
                serde_json::to_string(identity).unwrap_or_else(|_| "null".into()),
            ));
            applied_preview.set(Some(desired));
        })));
    });
    use_drop(move || {
        if let Some(task) = preview_task.write().take() {
            task.cancel();
        }
        let _ = document::eval("window.dxScreen.setSelfPreview(null,true);");
    });

    if !sharing() {
        return rsx! { Fragment {} };
    }

    let configured = selected_capture_settings(&settings.read());
    let requested_label = format!(
        "Selected: {}×{} · {} FPS",
        configured.width, configured.height, configured.fps
    );
    let actual_label = state
        .read()
        .screen_share_stats
        .as_ref()
        .map(|stats| {
            format!(
                "Sending: {}×{} · {:.0} FPS · capture {:.0} FPS · {} · {}",
                stats.encoded_width.map_or("—".into(), |v| v.to_string()),
                stats.encoded_height.map_or("—".into(), |v| v.to_string()),
                stats.encoded_fps.unwrap_or(0.0),
                stats.capture_fps.unwrap_or(0.0),
                stats.codec.as_deref().unwrap_or("—"),
                screen_encoder_label(stats.codec_implementation.as_deref())
            )
        })
        .unwrap_or_else(|| "Waiting for stream measurements…".into());

    rsx! {
        if drag().is_some() {
            div {
                class: "fixed inset-0 z-50",
                style: "z-index:{STREAM_DRAG_LAYER};",
                onmousemove: move |e| {
                    if !e.held_buttons().contains(dioxus::html::input_data::MouseButton::Primary) {
                        drag.set(None);
                        return;
                    }
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
                    if !e.held_buttons().contains(dioxus::html::input_data::MouseButton::Primary) { return; }
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Move { dx: c.x - px(), dy: c.y - py() }));
                },
                span { class: "w-2 h-2 rounded-full shrink-0", style: "background: var(--danger);" }
                span { class: "text-[11px] text-[var(--text)] truncate flex-1", "{requested_label}" }
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
                class: "relative flex-1 min-h-0 bg-black",
                div {
                    id: "screenshare-self",
                    style: "width: 100%; height: 100%; display: flex; align-items: center; justify-content: center;",
                    class: "text-[var(--text-dim)] text-[10px]",
                    "Starting…"
                }
                if !preview_active() {
                    div {
                        class: "absolute inset-0 flex items-center justify-center bg-black text-[var(--text-dim)] text-[10px]",
                        "Preview paused while the app is in the background"
                    }
                }
                if native_capture {
                    div {
                        class: "absolute left-2 bottom-2 px-1.5 py-0.5 rounded bg-black/70 text-[9px] pointer-events-none",
                        if frames() > 0 {
                            span { class: "text-[var(--up)]", "{actual_label}" }
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
                    if !e.held_buttons().contains(dioxus::html::input_data::MouseButton::Primary) { return; }
                    let c = e.client_coordinates();
                    drag.set(Some(Drag::Resize { px: c.x, py: c.y, w0: pw(), h0: ph() }));
                },
            }
        }
    }
}

fn preview_subscription_delay(
    applied: Option<&(Option<String>, bool)>,
    desired: &(Option<String>, bool),
) -> Option<std::time::Duration> {
    if applied == Some(desired) {
        return None;
    }
    Some(
        if applied.is_some_and(|(identity, _)| identity.is_some() && identity == &desired.0) {
            std::time::Duration::from_secs(1)
        } else {
            std::time::Duration::ZERO
        },
    )
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Drag {
    Move { dx: f64, dy: f64 },
    Resize { px: f64, py: f64, w0: f64, h0: f64 },
}

const STREAM_DRAG_LAYER: u32 = 100;

fn stream_tile_style(
    detached: bool,
    fullscreen: bool,
    maximized: bool,
    tiled: bool,
    rect: [f64; 4],
) -> String {
    // Dioxus retains omitted inline properties, so every mode resets the
    // complete layout, especially the layer above the drag surface.
    let (position, layer, radius, left, top, width, height) = if fullscreen {
        (
            "fixed",
            60,
            0,
            "0".into(),
            "0".into(),
            "100vw".into(),
            "100vh".into(),
        )
    } else if tiled {
        (
            "relative",
            0,
            8,
            "auto".into(),
            "auto".into(),
            "100%".into(),
            "100%".into(),
        )
    } else if maximized {
        (
            "fixed",
            50,
            8,
            "12px".into(),
            "12px".into(),
            "calc(100vw - 24px)".into(),
            "calc(100vh - 24px)".into(),
        )
    } else {
        let [x, y, w, h] = rect;
        (
            "fixed",
            40,
            8,
            format!("{x}px"),
            format!("{y}px"),
            format!("{w}px"),
            format!("{h}px"),
        )
    };
    let display = if detached { "none" } else { "flex" };
    format!(
        "display:{display};position:{position};z-index:{layer};border-top-left-radius:{radius}px;border-top-right-radius:{radius}px;border-bottom-left-radius:{radius}px;border-bottom-right-radius:{radius}px;left:{left};top:{top};right:auto;bottom:auto;min-width:0;min-height:0;width:{width};height:{height};"
    )
}

fn use_stream_drag() -> Signal<Option<Drag>> {
    let mut drag = use_signal(|| None::<Drag>);
    #[cfg(target_os = "windows")]
    use_effect(move || {
        if drag().is_none() {
            return;
        }
        spawn(async move {
            while drag.peek().is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(32)).await;
                if drag.peek().is_some() && !primary_mouse_button_down() {
                    drag.set(None);
                }
            }
        });
    });
    let window_id = dioxus::desktop::use_window().window.id();
    dioxus::desktop::use_wry_event_handler(move |event, _| {
        use dioxus::desktop::tao::event::{ElementState, Event, WindowEvent};
        use dioxus::desktop::tao::keyboard::Key;
        if let Event::WindowEvent {
            window_id: id,
            event,
            ..
        } = event
            && *id == window_id
            && drag.peek().is_some()
        {
            let cancel = match event {
                WindowEvent::Focused(false) | WindowEvent::CursorLeft { .. } => true,
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: dioxus::desktop::tao::event::MouseButton::Left,
                    ..
                } => true,
                WindowEvent::KeyboardInput { event, .. } => {
                    event.state == ElementState::Pressed && event.logical_key == Key::Escape
                }
                _ => false,
            };
            if cancel {
                drag.set(None);
            }
        }
    });
    drag
}

#[cfg(target_os = "windows")]
fn primary_mouse_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};

    // SAFETY: These queries accept constant identifiers and do not access caller memory.
    unsafe {
        let button = if GetSystemMetrics(SM_SWAPBUTTON) != 0 {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        GetAsyncKeyState(i32::from(button.0)) < 0
    }
}

#[component]
pub fn ScreenWatchWindow() -> Element {
    let state = use_app_state();
    let popouts = use_context::<super::stream_viewer::Popouts>();
    let detached = popouts.detached;
    let mut focused = use_signal::<Option<String>>(|| None);
    let viewing = use_memo(move || state.read().screen_viewing.clone());
    let mut fullscreen = use_signal::<Option<String>>(|| None);
    let mut was_fullscreen = use_signal(|| false);
    let window = dioxus::desktop::use_window();
    let original_fullscreen = use_hook({
        let window = window.clone();
        move || window.window.fullscreen()
    });
    let fullscreen_window = window.clone();
    let mut size = use_signal(|| (800.0, 500.0));
    let restore_fullscreen = original_fullscreen.clone();
    use_effect(move || {
        let active = fullscreen().is_some();
        if active != *was_fullscreen.peek() {
            fullscreen_window.window.set_fullscreen(if active {
                Some(dioxus::desktop::tao::window::Fullscreen::Borderless(None))
            } else {
                restore_fullscreen.clone()
            });
            was_fullscreen.set(active);
        }
    });
    let window_id = window.window.id();
    dioxus::desktop::use_wry_event_handler(move |event, _| {
        use dioxus::desktop::tao::event::{ElementState, Event, WindowEvent};
        if let Event::WindowEvent {
            window_id: id,
            event: WindowEvent::KeyboardInput { event, .. },
            ..
        } = event
            && *id == window_id
            && event.state == ElementState::Pressed
            && event.logical_key == dioxus::desktop::tao::keyboard::Key::Escape
        {
            fullscreen.set(None);
            focused.set(None);
        }
    });
    use_effect(move || {
        let watched = viewing.read();
        let detached = detached.read();
        if watched.iter().filter(|pk| !detached.contains(*pk)).count() <= 1
            && focused.peek().is_some()
        {
            focused.set(None);
        }
        for mut selection in [focused, fullscreen] {
            let invalid = selection
                .peek()
                .as_ref()
                .is_some_and(|pk| !watched.contains(pk) || detached.contains(pk));
            if invalid {
                selection.set(None);
            }
        }
    });
    let close_window = window.clone();
    use_drop(move || {
        if *was_fullscreen.peek() {
            close_window.window.set_fullscreen(original_fullscreen);
        }
    });

    let watching = use_memo(move || state.read().screen_viewing.clone());
    let stream_levels = use_memo(move || {
        let s = state.read();
        (s.stream_volumes.clone(), s.stream_muted.clone())
    });
    let voice_for_stream = use_voice_tx();
    let mut last_gains = use_signal(Vec::<(String, f32)>::new);
    let mut last_gain_epoch = use_signal(|| None::<u64>);
    use_effect(move || {
        let watched = watching();
        let _ = stream_levels();
        let s = state.read();
        let epoch = s.voice_session_epoch;
        let desired = crate::stream_audio::playback_gains(&s, &last_gains.peek());
        drop(s);
        if *last_gains.peek() == desired && *last_gain_epoch.peek() == Some(epoch) {
            return;
        }
        crate::dlog!(
            "watch gains changed watched={:?} gains={:?}",
            watched,
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
            let _ = document::eval(&format!(
                "window.dxScreen?.setStreamVolume({gain},{});",
                js_str(pk)
            ));
        }
        last_gains.set(desired);
        last_gain_epoch.set(Some(epoch));
    });

    let output_device = use_memo(move || state.read().selected_output_device.clone());
    use_effect(move || {
        let _ = document::eval(&stream_sink_js(output_device().as_deref()));
    });

    let mut watched: Vec<String> = viewing().into_iter().collect();
    watched.sort();
    let count = watched
        .iter()
        .filter(|pk| !detached.read().contains(*pk))
        .count();
    let selected = fullscreen().or(if count > 1 { focused() } else { None });
    let (width, height) = size();
    let grid =
        super::stream_viewer::grid_style(if selected.is_some() { 1 } else { count }, width, height);
    rsx! {
        if count > 0 { dioxus_grid_layout::GridItem { id: "streams", x: 3, y: 0, w: 7, h: 22,
            min_w: 3, min_h: 6, overlay: fullscreen().is_some(),
            div { class: "panel-hover w-full h-full min-h-0 flex flex-col bg-[var(--panel-solid)] border border-[var(--border)] rounded-xl overflow-hidden",
                div { class: "h-12 shrink-0 px-4 flex items-center gap-2 border-b border-[var(--border)]",
                    span { class: "text-sm font-semibold text-[var(--text)]", "Streams" }
                    span { class: "text-xs text-[var(--text-muted)]", "{count} live" }
                    if count > 1 && selected.is_some() {
                        button { class: "ml-auto text-xs px-2 py-1 rounded border border-[var(--border)] text-[var(--text)]",
                            onclick: move |_| { focused.set(None); fullscreen.set(None); }, "Mosaic" }
                    }
                }
                div { class: "flex-1 min-h-0 grid gap-2 p-2", style: "{grid}",
                    onmounted: move |event| {
                        let data = event.data();
                        spawn(async move {
                            if let Ok(rect) = data.get_client_rect().await {
                                size.set((rect.size.width, rect.size.height));
                            }
                        });
                    },
                    onresize: move |event| {
                        if let Ok(box_size) = event.get_content_box_size() {
                            size.set((box_size.width, box_size.height));
                        }
                    },
                    for pk in watched {
                        ScreenWatchTile { key: "{pk}", is_detached: detached.read().contains(&pk),
                            hidden: selected.as_ref().is_some_and(|selected| selected != &pk),
                            pubkey: pk, focused, fullscreen, multiple_streams: count > 1, on_popout: popouts.open }
                    }
                }
            }
        } }
    }
}

#[component]
fn ScreenWatchTile(
    pubkey: String,
    mut focused: Signal<Option<String>>,
    mut fullscreen: Signal<Option<String>>,
    hidden: bool,
    is_detached: bool,
    multiple_streams: bool,
    on_popout: EventHandler<String>,
) -> Element {
    let mut state = use_app_state();
    let pk = pubkey;
    let container = format!("screenshare-viewer-{pk}");
    let stats_pk = pk.clone();
    let mut received_label = use_signal(|| "Waiting for stream measurements…".to_owned());
    let window = dioxus::desktop::use_window();
    use_future(move || {
        let identity = js_str(&stats_pk);
        let window = window.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                if !window.window.is_visible() || window.window.is_minimized() {
                    continue;
                }
                let mut eval = document::eval(&format!(
                    "dioxus.send(await window.dxScreen.previewStats({identity}));"
                ));
                if let Ok(value) = eval.recv::<Value>().await {
                    let label = match (
                        value["width"].as_u64(),
                        value["height"].as_u64(),
                        value["fps"].as_f64(),
                    ) {
                        (Some(width), Some(height), Some(fps)) => {
                            format!("Received: {width}×{height} · {fps:.0} FPS")
                        }
                        _ => "Waiting for stream measurements…".into(),
                    };
                    if label != *received_label.peek() {
                        received_label.set(label);
                    }
                }
            }
        }
    });
    let attach_pk = pk.clone();
    let attach_container = container.clone();
    use_effect(move || {
        let _ = document::eval(&attach_js(&attach_pk, &attach_container, "screen"));
    });
    let detach_container = container.clone();
    let detach_pk = pk.clone();
    use_drop(move || {
        if fullscreen.peek().as_ref() == Some(&detach_pk) {
            fullscreen.set(None);
        }
        let _ = document::eval(&detach_js(&detach_container));
    });
    let is_fullscreen = fullscreen().as_ref() == Some(&pk);
    let selected = multiple_streams && focused().as_ref() == Some(&pk);
    let can_focus = multiple_streams && !is_fullscreen;
    let layout = stream_tile_style(is_detached || hidden, false, false, true, [0.0; 4]);

    let name = state.read().display_name(&pk);

    let stream_volume = state.read().stream_volumes.get(&pk).copied().unwrap_or(100);
    let stream_muted = state.read().stream_muted.contains(&pk);
    let has_audio = state.read().stream_has_audio.contains(&pk);
    let pk_vol = pk.clone();
    let pk_mute = pk.clone();
    let pk_fullscreen = pk.clone();
    let pk_popout = pk.clone();
    let pk_focus = pk.clone();
    let pk_focus_key = pk.clone();

    rsx! {
        div {
            class: "flex flex-col min-w-0 min-h-0 bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg overflow-hidden",
            style: "{layout}",
            div {
                class: "min-h-9 px-3 flex flex-wrap items-center gap-2 border-b border-[var(--border)] shrink-0 select-none",
                span { class: "w-2.5 h-2.5 rounded-full shrink-0", style: "background: var(--danger);" }
                span { class: "min-w-0 flex-1 text-sm text-[var(--text)] font-medium truncate", "{name}'s screen" }
                span { class: "text-[10px] uppercase tracking-wider text-[var(--danger)] font-semibold", "Live" }
                button {
                    r#type: "button",
                    class: "w-7 h-7 flex items-center justify-center rounded text-[var(--text-dim)] hover:text-[var(--text)]",
                    title: "Open in separate window",
                    aria_label: "Open in separate window",
                    onmousedown: move |e| e.stop_propagation(),
                    onclick: move |_| { focused.set(None); fullscreen.set(None); on_popout.call(pk_popout.clone()); },
                    dangerous_inner_html: crate::features::icons::WINDOW_POP_OUT,
                }
                button {
                    r#type: "button",
                    class: "w-7 h-7 flex items-center justify-center rounded text-[var(--text-dim)] hover:text-[var(--text)]",
                    title: if is_fullscreen { "Exit full screen (Esc)" } else { "Full screen" },
                    aria_label: if is_fullscreen { "Exit full screen" } else { "Full screen" },
                    onmousedown: move |e| e.stop_propagation(),
                    onclick: move |_| {
                        focused.set(None);
                        fullscreen.set(if is_fullscreen { None } else { Some(pk_fullscreen.clone()) });
                    },
                    if is_fullscreen { "⤡" } else { "⤢" }
                }
                button {
                    class: "text-[var(--text-dim)] hover:text-[var(--text)] text-lg leading-none",
                    onmousedown: move |e| e.stop_propagation(),
                    title: "Stop watching",
                    aria_label: "Stop watching",
                    onclick: move |_| { state.write().screen_viewing.remove(&pk); },
                    "✕"
                }
            }
            div {
                id: "{container}",
                class: "flex-1 min-h-0 bg-black flex items-center justify-center text-[var(--text-dim)] text-sm",
                style: if can_focus { "cursor: pointer;" } else { "cursor: default;" },
                role: if can_focus { "button" } else { "group" },
                tabindex: if can_focus { "0" } else { "-1" },
                title: can_focus.then_some(if selected { "Return to mosaic" } else { "Focus stream" }),
                aria_label: if can_focus { if selected { "Return to mosaic" } else { "Focus stream" }.to_owned() } else { format!("{name}'s screen") },
                aria_pressed: can_focus.then(|| selected.to_string()),
                onmousedown: move |e| e.stop_propagation(),
                onclick: move |e| {
                    e.stop_propagation();
                    if can_focus {
                        focused.set(if selected { None } else { Some(pk_focus.clone()) });
                    }
                },
                onkeydown: move |e| {
                    if can_focus && (e.key() == Key::Enter || e.key() == Key::Character(" ".into())) {
                        e.prevent_default();
                        e.stop_propagation();
                        focused.set(if selected { None } else { Some(pk_focus_key.clone()) });
                    }
                },
                "Connecting to stream…"
            }
            div {
                class: "px-3 py-1 flex flex-wrap items-center justify-between gap-2 shrink-0",
                span { class: "text-[10px] text-[var(--text-dim)]", "{received_label}" }
                div {
                    class: "flex flex-wrap items-center gap-1.5",
                    onmousedown: move |e| e.stop_propagation(),
                    if !has_audio {
                        span {
                            class: "text-[10px] text-[var(--text-dim)] italic",
                            title: "No stream audio track has been detected yet.",
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
                        },
                    }
                    span { class: "text-[10px] text-[var(--text-dim)] w-8 text-right", "{stream_volume}%" }
                }
            }
        }
    }
}

#[cfg(test)]
mod js_escaping_tests {
    use super::{attach_js, js_str, screen_stats_js, share_js};

    #[test]
    fn restoring_a_stream_resets_properties_retained_by_the_renderer() {
        use std::collections::BTreeMap;
        let properties = |style: String| -> BTreeMap<String, String> {
            style
                .split(';')
                .filter_map(|property| property.split_once(':'))
                .map(|(key, value)| (key.into(), value.into()))
                .collect()
        };
        let rect = [160.0, 90.0, 880.0, 540.0];
        let floating = properties(super::stream_tile_style(false, false, false, false, rect));
        for (detached, fullscreen, maximized, tiled) in [
            (false, false, true, false),
            (false, true, false, false),
            (false, false, false, true),
            (true, false, false, false),
        ] {
            let mut retained = floating.clone();
            retained.extend(properties(super::stream_tile_style(
                detached, fullscreen, maximized, tiled, rect,
            )));
            retained.extend(properties(super::stream_tile_style(
                false, false, false, false, rect,
            )));
            assert_eq!(
                retained, floating,
                "restore must discard the previous layout mode"
            );
            assert_eq!(retained["z-index"], "40");
            assert_eq!(retained["position"], "fixed");
            assert_eq!(retained["display"], "flex");
            retained.extend(properties(super::stream_tile_style(
                false,
                false,
                false,
                false,
                [240.0, 180.0, 880.0, 540.0],
            )));
            assert_eq!(retained["left"], "240px");
            assert_eq!(retained["top"], "180px");
            assert!(retained["z-index"].parse::<u32>().expect("layer") < super::STREAM_DRAG_LAYER);
        }
    }

    #[test]
    fn brief_focus_changes_do_not_cycle_the_preview_subscription() {
        use std::time::Duration;
        let visible = (Some("self".into()), true);
        let hidden = (Some("self".into()), false);
        assert_eq!(
            super::preview_subscription_delay(None, &visible),
            Some(Duration::ZERO)
        );
        assert_eq!(
            super::preview_subscription_delay(Some(&visible), &hidden),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            super::preview_subscription_delay(Some(&visible), &visible),
            None
        );
        assert_eq!(
            super::preview_subscription_delay(Some(&hidden), &visible),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            super::preview_subscription_delay(Some(&visible), &(None, true)),
            Some(Duration::ZERO)
        );
        assert_eq!(
            super::preview_subscription_delay(Some(&hidden), &(Some("other".into()), true)),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn opening_the_picker_preserves_every_saved_quality_preset() {
        for (preset, _, _) in super::QUALITY_PRESETS {
            let saved = crate::settings::ClientSettings {
                screenshare_quality: (*preset).into(),
                ..Default::default()
            };
            let before = super::selected_capture_settings(&saved);
            let draft = crate::settings::ClientSettings {
                screenshare_quality: super::picker_quality(&saved.screenshare_quality),
                screenshare_fps: Some(before.fps),
                ..saved.clone()
            };
            assert_eq!(draft.screenshare_quality, saved.screenshare_quality);
            assert_eq!(super::selected_capture_settings(&draft), before);
        }
        assert_eq!(super::picker_quality("unknown"), "balanced");
    }

    #[test]
    fn fps_selection_preserves_resolution_and_selects_the_upload_budget() {
        for (quality, height, budget_30, budget_60) in [
            ("720", 720, 8_000_000, 8_000_000),
            ("balanced", 1080, 14_000_000, 17_000_000),
            ("ultra", 1440, 21_000_000, 34_000_000),
            ("4k", 2160, 42_000_000, 50_000_000),
        ] {
            for (fps, budget) in [(15, budget_30), (30, budget_30), (60, budget_60)] {
                let settings = crate::settings::ClientSettings {
                    screenshare_quality: quality.into(),
                    screenshare_fps: Some(fps),
                    ..Default::default()
                };
                let capture = super::selected_capture_settings(&settings);
                assert_eq!(
                    (capture.height, capture.fps, capture.max_bitrate),
                    (height, fps, budget)
                );
            }
            let defaults = super::native_settings(quality);
            let js = super::share_js(true, quality, false);
            assert!(
                js.lines()
                    .last()
                    .unwrap()
                    .contains(&format!("bitrate:{}", defaults.max_bitrate))
            );
        }
        let mut settings = crate::settings::ClientSettings {
            screenshare_quality: "4k".into(),
            screenshare_fps: Some(60),
            ..Default::default()
        };
        let capture = super::selected_capture_settings(&settings);
        assert_eq!(
            (capture.width, capture.height, capture.fps),
            (3840, 2160, 60)
        );
        assert_eq!(capture.max_bitrate, 50_000_000);
        settings.screenshare_fps = Some(0);
        assert_eq!(super::selected_capture_settings(&settings).fps, 30);
    }

    #[test]
    fn native_sixty_fps_selection_prioritizes_motion_and_preserves_codec() {
        let mut settings = crate::settings::ClientSettings {
            screenshare_quality: "balanced".into(),
            screenshare_fps: Some(60),
            screenshare_codec: crate::sysvideo::Codec::Vp8,
            ..Default::default()
        };
        let capture = super::selected_capture_settings(&settings);
        assert_eq!(capture.priority, crate::sysvideo::Priority::Motion);
        assert_eq!(capture.codec, crate::sysvideo::Codec::Vp8);
        settings.screenshare_quality = "crisp".into();
        assert_eq!(
            super::selected_capture_settings(&settings).priority,
            crate::sysvideo::Priority::Detail
        );
    }

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
        assert!(
            js.len() < 512,
            "share command should not include the bridge"
        );
        assert!(screen_stats_js(true).len() < 128);
        let js = super::SCREEN_JS;
        assert!(
          js.contains("screenShareEncoding: { maxBitrate: quality.bitrate || 6000000, maxFramerate: wantFps }"),
          "screen shares use the SDK's screen-share encoding option"
        );
        assert!(
            js.contains("videoCodec: 'h264'"),
            "screen shares request H.264 encoding"
        );
        assert!(
            js.contains("vt.contentHint = quality.hint || 'motion'"),
            "screen shares default to motion encoding"
        );
        assert!(
            js.contains("const track = localShareVideoTrack;")
                && js.contains("await track.getRTCStatsReport()")
                && js.contains("powerEfficient: typeof outbound.powerEfficientEncoder"),
            "screen-share diagnostics read sender stats including encoder efficiency"
        );
        assert!(
            js.contains("track.getRTCStatsReport()") && js.contains("entry.type === 'inbound-rtp'"),
            "screen-share diagnostics read received video stats"
        );
        assert!(
            screen_stats_js(true).ends_with("window.dxScreen?.setStatsEnabled(true);"),
            "diagnostics can enable sender stats polling"
        );
    }
}
