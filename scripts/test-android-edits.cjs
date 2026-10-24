const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync('vendor/dioxus-desktop/src/android_edits.js', 'utf8');
function fixture(headless = false) {
  const frames = new Map(), timers = new Map(), applied = [], pending = [];
  const sockets = [];
  class EditSocket {
    constructor(url) { this.url = url; sockets.push(this); }
  }
  let id = 0, acknowledgements = 0;
  const interpreter = {
    headless,
    rafEdits(bytes) { applied.push(bytes); acknowledgements++; },
    enqueueBytes(bytes) { pending.push(bytes); },
    flushQueuedBytes() { applied.push(...pending.splice(0)); },
    markEditsFinished() { assert.equal(pending.length, 0); acknowledgements++; },
  };
  vm.runInNewContext(source, {
    window: { interpreter },
    WebSocket: EditSocket,
    ArrayBuffer,
    requestAnimationFrame(fn) { frames.set(++id, fn); return id; },
    cancelAnimationFrame(id) { frames.delete(id); },
    setTimeout(fn, delay) { assert.equal(delay, 100); timers.set(++id, fn); return id; },
    clearTimeout(id) { timers.delete(id); },
  });
  return { interpreter, frames, timers, applied, pending, sockets, acks: () => acknowledgements };
}
for (const first of ['frame', 'timer']) {
  const f = fixture();
  f.interpreter.rafEdits('first');
  const frame = [...f.frames.values()][0], timer = [...f.timers.values()][0];
  assert.equal(f.acks(), 0);
  (first === 'frame' ? frame : timer)();
  assert.deepEqual(f.applied, ['first']);
  assert.equal(f.acks(), 1);
  f.interpreter.rafEdits('second');
  (first === 'frame' ? timer : frame)();
  assert.equal(f.acks(), 1, 'a late callback cannot acknowledge a newer batch');
  [...f.timers.values()][0]();
  assert.deepEqual(f.applied, ['first', 'second']);
  assert.equal(f.acks(), 2);
  assert.equal(f.frames.size + f.timers.size, 0);
}
const headless = fixture(true);
headless.interpreter.rafEdits('immediate');
assert.equal(headless.acks(), 1);
assert.deepEqual(headless.applied, ['immediate']);
assert.equal(headless.frames.size + headless.timers.size, 0);

const transport = fixture(true);
transport.interpreter.waitForRequest('ws://localhost/first', 'server-key');
const firstSocket = transport.sockets[0];
assert.equal(firstSocket.binaryType, 'arraybuffer', 'edits bypass asynchronous Blob conversion');
firstSocket.onmessage({data: new ArrayBuffer(1)});
firstSocket.onmessage({data: 'wrong-key'});
firstSocket.onmessage({data: new ArrayBuffer(1)});
assert.equal(transport.acks(), 0, 'binary edits require server authentication');
firstSocket.onmessage({data: 'server-key'});
const batch = new ArrayBuffer(2);
firstSocket.onmessage({data: batch});
assert.equal(transport.applied[0], batch);
assert.equal(transport.acks(), 1, 'headless Android applies authenticated edits synchronously');
for (let i = 0; i < 1000; i++) firstSocket.onmessage({data: new ArrayBuffer(1)});
assert.equal(transport.acks(), 1001);
assert.equal(transport.frames.size + transport.timers.size, 0);

firstSocket.onclose();
const oldReconnect = [...transport.timers.values()][0];
transport.timers.clear();
transport.interpreter.waitForRequest('ws://localhost/second', 'new-key');
firstSocket.onmessage({data: batch});
oldReconnect();
assert.equal(transport.acks(), 1001, 'a superseded socket cannot apply edits or acknowledge another connection');
assert.equal(transport.sockets.length, 2, 'a stale close cannot replace the new connection');
const secondSocket = transport.sockets[1];
secondSocket.onmessage({data: batch});
assert.equal(transport.acks(), 1001, 'a new socket starts unauthenticated');
secondSocket.onmessage({data: 'new-key'});
secondSocket.onmessage({data: batch});
assert.equal(transport.acks(), 1002);
secondSocket.onclose();
[...transport.timers.values()][0]();
assert.equal(transport.sockets.length, 3, 'the current socket reconnects');
assert.equal(transport.sockets[2].binaryType, 'arraybuffer');
console.log('Android edit sockets apply authenticated ArrayBuffers without Blob conversion; flushes, reconnects and late callbacks preserve order.');
