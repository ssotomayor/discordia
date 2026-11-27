const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../assets/activity-audio.js'), 'utf8');

function setup({ blocked = false, slow = false, fail = false } = {}) {
  const rooms = [], workers = [], elements = [], reports = [], timers = new Set();
  let released = 0;
  class Worker {
    constructor() { this.messages = []; this.handlers = {}; workers.push(this); }
    postMessage(message) { this.messages.push(message); }
    addEventListener(name, fn) { this.handlers[name] = fn; }
    terminate() { this.stopped = true; }
  }
  class Room {
    constructor(options) { this.options = options; this.handlers = {}; this.remoteParticipants = new Map(); rooms.push(this); }
    on(name, fn) { this.handlers[name] = fn; }
    async setE2EEEnabled(value) { this.encrypted = value; }
    async connect(url, token, options) {
      this.connected = { url, token, options };
      if (fail) throw new Error('sensitive-token-should-not-escape');
      if (slow) await new Promise(resolve => { this.finishConnect = resolve; });
    }
    async disconnect() { this.closed = true; }
  }
  const events = Object.fromEntries(['TrackPublished','TrackSubscribed','TrackUnsubscribed','Disconnected','Reconnecting','Reconnected'].map(name => [name, name]));
  const context = {
    window: { LivekitClient: { Room, RoomEvent: events, ExternalE2EEKeyProvider: class {} }, __dxfE2eeWorkerSrc: 'worker', __dxfActivityAudioWanted: 'request' },
    document: { body: { appendChild: element => elements.push(element) } },
    Blob: class {}, Worker,
    URL: { createObjectURL: () => 'blob:test', revokeObjectURL: () => { released++; } },
    setTimeout(fn) { const timer = { fn }; timers.add(timer); return timer; },
    clearTimeout(timer) { timers.delete(timer); },
  };
  vm.runInNewContext(source, context);
  const api = context.window.__dxfActivityAudio;
  const options = () => ({ request: 'request', publisher: 'music-service:app', keyHex: 'ab'.repeat(32), token: 'private-test-token', url: 'ws://127.0.0.1:1234', iceServers: [] });
  const participant = { identity: 'music-service:app' };
  const publication = { kind: 'audio', trackName: 'music', setSubscribed(value) { this.subscribed = value; } };
  const track = () => ({ kind: 'audio', setVolume(value) { this.volume = value; }, detach() { this.detached = true; }, attach() {
    return { style: {}, remove() { this.removed = true; }, play() { return blocked ? Promise.reject(new Error()) : Promise.resolve(); } };
  } });
  return { api, options, rooms, workers, reports, elements, timers, publication, participant, track, released: () => released, send: value => reports.push(value), wanted: request => { context.window.__dxfActivityAudioWanted = request; } };
}

const settle = () => new Promise(resolve => setImmediate(resolve));

test('only the music publisher audio is subscribed, with E2EE and no local devices', async () => {
  const h = setup(), options = h.options();
  await h.api.start(options, h.send);
  const room = h.rooms[0];
  assert.equal(room.encrypted, true);
  assert.equal(room.connected.options.autoSubscribe, false);
  assert.equal(h.workers[0].messages[0].kind, 'setKeyRawBytes');
  assert.equal(options.keyHex, ''); assert.equal(options.token, '');
  room.handlers.TrackPublished(h.publication, h.participant);
  assert.equal(h.publication.subscribed, true);
  room.handlers.TrackPublished(h.publication, { identity: 'another-app' });
  assert.equal(h.publication.subscribed, false);
  const video = { ...h.publication, kind: 'video' };
  room.handlers.TrackPublished(video, h.participant);
  assert.equal(video.subscribed, false);
  const track = h.track();
  room.handlers.TrackSubscribed(track, h.publication, h.participant);
  await settle();
  assert.equal(h.elements.length, 1); assert.equal(track.volume, 0.7);
  h.api.setVolume(0.2); assert.equal(track.volume, 0.2);
  assert.equal(h.api.setVolume(NaN), false);
  await h.api.stop();
  assert.equal(room.closed, true); assert.equal(track.detached, true);
  assert.equal(h.elements[0].removed, true); assert.equal(h.workers[0].stopped, true);
  assert.equal(h.released(), 1); assert.equal(h.timers.size, 0);
  assert.equal(JSON.stringify(h.reports).includes('private-test-token'), false);
});

test('a cancelled connection cannot attach audio or overwrite its replacement', async () => {
  const h = setup({ slow: true });
  const first = h.api.start(h.options(), h.send);
  await settle();
  const old = h.rooms[0];
  h.wanted('replacement');
  const second = h.api.start({ ...h.options(), request: 'replacement' }, h.send);
  await settle();
  old.finishConnect(); await first;
  old.handlers.TrackSubscribed(h.track(), h.publication, h.participant);
  assert.equal(h.elements.length, 0);
  h.rooms[1].finishConnect(); await second;
  assert.equal(h.reports.at(-1).request, 'replacement');
  assert.equal(h.reports.at(-1).status, 'waiting_for_audio');
  await h.api.stop();
});

test('an audio start queued before leaving cannot recreate a cancelled room', async () => {
  const h=setup();
  await h.api.stop();
  const options=h.options();
  await h.api.start(options,h.send);
  assert.equal(h.rooms.length,0);
  assert.equal(h.workers.length,0);
  assert.equal(options.keyHex,'');
  assert.equal(options.token,'');
  assert.equal(h.reports.at(-1).status,'stopped');
});

test('playback restrictions are reported and subscriptions remain bounded', async () => {
  const h = setup({ blocked: true });
  await h.api.start(h.options(), h.send);
  h.rooms[0].handlers.TrackSubscribed(h.track(), h.publication, h.participant);
  h.rooms[0].handlers.TrackSubscribed(h.track(), h.publication, h.participant);
  await settle();
  assert.equal(h.elements.length, 1);
  assert.equal(h.reports.at(-1).status, 'playback_blocked');
  await h.api.stop();
});

test('connection and encryption errors close the room without leaking SDK details', async () => {
  const h = setup({ fail: true });
  await h.api.start(h.options(), h.send); await settle();
  assert.equal(h.reports.some(r => r.status === 'connection_failed'), true);
  assert.equal(JSON.stringify(h.reports).includes('sensitive-token'), false);
  assert.equal(h.rooms[0].closed, true);
  const good = setup();
  await good.api.start(good.options(), good.send);
  good.workers[0].handlers.message({ data: { kind: 'error' } }); await settle();
  assert.equal(good.reports.some(r => r.status === 'encryption_failed'), true);
  assert.equal(good.rooms[0].closed, true); assert.equal(good.workers[0].stopped, true);
});

test('a stalled connection times out and late completion remains detached', async () => {
  const h = setup({ slow: true });
  const pending = h.api.start(h.options(), h.send); await settle();
  [...h.timers][0].fn(); await settle();
  assert.equal(h.reports.some(r => r.status === 'connection_timeout'), true);
  h.rooms[0].finishConnect(); await pending;
  assert.equal(h.rooms[0].closed, true);
  assert.equal(h.workers[0].stopped, true);
});
