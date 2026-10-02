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
  let pages = [], rows = [], scheduled, observer;
  let node = element(), receive;
  const geometry = target => {
    target.getBoundingClientRect = () => ({ top: 0 });
    target.querySelectorAll = selector => selector === '[data-chat-row]' ? rows : pages;
  };
  geometry(node);
  const samples = [];
  const window = {};
  let closed = false;
  const run = vm.runInNewContext('(async function(window, document, dioxus) {' + script + '})', {
    requestAnimationFrame: fn => { scheduled = fn; return 1; },
    cancelAnimationFrame: () => { scheduled = null; },
    ResizeObserver: class {
      constructor(fn) { this.callback = fn; this.nodes = new Set(); observer = this; }
      observe(node) { this.nodes.add(node); }
      unobserve(node) { this.nodes.delete(node); }
      disconnect() { this.nodes.clear(); }
    },
  });
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
  let rowTop = 20;
  const page = { dataset: { chatPage: 'page-one' }, getBoundingClientRect: () => ({ top: -100, bottom: 900, height: 1000 }) };
  const row = { dataset: { chatRow: 'row-one' }, getBoundingClientRect: () => ({ top: rowTop, bottom: rowTop + 40, height: 40 }) };
  pages = [page]; rows = [row];
  window.__dxfChatScrollMeasure('append', 'one');
  const measured = samples.pop();
  assert.equal(measured.pages[0].height, 1000);
  assert(observer.nodes.has(page));
  rowTop += 200;
  observer.callback(); observer.callback();
  const resizeFrame = scheduled; scheduled = null;
  resizeFrame();
  const resizeSample = samples.pop();
  assert.equal(resizeSample.mode, 'resize');
  assert.equal(resizeSample.anchor_shift, 200, 'image and page reflow reports the actual reading-anchor movement');
  pages = []; rows = [];
  window.__dxfChatScrollMeasure('channel', 'two');
  assert(!observer.nodes.has(page), 'removed history pages are released');
  const next = samples.pop();
  receive({ channel: 'one', sequence: next.sequence, top: 1 });
  await tick();
  assert.equal(node.scrollTop, 900, 'channel identity is checked even if the sequence matches');
  const old = node;
  node = element(); geometry(node);
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
  assert.equal(observer.nodes.size, 0);
}

function globeBridge() {
  const script = fs.readFileSync(path.join(root, 'assets/globe.js'), 'utf8');
  const messages = [], arcs = [], listeners = new Map();
  let frame, cancelled = false;
  const documentEvents = new Map(), windowEvents = new Map();
  const document = { hidden: false, getElementById: () => canvas,
    addEventListener: (name, handler) => documentEvents.set(name, handler),
    removeEventListener: name => documentEvents.delete(name) };
  const context = new Proxy({ measureText: text => ({ width: text.length * 6 }),
    arc: (...args) => { assert(args.every(Number.isFinite)); arcs.push(args); } },
    { get: (object, key) => key in object ? object[key] : () => {} });
  const rect = { width: 320, height: 300, left: 0, top: 0 };
  const canvas = { style: {}, getContext: () => context, getBoundingClientRect: () => rect,
    addEventListener: (name, fn) => listeners.set(name, fn),
    removeEventListener: name => listeners.delete(name), setPointerCapture() {} };
  const window = { devicePixelRatio: 1, matchMedia: () => ({ matches: true }),
    addEventListener: (name, handler) => windowEvents.set(name, handler),
    removeEventListener: name => windowEvents.delete(name) };
  vm.runInNewContext(script, { window, document,
    getComputedStyle: () => ({ getPropertyValue: () => '' }), performance: { now: () => 100 },
    requestAnimationFrame: callback => { frame = callback; return 1; },
    cancelAnimationFrame: () => { cancelled = true; frame = null; },
    setTimeout: () => 1, clearTimeout() {} });
  const globe = window.dxGlobe;
  globe.setPins('globe', [{ code: 'one', label: 'A', fresh: true,
    u: { x: 1, y: 0, z: 0 }, focus: { yaw: -Math.PI / 2, pitch: 0 } }]);
  globe.setSelected('globe', 'one');
  globe.setPlace('globe', { u: { x: 1, y: 0, z: 0 }, focus: { yaw: -Math.PI / 2, pitch: 0 } });
  globe.setPick('globe', true);
  globe.mount('globe', message => messages.push(message));
  const drawFrame = time => { const callback = frame; frame = null; assert(callback); callback(time); };
  drawFrame(100);
  const resize = messages.pop();
  assert.equal(resize.__dxf, 'globe-resize');
  assert.equal(resize.radius, 136);
  const dots = [{ x: 0, y: 0, z: 1 }];
  globe.setDots('globe', resize.radius + 1, dots);
  arcs.length = 0;
  drawFrame(116);
  const without = arcs.length;
  globe.setDots('globe', resize.radius, dots);
  arcs.length = 0;
  drawFrame(132);
  assert(arcs.length > without, 'only geometry for the current radius is accepted');
  for (let i = 0; frame && i < 100; i++) drawFrame(150 + i * 16);
  assert.equal(frame, null, 'a settled reduced-motion globe must stop scheduling animation frames');
  document.hidden = true;
  documentEvents.get('visibilitychange')();
  globe.setPins('globe', []);
  assert.equal(frame, null, 'hidden globes must not schedule draws when data changes');
  document.hidden = false;
  documentEvents.get('visibilitychange')();
  drawFrame(2000);
  assert.equal(frame, null);
  rect.width = 360;
  windowEvents.get('resize')();
  drawFrame(2100);
  assert.equal(canvas.width, 360, 'a static globe wakes for resize');
  messages.length = 0;
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
  assert.equal(listeners.size, 0);
  assert.equal(documentEvents.size, 0);
  assert.equal(windowEvents.size, 0);
}

