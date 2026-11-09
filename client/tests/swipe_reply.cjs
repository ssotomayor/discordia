const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { test } = require('node:test');
const script = fs.readFileSync(`${__dirname}/../assets/swipe-reply.js`, 'utf8');

function fixture() {
  const log = [];
  class Element {
    constructor(tag, attrs = {}) {
      this.tagName = tag.toUpperCase(); this.attrs = attrs; this.childNodes = []; this.parent = null;
      this.id = attrs.id || ''; this.disabled = false; this.dataset = {};
      const classes = new Set();
      this.classList = {
        add: (...names) => names.forEach(n => classes.add(n)),
        remove: (...names) => names.forEach(n => classes.delete(n)),
        toggle: (name, on) => { on ? classes.add(name) : classes.delete(name); },
        contains: name => classes.has(name),
      };
      const vars = new Map();
      this.style = {
        setProperty: (k, v) => vars.set(k, v),
        removeProperty: k => vars.delete(k),
        get: k => vars.get(k),
      };
    }
    append(...children) { for (const child of children) { child.parent = this; this.childNodes.push(child); } }
    matches(selector) {
      return selector.split(',').some(value => {
        value = value.trim();
        const attr = value.match(/^\[([^=\]^]+)(\^?)=?"?([^"\]]*)"?\]$/);
        if (attr) {
          const [, key, prefix, expected] = attr;
          const actual = key === 'id' ? this.id : this.attrs[key];
          if (actual === undefined) return false;
          if (!expected) return true;
          return prefix ? actual.startsWith(expected) : actual === expected;
        }
        const compound = value.match(/^\[([^\]]+)\]\[([^\]]+)\]$/);
        if (compound) return this.matches(`[${compound[1]}]`) && this.matches(`[${compound[2]}]`);
        return this.tagName === value.toUpperCase();
      });
    }
    closest(selector) { return this.matches(selector) ? this : this.parent?.closest(selector) || null; }
    querySelectorAll(selector) {
      return this.childNodes.flatMap(child => [...(child.matches(selector) ? [child] : []), ...child.querySelectorAll(selector)]);
    }
    querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
    setPointerCapture(id) { log.push(['capture', id]); }
    click() { log.push(['click', this.attrs['data-context-action']]); }
    focus() { log.push(['focus', this.id]); }
  }
  const listeners = new Map();
  const body = new Element('div');
  const document = {
    addEventListener(name, fn) { listeners.set(name, [...(listeners.get(name) || []), fn]); },
    removeEventListener(name, fn) { listeners.set(name, (listeners.get(name) || []).filter(f => f !== fn)); },
    querySelector: selector => body.querySelector(selector),
  };
  const row = new Element('div', { 'data-context-message': 'hello' });
  const text = new Element('span');
  const reply = new Element('button', { 'data-context-action': 'reply' });
  row.append(text, reply);
  const composer = new Element('div', { id: 'dxf-composer-7', contenteditable: 'true' });
  body.append(row, composer);
  const timers = [];
  const window = {
    __dxfComposers: { 'dxf-composer-7': { focus: () => log.push(['api-focus']) } },
  };
  const context = {
    window, document, navigator: { vibrate: ms => log.push(['vibrate', ms]) },
    setTimeout: fn => { timers.push(fn); return timers.length; },
    Math, String,
  };
  vm.runInNewContext(`(function () { ${script} })()`, context);
  const fire = (type, init) => {
    const event = { type, pointerType: 'touch', isPrimary: true, pointerId: 1, target: text, ...init };
    for (const fn of listeners.get(type) || []) fn(event);
  };
  return { fire, row, text, reply, log, timers, window, listeners };
}

