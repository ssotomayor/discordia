const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const {test} = require('node:test');
const vm = require('node:vm');
const source = readFileSync('client/assets/mobile-pager.js', 'utf8').replace('__PAGER_ID__', '"pager"');

async function fixture(initial = 0, reduced = false, deferredSmooth = false) {
  const listeners = new Map();
  const timers = new Map();
  const sent = [];
  const observers = [];
  const calls = [];
  let nextTimer = 0;
  let hide;
  const pager = {
    clientWidth: 400, scrollLeft: 0, dataset: {mobileTab: String(initial)},
    scrollTo(options) {
      calls.push(options);
      if (!deferredSmooth || options.behavior !== 'smooth') this.scrollLeft = options.left;
    },
    addEventListener(name, callback) { listeners.set(name, callback); },
    removeEventListener(name, callback) { if (listeners.get(name) === callback) listeners.delete(name); },
  };
  class Observer {
    constructor(callback) { this.callback = callback; observers.push(this); }
    observe() {}
    disconnect() { this.disconnected = true; }
  }
  const window = {
    matchMedia() { return {matches: reduced}; },
    addEventListener(name, callback) { if (name === 'pagehide') hide = callback; },
    removeEventListener(name, callback) { if (name === 'pagehide' && hide === callback) hide = null; },
  };
  const pending = vm.runInNewContext(`(async () => {${source}\n})()`, {
    document: {getElementById: id => id === 'pager' ? pager : null}, window,
    requestAnimationFrame: callback => callback(), MutationObserver: Observer, ResizeObserver: Observer,
    setTimeout: callback => { timers.set(++nextTimer, callback); return nextTimer; },
    clearTimeout: id => timers.delete(id),
    dioxus: {send: event => sent.push(JSON.parse(JSON.stringify(event)))},
  });
  await Promise.resolve();
  return {
    pager, calls, sent, observers, window, listeners, timers,
    fire(name, event = {touches: []}) { listeners.get(name)?.(event); },
    tick() { for (const [id, callback] of [...timers]) { timers.delete(id); callback(); } },
    async close() { hide?.(); await pending; },
  };
}

test('partial swipes reveal members without selecting a tab while the finger is held', async () => {
  const f = await fixture(1);
  assert.equal(f.pager.scrollLeft, 400);
  f.fire('touchstart');
  for (const x of [420, 450, 570]) { f.pager.scrollLeft = x; f.fire('scroll'); }
  f.fire('scrollend');
  f.tick();
  assert.deepEqual(f.sent, [{type: 'preview_members'}]);
  assert.equal(f.pager.dataset.mobileTab, '1');
  f.fire('touchend');
  f.pager.scrollLeft = 800;
  f.fire('scroll');
  f.tick();
  assert.deepEqual(f.sent, [{type: 'preview_members'}, {type: 'select', from: 1, index: 2}, {type: 'settled'}]);
  await f.close();
});

test('a cancelled partial swipe releases members without switching tabs', async () => {
  const f = await fixture(1);
  f.fire('touchstart');
  f.pager.scrollLeft = 450;
  f.fire('scroll');
  f.fire('touchend');
  f.pager.scrollLeft = 400;
  f.fire('scroll');
  f.tick();
  assert.deepEqual(f.sent, [{type: 'preview_members'}, {type: 'settled'}]);
  await f.close();
});

test('a vertical gesture or tap does not change pages', async () => {
  const f = await fixture();
  f.fire('touchstart');
  f.fire('touchend');
  f.tick();
  assert.deepEqual(f.sent, []);
  await f.close();
});

test('interrupted scrolling snaps once instead of polling while idle', async () => {
  const f = await fixture();
  f.pager.scrollLeft = 170;
  f.fire('scroll');
  f.tick();
  assert.equal(f.calls.at(-1).left, 0);
  assert.equal(f.calls.at(-1).behavior, 'auto');
  assert.equal(f.timers.size, 0);
  f.fire('scrollend');
  assert.deepEqual(f.sent, [{type: 'settled'}]);
  await f.close();
});

