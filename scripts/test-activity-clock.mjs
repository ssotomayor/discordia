import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import assert from 'node:assert/strict';

const source = readFileSync(new URL('../client/assets/activity-clock.js', import.meta.url), 'utf8');

function harness() {
  const window = new EventTarget();
  const document = new EventTarget();
  document.hidden = false;
  document.hasFocus = () => true;
  document.documentElement = {};
  let clocks = [];
  let queries = 0;
  document.querySelectorAll = () => { queries++; return clocks; };
  let now = 65_000;
  let observer;
  const timers = new Map();
  let nextId = 0;
  const context = vm.createContext({
    window, document, Date: { now: () => now },
    setInterval: f => { timers.set(++nextId, f); return nextId; },
    clearInterval: id => timers.delete(id),
    MutationObserver: class {
      constructor(callback) { observer = callback; }
      observe() {}
    },
  });
  vm.runInContext(source, context);
  return {
    window, document, timers,
    mount(start = 1_000) {
      const clock = {
        nodeType: 1,
        firstChild: { nodeType: 3, nodeValue: '' },
        get textContent() { return this.firstChild.nodeValue; },
        set textContent(value) { this.firstChild = { nodeType: 3, nodeValue: value }; },
        getAttribute: () => String(start), matches: () => true,
      };
      clock.setStart = value => {
        start = value;
        observer([{ type: 'attributes' }]);
      };
      clocks = [clock];
      observer([{ type: 'childList', addedNodes: [clock], removedNodes: [] }]);
      return clock;
    },
    remove() {
      const removed = clocks;
      clocks = [];
      observer([{ type: 'childList', addedNodes: [], removedNodes: removed }]);
    },
    advance(ms) { now += ms; for (const f of timers.values()) f(); },
    runAgain() { vm.runInContext(source, context); },
    queries() { return queries; },
    unrelatedChange() {
      observer([{ type: 'childList', addedNodes: [{ nodeType: 1, matches: () => false, querySelector: () => null }], removedNodes: [] }]);
      observer([{ type: 'childList', addedNodes: [{ nodeType: 3 }], removedNodes: [{ nodeType: 3 }] }]);
    },
  };
}

test('mounting a clock starts one timer and updates only its text', () => {
  const h = harness();
  assert.equal(h.timers.size, 0);
  const clock = h.mount();
  assert.equal(clock.textContent, '01:04 elapsed');
  h.advance(1_000);
  assert.equal(clock.textContent, '01:05 elapsed');
  h.runAgain();
  assert.equal(h.timers.size, 1);
});

test('blur stops ticking and focus catches up from the original start', () => {
  const h = harness();
  const clock = h.mount();
  h.window.dispatchEvent(new Event('blur'));
  assert.equal(h.timers.size, 0);
  h.advance(3_600_000);
  assert.equal(clock.textContent, '01:04 elapsed');
  h.window.dispatchEvent(new Event('focus'));
  assert.equal(clock.textContent, '1:01:04 elapsed');
  assert.equal(h.timers.size, 1);
});

test('hidden clocks stay stopped even if focus returns', () => {
  const h = harness();
  const clock = h.mount();
  h.document.hidden = true;
  h.document.dispatchEvent(new Event('visibilitychange'));
  h.window.dispatchEvent(new Event('focus'));
  h.advance(10_000);
  assert.equal(h.timers.size, 0);
  h.document.hidden = false;
  h.document.dispatchEvent(new Event('visibilitychange'));
  assert.equal(clock.textContent, '01:14 elapsed');
  assert.equal(h.timers.size, 1);
});

test('closing a card releases the timer; reopening creates only one', () => {
  const h = harness();
  h.mount();
  h.remove();
  assert.equal(h.timers.size, 0);
  h.advance(30_000);
  assert.equal(h.mount().textContent, '01:34 elapsed');
  assert.equal(h.timers.size, 1);
});

test('invalid and future starts render no elapsed duration', () => {
  for (const start of [0, -1, NaN, 100_000]) {
    assert.equal(harness().mount(start).textContent, '');
  }
});

test('a new game timestamp resets elapsed immediately without adding timers', () => {
  const h = harness();
  const clock = h.mount();
  clock.setStart(60_000);
  assert.equal(clock.textContent, '00:05 elapsed');
  assert.equal(h.timers.size, 1);
  h.window.dispatchEvent(new Event('blur'));
  clock.setStart(64_000);
  assert.equal(h.timers.size, 0);
  h.advance(5_000);
  h.window.dispatchEvent(new Event('focus'));
  assert.equal(clock.textContent, '00:06 elapsed');
});

test('ticks preserve the text node retained by Dioxus across activity changes', () => {
  const h = harness();
  const clock = h.mount();
  const text = clock.firstChild;
  h.advance(1_000);
  assert.equal(clock.firstChild, text);
  assert.equal(text.nodeValue, '01:05 elapsed');
  clock.setStart(60_000);
  assert.equal(clock.firstChild, text);
  assert.equal(text.nodeValue, '00:06 elapsed');
});

test('chat updates and clock text changes avoid rescanning the document', () => {
  const h = harness();
  h.mount();
  const before = h.queries();
  for (let n = 0; n < 100; n++) h.unrelatedChange();
  assert.equal(h.queries(), before);
  assert.equal(h.timers.size, 1);
});