function globeIdleAndVisibility() {
  const script = fs.readFileSync(path.join(root, 'assets/globe.js'), 'utf8');
  const frames = new Map(), timers = new Map(), events = new Map();
  let next = 0, now = 100, draws = 0;
  const context = new Proxy({ clearRect: () => draws++ }, { get: (object, key) => object[key] || (() => {}) });
  const canvas = { style: {}, getContext: () => context,
    getBoundingClientRect: () => ({ width: 320, height: 300 }),
    addEventListener() {}, removeEventListener() {} };
  const document = { hidden: false, getElementById: () => canvas,
    addEventListener: (key, fn) => events.set(key, fn), removeEventListener: key => events.delete(key) };
  const window = { matchMedia: () => ({ matches: false }), addEventListener() {}, removeEventListener() {} };
  vm.runInNewContext(script, { window, document, performance: { now: () => now },
    getComputedStyle: () => ({ getPropertyValue: () => '' }),
    requestAnimationFrame: callback => { const id = ++next; frames.set(id, callback); return id; },
    cancelAnimationFrame: id => frames.delete(id),
    setTimeout: callback => { const id = ++next; timers.set(id, callback); return id; },
    clearTimeout: id => timers.delete(id) });
  const draw = () => { const [id, callback] = frames.entries().next().value; frames.delete(id); callback(now); };
  window.dxGlobe.mount('idle', () => {});
  draw();
  assert.equal(draws, 1);
  assert.equal(frames.size, 0, 'idle spin waits without redrawing during the interaction delay');
  assert.equal(timers.size, 1);
  const [id, timer] = timers.entries().next().value;
  timers.delete(id); now = 2601; timer(); draw();
  assert.equal(frames.size, 1, 'idle auto-rotation resumes after its delay');
  document.hidden = true; events.get('visibilitychange')();
  assert.equal(frames.size, 0);
  assert.equal(timers.size, 0);
  window.dxGlobe.setPins('idle', []);
  assert.equal(frames.size, 0);
  document.hidden = false; events.get('visibilitychange')();
  assert.equal(frames.size, 1);
  draw();
  window.dxGlobe.destroy('idle');
  assert.equal(frames.size, 0);
  assert.equal(timers.size, 0);
  assert.equal(events.size, 0);
}

(async () => {
  await chatBridge();
  globeBridge();
  globeIdleAndVisibility();
  console.log('Native UI bridges: passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
