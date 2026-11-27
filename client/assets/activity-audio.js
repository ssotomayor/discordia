(function () {
  if (window.__dxfActivityAudio) return;
  let current = null, revision = 0, volume = 0.7;
  function report(s, status) {
    if (current === s) s.report({ request: s.request, status });
  }
  async function dispose(s) {
    if (!s) return;
    clearTimeout(s.timer);
    for (const [track, element] of s.tracks) {
      try { track.detach(element); } catch (_) {}
      element.remove();
    }
    s.tracks.clear();
    if (s.worker) s.worker.terminate();
    if (s.workerUrl) URL.revokeObjectURL(s.workerUrl);
    if (s.room) {
      let timeout;
      try { await Promise.race([s.room.disconnect(), new Promise(resolve => { timeout = setTimeout(resolve, 1500); })]); } catch (_) {}
      finally { clearTimeout(timeout); }
    }
    s.report({ request: s.request, status: 'stopped' });
  }
  function stop() {
    window.__dxfActivityAudioWanted = null;
    revision++;
    const old = current;
    current = null;
    return dispose(old);
  }
  function setVolume(value) {
    if (typeof value !== 'number' || !Number.isFinite(value)) return false;
    volume = Math.max(0, Math.min(1, value));
    if (current) for (const track of current.tracks.keys()) track.setVolume(volume);
    return true;
  }
  async function start(options, send) {
    const cancelled = () => {
      options.keyHex = ''; options.token = '';
      send({ request: options.request, status: 'stopped' });
    };
    if (options.request !== window.__dxfActivityAudioWanted) { cancelled(); return; }
    const ticket = ++revision;
    const old = current;
    current = null;
    await dispose(old);
    if (ticket !== revision || options.request !== window.__dxfActivityAudioWanted) { cancelled(); return; }
    const s = { request: options.request, report: send, tracks: new Map(), room: null };
    current = s;
    const fail = status => {
      if (current !== s) return;
      report(s, status);
      current = null;
      if (window.__dxfActivityAudioWanted === s.request) window.__dxfActivityAudioWanted = null;
      void dispose(s);
    };
    try {
      const sdk = window.LivekitClient || window.LiveKitClient;
      if (!sdk || !window.__dxfE2eeWorkerSrc || !/^[0-9a-f]{64}$/.test(options.keyHex)) throw new Error();
      s.workerUrl = URL.createObjectURL(new Blob([window.__dxfE2eeWorkerSrc], { type: 'application/javascript' }));
      s.worker = new Worker(s.workerUrl);
      s.worker.onerror = () => fail('encryption_failed');
      s.worker.addEventListener('message', event => {
        if (event.data && event.data.kind === 'error') fail('encryption_failed');
      });
      s.room = new sdk.Room({
        adaptiveStream: false, dynacast: false,
        e2ee: { keyProvider: new sdk.ExternalE2EEKeyProvider(), worker: s.worker },
      });
      const wanted = (publication, participant) => participant && participant.identity === options.publisher
        && publication.kind === 'audio' && publication.trackName === 'music';
      const subscribe = (publication, participant) => {
        if (current === s) publication.setSubscribed(!!wanted(publication, participant));
      };
      s.room.on(sdk.RoomEvent.TrackPublished, subscribe);
      s.room.on(sdk.RoomEvent.TrackSubscribed, (track, publication, participant) => {
        if (current !== s || !wanted(publication, participant) || track.kind !== 'audio' || s.tracks.has(track)) return;
        if (s.tracks.size) { publication.setSubscribed(false); return; }
        track.setVolume(volume);
        const element = track.attach();
        element.style.display = 'none';
        document.body.appendChild(element);
        s.tracks.set(track, element);
        Promise.resolve(element.play()).then(() => report(s, 'playing'), () => report(s, 'playback_blocked'));
      });
      s.room.on(sdk.RoomEvent.TrackUnsubscribed, track => {
        const element = s.tracks.get(track);
        if (element) { track.detach(element); element.remove(); s.tracks.delete(track); }
        if (!s.tracks.size) report(s, 'waiting_for_audio');
      });
      s.room.on(sdk.RoomEvent.Reconnecting, () => report(s, 'reconnecting'));
      s.room.on(sdk.RoomEvent.Reconnected, () => report(s, s.tracks.size ? 'playing' : 'waiting_for_audio'));
      s.room.on(sdk.RoomEvent.Disconnected, () => fail('disconnected'));
      s.timer = setTimeout(() => fail('connection_timeout'), 20000);
      s.worker.postMessage({ kind: 'setKeyRawBytes', data: { keyHex: options.keyHex, keyIndex: 0, updateCurrentKeyIndex: true } });
      options.keyHex = '';
      await s.room.setE2EEEnabled(true);
      if (current !== s) return;
      await s.room.connect(options.url, options.token, { autoSubscribe: false, rtcConfig: { iceServers: options.iceServers } });
      options.token = '';
      if (current !== s) { await s.room.disconnect(); return; }
      clearTimeout(s.timer);
      s.room.remoteParticipants.forEach(participant => participant.trackPublications.forEach(publication => subscribe(publication, participant)));
      report(s, s.tracks.size ? 'playing' : 'waiting_for_audio');
    } catch (_) {
      fail('connection_failed');
    } finally {
      options.keyHex = '';
      options.token = '';
    }
  }
  window.__dxfActivityAudio = { start, stop, setVolume };
})();
