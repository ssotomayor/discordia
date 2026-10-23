import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const source = fs.readFileSync(new URL('../client/assets/globe.js', import.meta.url), 'utf8');

function fixture({ mobile = false, reduced = false } = {}) {
  const frames = new Map(), timers = new Map(), listeners = new Map(), windowListeners = new Map();
  let next = 0, now = 5000, draws = 0, disconnected = 0;
  const ctx = new Proxy({ clearRect() { draws++; }, measureText() { return { width: 40 }; } }, {
    get(target, key) { return target[key] ?? (() => {}); },
  });
  const canvas = {
    style: {}, getContext() { return ctx; }, closest() { return mobile ? {} : null; },
    getBoundingClientRect() { return { width: 320, height: 240, left: 0, top: 0 }; },
    addEventListener(name, handler) { listeners.set(name, handler); },
    removeEventListener(name) { listeners.delete(name); }, setPointerCapture() {},
  };
  const documentListeners = new Map();
  const document = {
    hidden: false, documentElement: {}, getElementById() { return canvas; },
    addEventListener(name, handler) { documentListeners.set(name, handler); },
    removeEventListener(name) { documentListeners.delete(name); },
  };
  class Observer { observe() {} disconnect() { disconnected++; } }
  const context = {
    window: {
      devicePixelRatio: 2, matchMedia() { return { matches: reduced }; },
      addEventListener(name, handler) { windowListeners.set(name, handler); },
      removeEventListener(name) { windowListeners.delete(name); },
    }, document, performance: { now: () => now }, ResizeObserver: Observer, MutationObserver: Observer,
    getComputedStyle() { return { getPropertyValue() { return ''; } }; },
    requestAnimationFrame(callback) { const id = ++next; frames.set(id, callback); return id; },
    cancelAnimationFrame(id) { frames.delete(id); },
    setTimeout(callback) { const id = ++next; timers.set(id, callback); return id; },
    clearTimeout(id) { timers.delete(id); },
  };
  vm.runInNewContext(source, context);
  const bridge = context.window.dxGlobe;
  bridge.mount('test', () => {});
  const run = (limit = 200) => {
    for (let i = 0; i < limit && frames.size; i++) {
      const current = [...frames.values()]; frames.clear(); now += 16.67;
      current.forEach(callback => callback(now));
    }
  };
  return { bridge, frames, timers, listeners, document, documentListeners, run,
    draws: () => draws, disconnected: () => disconnected, windowListeners };
}

for (const config of [{ mobile: true }, { reduced: true }]) {
  const f = fixture(config);
  f.run();
  assert.ok(f.draws() > 0, 'a static globe must still paint');
  assert.equal(f.frames.size, 0, 'idle globe must stop requesting frames');
  assert.equal(f.timers.size, 0, 'idle globe must not schedule an auto-rotation timer');
  f.bridge.setPins('test', [{ code: 'room', label: 'Room', fresh: true, u: { x: 0, y: 0, z: 1 }, focus: { yaw: 0, pitch: 0 } }]);
  f.bridge.setSelected('test', 'room');
  const initial = f.draws();
  f.run();
  assert.ok(f.draws() > initial, 'pin selection must still focus and redraw');
  assert.equal(f.frames.size, 0, 'a fresh or selected pin must settle without perpetual pulses');
  f.listeners.get('pointerdown')({ clientX: 100, clientY: 100, pointerId: 1 });
  f.listeners.get('pointermove')({ clientX: 130, clientY: 110 });
  f.listeners.get('pointerup')({ clientX: 130, clientY: 110 });
  const beforeDrag = f.draws();
  f.run();
  assert.ok(f.draws() > beforeDrag, 'dragging must redraw');
  assert.equal(f.frames.size, 0, 'drag inertia must settle');
  f.bridge.destroy('test');
  assert.equal(f.listeners.size, 0);
  assert.equal(f.windowListeners.size, 0);
  assert.equal(f.documentListeners.size, 0);
  assert.equal(f.disconnected(), 2);
}

const desktop = fixture();
desktop.run(3);
assert.equal(desktop.frames.size, 1, 'desktop retains its decorative rotation');
desktop.document.hidden = true;
desktop.documentListeners.get('visibilitychange')();
assert.equal(desktop.frames.size, 0, 'hidden pages suspend the globe');
desktop.document.hidden = false;
desktop.documentListeners.get('visibilitychange')();
assert.equal(desktop.frames.size, 1, 'returning to the page repaints');
desktop.bridge.destroy('test');
assert.equal(desktop.frames.size, 0);
console.log('Globe: mobile/reduced-motion idle, pin focus, drag inertia, visibility and teardown passed.');
