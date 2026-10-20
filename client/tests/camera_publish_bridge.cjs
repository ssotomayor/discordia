const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/features/screenshare.rs'), 'utf8');
const script = source.split('const SCREEN_JS: &str = r#"')[1].split('"#;')[0];

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

const messages = [], rooms = [];
class Room {
  constructor() {
    this.handlers = new Map();
    this.join = deferred();
    this.published = [];
    this.remoteParticipants = new Map();
    const room = this;
    this.localParticipant = {
      identity: 'self', trackPublications: new Map(),
      async publishTrack(track) { room.published.push(track); },
      async unpublishTrack() {}, async setScreenShareEnabled() {},
    };
    rooms.push(this);
  }
  on(name, callback) { this.handlers.set(name, callback); }
  connect() { return this.join.promise; }
  async disconnect() {}
  emit(name, ...values) { this.handlers.get(name)?.(...values); }
}
const camera = {
  kind: 'video', label: 'Test Camera', readyState: 'live', muted: false,
  addEventListener() {}, getSettings() { return { deviceId: 'cam' }; }, stop() { this.readyState = 'ended'; },
};
const context = {
  window: {
    LivekitClient: {
      Room, Track: { Source: { Camera: 'camera' } },
      RoomEvent: new Proxy({}, { get: (_, name) => name }),
    },
    postMessage(m) { messages.push(m); },
  },
  document: { getElementById() { return null; } },
  navigator: { mediaDevices: {
    addEventListener() {},
    async getUserMedia() { return { getTracks() { return [camera]; }, getVideoTracks() { return [camera]; } }; },
    async enumerateDevices() { return []; },
  } },
  setTimeout(fn, ms) { const t = setTimeout(fn, ms); t.unref(); return t; }, clearTimeout,
  console: { log() {}, warn() {}, error() {} },
};
vm.runInNewContext(script, context);
const bridge = context.window.dxScreen;

async function settle() { for (let i = 0; i < 50; i++) await new Promise(r => setImmediate(r)); }
const ended = () => messages.filter(m => m.__dxf === 'camera-ended' || m.__dxf === 'camera-error');

async function run() {
  const joining = bridge.connect('wss://a', 'a-token', null, false, 1);
  const starting = bridge.startCamera({});
  await settle();
  rooms[0].join.resolve();
  await joining;
  await starting;
  await settle();
  assert.equal(rooms[0].published.length, 1, 'A camera started during the join must be published once');
  assert.ok(messages.some(m => m.__dxf === 'camera-started'));
  assert.deepEqual(ended(), []);

  rooms[0].emit('Disconnected', 'signal lost');
  const rejoining = bridge.connect('wss://a', 'a-token', null, false, 2);
  await settle();
  rooms[1].join.resolve();
  await rejoining;
  await settle();
  assert.equal(rooms[1].published.length, 1, 'A live camera must be republished into the room that replaced a dropped one');
  assert.equal(rooms[1].published[0], camera);
  assert.deepEqual(ended(), []);
  console.log('Camera publish checks passed');
}
run().catch(error => { console.error(error); process.exitCode = 1; });
