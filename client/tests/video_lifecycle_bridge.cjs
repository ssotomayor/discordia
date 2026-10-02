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

function fixture({ encrypted = false, constructorFailure = false } = {}) {
  const messages = [], rooms = [], timers = [], workers = [];
  class Room {
    constructor() {
      if (constructorFailure) throw new Error('SDK initialization failed');
      this.handlers = new Map();
      this.join = deferred();
      this.keySetup = deferred();
      this.disconnects = 0;
      this.remoteParticipants = new Map();
      this.localParticipant = {
        identity: 'self', trackPublications: new Map(),
        async setScreenShareEnabled() {}, async unpublishTrack() {},
      };
      rooms.push(this);
    }
    on(name, callback) { this.handlers.set(name, callback); }
    connect() { return this.join.promise; }
    setE2EEEnabled() { return this.keySetup.promise; }
    disconnect() { this.disconnects++; return this.leave ? this.leave.promise : Promise.resolve(); }
    emit(name, value) { this.handlers.get(name)?.(value); }
  }
  const RoomEvent = Object.fromEntries(['EncryptionError', 'Disconnected', 'ConnectionStateChanged',
    'TrackPublished', 'TrackSubscribed', 'TrackUnsubscribed', 'LocalTrackPublished', 'LocalTrackUnpublished']
    .map(name => [name, name]));
  const context = {
    window: { LivekitClient: { Room, RoomEvent }, postMessage(m) { messages.push(m); } },
    document: { body: {}, getElementById() { return null; } },
    navigator: { mediaDevices: { addEventListener() {} } },
    setTimeout(fn, ms) { timers.push({ fn, ms }); }, clearTimeout() {},
    console: { log() {}, warn() {}, error() {} },
  };
  if (encrypted) {
    context.window.__dxfE2eeWorkerSrc = 'test worker';
    context.window.LivekitClient.ExternalE2EEKeyProvider = class {};
    context.Blob = Blob;
    context.URL = { createObjectURL() { return 'blob:test'; }, revokeObjectURL() {} };
    context.Worker = class {
      constructor() { workers.push(this); }
      postMessage() {}
      terminate() { this.terminated = true; }
    };
  }
  vm.runInNewContext(script, context);
  return { bridge: context.window.dxScreen, rooms, messages, timers, workers };
}

async function settle() { for (let i = 0; i < 20; i++) await Promise.resolve(); }

(async () => {
  {
    const f = fixture();
    const pending = f.bridge.connect('wss://test', 'token', null, false, 1);
    await settle();
    f.rooms[0].join.reject(new Error('network offline'));
    await pending;
    const state = f.messages.findLast(m => m.__dxf === 'screen-room-state');
    assert.equal(state.generation, 1);
    assert.equal(state.status, 'retry');
    assert.equal(f.rooms.length, 1);
    assert.equal(f.timers.length, 0, 'JavaScript must not schedule reconnects');
    const retry = f.bridge.connect('wss://test', 'token', null, false, 2);
    await settle();
    f.rooms[1].join.resolve();
    await retry;
    assert.equal(f.messages.at(-1).status, 'connected');
    assert.equal(f.messages.at(-1).generation, 2);
    f.rooms[0].emit('Disconnected', 'old failure');
    assert.equal(f.messages.at(-1).status, 'connected');
  }
  {
    const f = fixture();
    const pending = f.bridge.connect('wss://old', 'old-token', null, false, 3);
    await settle();
    await f.bridge.disconnect();
    const count = f.messages.length;
    f.rooms[0].join.resolve();
    await pending;
    assert.equal(f.messages.length, count, 'a join completed after leaving must remain silent');
    assert.ok(f.rooms[0].disconnects >= 1);
  }
  {
    const f = fixture();
    const old = f.bridge.connect('wss://old', 'old-token', null, false, 4);
    await settle();
    const next = f.bridge.connect('wss://next', 'next-token', null, false, 5);
    await settle();
    f.rooms[0].join.resolve();
    await old;
    assert.equal(f.messages.some(m => m.status === 'connected' && m.generation === 4), false);
    f.rooms[1].join.resolve();
    await next;
    assert.equal(f.messages.at(-1).generation, 5);
    assert.equal(f.messages.at(-1).status, 'connected');
  }
  {
    const f = fixture();
    const old = f.bridge.connect('wss://old', 'old-token', null, false, 6);
    await settle();
    f.rooms[0].join.resolve();
    await old;
    f.rooms[0].leave = deferred();
    const leaving = f.bridge.disconnect();
    await settle();
    const next = f.bridge.connect('wss://next', 'next-token', null, false, 7);
    await settle();
    f.rooms[1].join.resolve();
    await next;
    f.rooms[0].leave.resolve();
    await leaving;
    f.rooms[1].emit('Disconnected', 'network interrupted');
    assert.equal(f.messages.at(-1).generation, 7);
    assert.equal(f.messages.at(-1).status, 'retry', 'old teardown must not clear the new room');
  }
  {
    const f = fixture({ constructorFailure: true });
    await f.bridge.connect('wss://test', 'token', null, false, 9);
    assert.equal(f.messages.at(-1).status, 'retry');
    assert.equal(f.messages.at(-1).generation, 9);
    assert.equal(f.timers.length, 0);
  }
  {
    const f = fixture({ encrypted: true });
    const pending = f.bridge.connect('wss://test', 'token', 'secret', true, 10);
    await settle();
    f.rooms[0].keySetup.reject(new Error('encryption failed'));
    await pending;
    assert.equal(f.messages.at(-1).status, 'blocked');
    assert.equal(f.rooms[0].disconnects, 1);
    assert.equal(f.workers[0].terminated, true);
  }
  {
    const f = fixture({ encrypted: true });
    const old = f.bridge.connect('wss://old', 'old-token', 'old-key', true, 11);
    await settle();
    f.rooms[0].leave = deferred();
    f.rooms[0].keySetup.reject(new Error('old encryption failed'));
    await settle();
    const next = f.bridge.connect('wss://next', 'next-token', 'new-key', true, 12);
    await settle();
    f.rooms[0].leave.resolve();
    await old;
    assert.notEqual(f.workers[1].terminated, true, 'old failure must preserve the new encryption worker');
    assert.equal(f.messages.some(m => m.__dxf === 'e2ee-error'), false);
    f.rooms[1].keySetup.resolve();
    await settle();
    f.rooms[1].join.resolve();
    await next;
    assert.equal(f.messages.at(-1).status, 'connected');
    assert.equal(f.messages.at(-1).generation, 12);
  }
  {
    const f = fixture();
    await f.bridge.connect('wss://test', 'token', 'secret', true, 8);
    assert.equal(f.rooms.length, 0, 'missing encryption support must never join in the clear');
    assert.equal(f.messages.at(-1).generation, 8);
    assert.equal(f.messages.at(-1).status, 'blocked');
    assert.equal(f.timers.length, 0);
  }
  console.log('Native video lifecycle bridge: retry reporting, cancellation, room replacement and encryption guards passed.');
})().catch(error => { console.error(error); process.exitCode = 1; });