test('button and Back changes animate without echoing a second selection', async () => {
  const f = await fixture();
  f.pager.dataset.mobileTab = '2';
  f.observers[0].callback();
  assert.equal(f.calls.at(-1).left, 800);
  assert.equal(f.calls.at(-1).behavior, 'smooth');
  f.fire('scroll'); f.fire('scrollend');
  assert.deepEqual(f.sent, [{type: 'settled'}]);
  f.pager.dataset.mobileTab = '0';
  f.observers[0].callback();
  assert.equal(f.calls.at(-1).left, 0);
  await f.close();
});

test('resize preserves the selected page and reduced motion avoids smooth animation', async () => {
  const f = await fixture(2, true);
  f.pager.clientWidth = 320;
  f.observers[1].callback();
  assert.equal(f.calls.at(-1).left, 640);
  assert.equal(f.calls.at(-1).behavior, 'auto');
  f.pager.dataset.mobileTab = '1';
  f.observers[0].callback();
  assert.equal(f.calls.at(-1).behavior, 'auto');
  await f.close();
});

test('an old scrollend cannot undo opening a selected conversation', async () => {
  const f = await fixture(0, false, true);
  f.fire('touchstart'); f.fire('touchend');
  f.pager.dataset.mobileTab = '1';
  f.observers[0].callback();
  f.fire('scrollend');
  f.tick();
  assert.ok(!f.sent.some(event => event.type === 'select'));
  assert.equal(f.pager.scrollLeft, 400);
  await f.close();
});

test('opening DMs is immediate while swipes still select their destination', async () => {
  const f = await fixture(0, false, true);
  f.pager.dataset.mobileDm = 'true';
  f.pager.dataset.mobileTab = '1'; f.observers[0].callback();
  assert.equal(f.calls.at(-1).behavior, 'auto');
  assert.equal(f.pager.scrollLeft, 400);
  f.fire('scrollend');
  f.fire('touchstart');
  f.pager.scrollLeft = 0; f.fire('scroll');
  f.fire('touchend'); f.fire('scrollend');
  assert.ok(f.sent.some(event => event.type === 'select' && event.index === 0));
  await f.close();
});

test('rapid button changes finish at the latest requested page', async () => {
  const f = await fixture(0, false, true);
  f.pager.dataset.mobileTab = '2'; f.observers[0].callback();
  f.pager.scrollLeft = 400; f.fire('scroll');
  f.pager.dataset.mobileTab = '0'; f.observers[0].callback();
  f.fire('scrollend');
  assert.ok(!f.sent.some(event => event.type === 'select'));
  assert.equal(f.pager.scrollLeft, 0);
  await f.close();
});

test('a new gesture can override an unfinished button animation', async () => {
  const f = await fixture(0, false, true);
  f.pager.dataset.mobileTab = '1'; f.observers[0].callback();
  f.fire('touchstart');
  f.pager.scrollLeft = 800; f.fire('scroll');
  f.fire('touchend'); f.fire('scrollend');
  assert.ok(f.sent.some(event => event.type === 'select' && event.index === 2));
  await f.close();
});

test('leaving members with a button releases its retained content after scrolling', async () => {
  const f = await fixture(2);
  f.pager.dataset.mobileTab = '1';
  f.observers[0].callback();
  f.fire('scroll'); f.fire('scrollend');
  assert.deepEqual(f.sent, [{type: 'settled'}]);
  await f.close();
});

test('cleanup removes callbacks, observers and timers without deleting a replacement owner', async () => {
  const f = await fixture();
  f.pager.scrollLeft = 400;
  f.fire('scroll');
  const cleanup = f.window.__dxfMobilePagers.get('pager');
  const replacement = () => {};
  f.window.__dxfMobilePagers.set('pager', replacement);
  await f.close();
  assert.equal(f.listeners.size, 0);
  assert.equal(f.timers.size, 0);
  assert.ok(f.observers.every(observer => observer.disconnected));
  cleanup();
  assert.equal(f.window.__dxfMobilePagers.get('pager'), replacement);
  assert.deepEqual(f.sent, []);
});
