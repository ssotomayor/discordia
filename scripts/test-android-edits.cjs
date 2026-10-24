const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync('vendor/dioxus-desktop/src/android_edits.js', 'utf8');
function fixture(headless = false) {
  const frames = new Map(), timers = new Map(), applied = [], pending = [];
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
    requestAnimationFrame(fn) { frames.set(++id, fn); return id; },
    cancelAnimationFrame(id) { frames.delete(id); },
    setTimeout(fn, delay) { assert.equal(delay, 100); timers.set(++id, fn); return id; },
    clearTimeout(id) { timers.delete(id); },
  });
  return { interpreter, frames, timers, applied, pending, acks: () => acknowledgements };
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
console.log('Android edits flush and acknowledge once without animation frames; late callbacks preserve batch order.');
