const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const root = path.join(__dirname, '..');
const tick = () => new Promise(resolve => setImmediate(resolve));

async function chatBridge() {
  const source = fs.readFileSync(path.join(root, 'src/features/chat.rs'), 'utf8');
  const script = source.match(/const SCROLL_JS: &str = r#"([\s\S]*?)"#;/)[1];
  const element = () => ({ scrollHeight: 1000, scrollTop: 300, clientHeight: 200,
    listeners: new Map(), addEventListener(name, fn) { this.listeners.set(name, fn); },
    removeEventListener(name, fn) { if (this.listeners.get(name) === fn) this.listeners.delete(name); } });
  let node = element(), receive;
  const samples = [];
  const window = {};
  let closed = false;
  const run = vm.runInNewContext('(async function(window, document, dioxus) {' + script + '})');
  const lifetime = run(window, { getElementById: () => node },
    { send: value => samples.push(value), recv: () => new Promise(resolve => { receive = resolve; }) })
    .then(() => { closed = true; });
  await tick();
  assert.equal(closed, false, 'Dioxus must keep the bidirectional channel open');
  assert.equal(samples.shift(), true);
  window.__dxfChatScrollMeasure('channel', 'one');
  const first = samples.pop();
  window.__dxfChatScrollMeasure('append', 'one');
  const second = samples.pop();
  receive({ channel: 'one', sequence: first.sequence, top: 1000 });
  await tick();
  assert.equal(node.scrollTop, 300, 'a late response cannot override a newer measurement');
  receive({ channel: 'one', sequence: second.sequence, top: 900 });
  await tick();
  assert.equal(node.scrollTop, 900);
  node.listeners.get('scroll')();
  const manual = samples.pop();
  assert.equal(manual.mode, 'scroll');
  assert.equal(manual.top, 900);
  window.__dxfChatScrollMeasure('channel', 'two');
  const next = samples.pop();
  receive({ channel: 'one', sequence: next.sequence, top: 1 });
  await tick();
  assert.equal(node.scrollTop, 900, 'channel identity is checked even if the sequence matches');
  const old = node;
  node = element();
  receive({ channel: 'two', sequence: next.sequence, top: 1 });
  await tick();
  assert.equal(node.scrollTop, 300, 'a remounted node cannot receive an old command');
  window.__dxfChatScrollMeasure('append', 'two');
  assert.equal(old.listeners.size, 0);
  assert.equal(node.listeners.size, 1);
  window.__dxfChatScrollOff();
  await lifetime;
  assert.equal(closed, true, 'unmount closes the eval even while waiting for Rust');
  assert.equal(node.scrollTop, 300);
  assert.equal(node.listeners.size, 0);
}

function globeBridge() {
  const script = fs.readFileSync(path.join(root, 'assets/globe.js'), 'utf8');
  const messages = [], arcs = [], listeners = new Map();
  let frame, cancelled = false;
  const context = new Proxy({ measureText: text => ({ width: text.length * 6 }),
    arc: (...args) => { assert(args.every(Number.isFinite)); arcs.push(args); } },
    { get: (object, key) => key in object ? object[key] : () => {} });
  const rect = { width: 320, height: 300, left: 0, top: 0 };
  const canvas = { style: {}, getContext: () => context, getBoundingClientRect: () => rect,
    addEventListener: (name, fn) => listeners.set(name, fn), setPointerCapture() {} };
  const window = { devicePixelRatio: 1, matchMedia: () => ({ matches: true }) };
  vm.runInNewContext(script, { window, document: { getElementById: () => canvas },
    getComputedStyle: () => ({ getPropertyValue: () => '' }), performance: { now: () => 100 },
    requestAnimationFrame: callback => { frame = callback; return 1; },
    cancelAnimationFrame: () => { cancelled = true; } });
  const globe = window.dxGlobe;
  globe.setPins('globe', [{ code: 'one', label: 'A', fresh: true,
    u: { x: 1, y: 0, z: 0 }, focus: { yaw: -Math.PI / 2, pitch: 0 } }]);
  globe.setSelected('globe', 'one');
  globe.setPlace('globe', { u: { x: 1, y: 0, z: 0 }, focus: { yaw: -Math.PI / 2, pitch: 0 } });
  globe.setPick('globe', true);
  globe.mount('globe', message => messages.push(message));
  frame(100);
  const resize = messages.pop();
  assert.equal(resize.__dxf, 'globe-resize');
  assert.equal(resize.radius, 136);
  const dots = [{ x: 0, y: 0, z: 1 }];
  globe.setDots('globe', resize.radius + 1, dots);
  arcs.length = 0;
  frame(116);
  const without = arcs.length;
  globe.setDots('globe', resize.radius, dots);
  arcs.length = 0;
  frame(132);
  assert(arcs.length > without, 'only geometry for the current radius is accepted');
  globe.setPins('globe', []);
  listeners.get('pointerdown')({ clientX: 160, clientY: 150, pointerId: 1 });
  listeners.get('pointerup')({ clientX: 160, clientY: 150 });
  const place = messages.pop();
  assert.equal(place.__dxf, 'globe-place');
  assert.equal(place.sx, 160);
  assert.equal(place.radius, 136);
  assert.equal(place.lat, undefined, 'geographic conversion belongs to Rust');
  globe.destroy('globe');
  globe.setDots('globe', 136, dots);
  assert.equal(cancelled, true);
}

(async () => {
  await chatBridge();
  globeBridge();
  console.log('Native UI bridges: passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
