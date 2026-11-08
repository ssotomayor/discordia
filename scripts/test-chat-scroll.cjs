const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const chat = fs.readFileSync('client/src/features/chat.rs', 'utf8');
const source = chat.match(/const SCROLL_JS: &str = r#"([\s\S]*?)"#;/)[1];

// Rows are laid out top to bottom; the scroller's top edge sits at y = 0.
function fixture(heights = Array(20).fill(100), viewport = 500) {
  const samples = [], frames = [], scrolls = [];
  let rows = [], top = 0, resized = null, switched = null;
  const hidden = () => node.clientHeight === 0;
  const layout = () => { let y = 0; for (const row of rows) { row.y = y; y += row.h; } return y; };
  const row = (id, h) => {
    const el = { h, y: 0, isConnected: true, dataset: { chatRow: id },
      getBoundingClientRect: () => hidden() ? { top: 0, bottom: 0, height: 0 }
        : (layout(), { top: el.y - top, bottom: el.y + el.h - top, height: el.h }) };
    return el;
  };
  const page = { dataset: { chatPage: 'page-0' },
    getBoundingClientRect: () => { const h = layout(); return { top: -top, bottom: h - top, height: h }; } };
  rows = heights.map((h, i) => row('row-' + i, h));
  const node = {
    clientHeight: viewport, dataset: { chatChannel: 'one' },
    get scrollHeight() { return Math.max(layout(), this.clientHeight); },
    get scrollTop() { return hidden() ? 0 : top; },
    set scrollTop(value) {
      const next = Math.max(0, Math.min(this.scrollHeight - this.clientHeight, value));
      if (next !== top) scrolls.push(next);
      top = next;
    },
    getBoundingClientRect: () => ({ top: 0 }),
    querySelectorAll: selector => (layout(), selector === '[data-chat-row]' ? rows : [page]),
    querySelector: selector => rows.find(r => selector === '[data-chat-row="' + r.dataset.chatRow + '"]') || null,
    listeners: new Map(),
    addEventListener(name, fn) { this.listeners.set(name, fn); },
    removeEventListener(name, fn) { if (this.listeners.get(name) === fn) this.listeners.delete(name); },
  };
  const content = {};
  const window = { listeners: new Map(),
    addEventListener(name, fn) { this.listeners.set(name, fn); },
    removeEventListener(name, fn) { if (this.listeners.get(name) === fn) this.listeners.delete(name); } };
  let done = false;
  const task = vm.runInNewContext(`(function(){${source}})()`, {
    window,
    document: { getElementById: id => id === 'dxf-chat-scroll' ? node : id === 'dxf-chat-content' ? content : null },
    dioxus: { send: value => samples.push(value), recv: () => new Promise(() => {}) },
    requestAnimationFrame: fn => { frames.push(fn); return frames.length; },
    cancelAnimationFrame() {},
    ResizeObserver: class { constructor(fn) { resized = fn; } observe() {} disconnect() {} },
    MutationObserver: class { constructor(fn) { switched = fn; } observe() {} disconnect() {} },
  }).then(() => { done = true; });
  const bottom = () => node.scrollHeight - node.clientHeight;
  return {
    node, window, samples, task, bottom, row,
    rows: () => rows, setRows: next => { rows = next; },
    // What the engine does after layout and before paint.
    resize: () => resized(), retitle: () => switched(),
    // The browser's scroll event, dispatched a frame after the move.
    scroll: value => { if (value !== undefined) top = value; node.listeners.get('scroll')(); },
    // Layout clamping a scroller whose content shrank, with no event yet.
    clamp: () => { top = Math.min(top, node.scrollHeight - node.clientHeight); },
    input: (name, event = {}) => node.listeners.get(name)({ target: node, ...event }),
    frame: () => { const pending = frames.splice(0); pending.forEach(fn => fn()); },
    done: () => done,
  };
}

test('opening a conversation shows the latest messages before Rust answers', async () => {
  const f = fixture();
  try {
    assert.equal(f.samples[0], true, 'Rust learns the bridge is up first');
    assert.equal(f.node.scrollTop, f.bottom());
    f.frame();
    const sample = f.samples.at(-1);
    assert.equal(sample.channel, 'one');
    assert.equal(sample.viewport, 500);
    assert.equal(sample.pages[0].id, 'page-0');
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});

test('late images and new messages keep the bottom without a visible frame', async () => {
  const f = fixture();
  try {
    f.rows()[19].h = 340;
    f.resize();
    assert.equal(f.node.scrollTop, f.bottom(), 'an image below is followed in the same frame');
    f.setRows([...f.rows(), f.row('row-20', 80)]);
    // The scroll event of the last move can run before the resize callback.
    f.scroll();
    assert.equal(f.node.scrollTop, f.bottom(), 'growth seen first by a scroll event is not leaving');
    f.resize();
    assert.equal(f.node.scrollTop, f.bottom());
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});

test('an image swapped in and loaded before the scroll event keeps the bottom', async () => {
  const f = fixture();
  try {
    f.rows()[19].h = 0;
    f.clamp();
    f.rows()[19].h = 340;
    f.scroll();
    assert.equal(f.node.scrollTop, f.bottom(), 'the clamp is not the reader leaving');
    f.setRows([...f.rows(), f.row('row-20', 80)]);
    f.resize();
    assert.equal(f.node.scrollTop, f.bottom());
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});

test('a wheel up while an image loads leaves the bottom', async () => {
  const f = fixture();
  try {
    f.rows()[19].h = 340;
    f.input('wheel', { deltaY: -120 });
    f.scroll(f.node.scrollTop - 120);
    const reading = f.node.scrollTop;
    f.setRows([...f.rows(), f.row('row-20', 80)]);
    f.resize();
    assert.equal(f.node.scrollTop, reading);
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});

test('a jump to a found message leaves the bottom', async () => {
  const f = fixture();
  try {
    f.window.__dxfChatScrollLeave();
    f.scroll(400);
    f.setRows([...f.rows(), f.row('row-20', 80)]);
    f.resize();
    assert.equal(f.node.scrollTop, 400);
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});

test('sending a message from history goes to the bottom', async () => {
  const f = fixture();
  try {
    f.scroll(300);
    f.window.__dxfChatScrollFollow();
    assert.equal(f.node.scrollTop, f.bottom());
    f.setRows([...f.rows(), f.row('mine', 80)]);
    f.resize();
    assert.equal(f.node.scrollTop, f.bottom(), 'and stays there for the message when it lands');
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});

test('a trackpad leaves the bottom in small steps and nothing pulls it back', async () => {
  const f = fixture();
  try {
    for (let i = 0; i < 6; i++) f.scroll(f.node.scrollTop - 10);
    const reading = f.node.scrollTop;
    f.setRows([...f.rows(), f.row('row-20', 80)]);
    f.resize();
    assert.equal(f.node.scrollTop, reading, 'a new message does not move a reader');
    f.scroll(f.bottom() - 20);
    f.setRows([...f.rows(), f.row('row-21', 80)]);
    f.resize();
    assert.equal(f.node.scrollTop, f.bottom(), 'returning near the bottom follows again');
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});

test('history above and reflow keep the line being read in place', async () => {
  const f = fixture();
  try {
    f.scroll(1000);
    const line = () => f.rows().find(r => r.dataset.chatRow === 'row-10').getBoundingClientRect().top;
    const before = line();
    f.setRows([...Array.from({ length: 5 }, (_, i) => f.row('old-' + i, 100)), ...f.rows()]);
    f.resize();
    assert.equal(line(), before, 'prepended history');
    f.rows()[2].h = 400;
    f.resize();
    assert.equal(line(), before, 'an image above loading');
    f.rows()[2].h = 50;
    f.scroll(f.node.scrollTop - 30);
    assert.equal(line(), before + 30, 'a reflow seen by a scroll event is corrected, the scroll kept');
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});

test('switching conversations follows the bottom again in the same frame', async () => {
  const f = fixture();
  try {
    f.scroll(300);
    f.node.dataset.chatChannel = 'two';
    f.setRows(Array.from({ length: 30 }, (_, i) => f.row('two-' + i, 60)));
    f.retitle();
    assert.equal(f.node.scrollTop, f.bottom());
    f.frame();
    assert.equal(f.samples.at(-1).channel, 'two');
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});

test('a hidden panel keeps its place', async () => {
  const f = fixture();
  try {
    f.scroll(600);
    f.node.clientHeight = 0;
    f.resize();
    f.node.clientHeight = 500;
    f.resize();
    assert.equal(f.node.scrollTop, 600);
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});

test('unmounting closes the bridge while Rust is silent', async () => {
  const f = fixture();
  f.window.__dxfChatScrollOff();
  await f.task;
  assert.equal(f.done(), true);
  assert.equal(f.node.listeners.size, 0);
  assert.equal(f.window.listeners.size, 0);
  assert.equal(f.window.__dxfChatScrollMeasure, null);
  assert.equal(f.window.__dxfChatScrollFollow, null);
});

test('the scroller is shown again once pinned', () => {
  // Dioxus restores a style property the new string leaves out (trap 31).
  const line = chat.split('\n').find(l => l.includes('visibility: hidden'));
  assert.match(line, /visibility: visible/);
});