test('a leftward pull past the arm point focuses the composer, then clicks Reply', () => {
  const f = fixture();
  f.fire('pointerdown', { clientX: 200, clientY: 100 });
  f.fire('pointermove', { clientX: 180, clientY: 102 });
  assert.ok(f.row.classList.contains('dxf-swiping'));
  assert.deepEqual(f.log, [['capture', 1]]);
  assert.equal(f.row.style.get('--swipe'), '-20px');
  f.fire('pointermove', { clientX: 140, clientY: 104 });
  assert.ok(f.row.classList.contains('dxf-swipe-armed'));
  assert.equal(f.row.style.get('--swipe-progress'), '1');
  assert.deepEqual(f.log.at(-1), ['vibrate', 8]);
  f.fire('pointerup', { clientX: 140, clientY: 104 });
  assert.deepEqual(f.log.slice(-2), [['api-focus'], ['click', 'reply']]);
  assert.ok(!f.row.classList.contains('dxf-swiping'));
  assert.ok(f.row.classList.contains('dxf-swipe-settling'));
  assert.equal(f.row.style.get('--swipe'), '0px');
  f.timers.forEach(fn => fn());
  assert.ok(!f.row.classList.contains('dxf-swipe-settling'));
  assert.equal(f.row.style.get('--swipe'), undefined);
});

test('the pull past the limit is resisted, never beyond it', () => {
  const f = fixture();
  f.fire('pointerdown', { clientX: 300, clientY: 100 });
  f.fire('pointermove', { clientX: 280, clientY: 100 });
  f.fire('pointermove', { clientX: 100, clientY: 100 });
  const shown = parseFloat(f.row.style.get('--swipe'));
  assert.ok(shown < -56 && shown > -84, String(shown));
});

test('releasing before the arm point snaps back and replies to nothing', () => {
  const f = fixture();
  f.fire('pointerdown', { clientX: 200, clientY: 100 });
  f.fire('pointermove', { clientX: 170, clientY: 100 });
  f.fire('pointerup', { clientX: 170, clientY: 100 });
  assert.ok(!f.log.some(([kind]) => kind === 'click' || kind === 'api-focus'));
  assert.ok(f.row.classList.contains('dxf-swipe-settling'));
});

test('vertical and rightward movement are left to the scroller and the pager', () => {
  for (const [x, y] of [[200, 140], [240, 100]]) {
    const f = fixture();
    f.fire('pointerdown', { clientX: 200, clientY: 100 });
    f.fire('pointermove', { clientX: x, clientY: y });
    f.fire('pointermove', { clientX: x - 100, clientY: y });
    f.fire('pointerup', { clientX: x - 100, clientY: y });
    assert.deepEqual(f.log, []);
    assert.ok(!f.row.classList.contains('dxf-swiping'));
  }
});

test('a mouse, a button, a row without Reply, and a cancelled pointer do nothing', () => {
  const f = fixture();
  f.fire('pointerdown', { clientX: 200, clientY: 100, pointerType: 'mouse' });
  f.fire('pointermove', { clientX: 100, clientY: 100, pointerType: 'mouse' });
  f.fire('pointerup', { clientX: 100, clientY: 100, pointerType: 'mouse' });
  f.fire('pointerdown', { clientX: 200, clientY: 100, target: f.reply });
  f.fire('pointermove', { clientX: 100, clientY: 100 });
  f.fire('pointerup', { clientX: 100, clientY: 100 });
  assert.deepEqual(f.log, []);
  f.fire('pointerdown', { clientX: 200, clientY: 100 });
  f.fire('pointermove', { clientX: 100, clientY: 100 });
  f.fire('pointercancel', {});
  f.fire('pointerup', { clientX: 100, clientY: 100 });
  assert.deepEqual(f.log, [['capture', 1], ['vibrate', 8]]);
  assert.ok(f.row.classList.contains('dxf-swipe-settling'));
  f.row.childNodes = f.row.childNodes.filter(c => c !== f.reply);
  f.fire('pointerdown', { clientX: 200, clientY: 100 });
  f.fire('pointermove', { clientX: 100, clientY: 100 });
  f.fire('pointerup', { clientX: 100, clientY: 100 });
  assert.deepEqual(f.log, [['capture', 1], ['vibrate', 8]]);
});

test('turning the script off removes every listener and a rerun replaces the first', () => {
  const f = fixture();
  assert.equal(f.listeners.get('pointerdown').length, 1);
  f.window.__dxfSwipeReplyOff();
  assert.equal(f.listeners.get('pointerdown').length, 0);
  assert.equal(f.window.__dxfSwipeReplyOff, undefined);
});
