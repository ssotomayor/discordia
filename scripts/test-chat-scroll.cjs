const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync('client/src/features/chat.rs', 'utf8').match(/const SCROLL_JS: &str = r#"([\s\S]*?)"#;/)[1];

function fixture() {
  const samples = [];
  let top = 0;
  const pages = [0, 1000, 2000].map((position, index) => ({
    dataset: {chatPage: `page-${index}`},
    getBoundingClientRect: () => ({top: position - top, bottom: position - top + 1000, height: 1000}),
  }));
  const node = {
    scrollHeight: 3000, clientHeight: 500,
    get scrollTop() { return top; },
    set scrollTop(value) { top = Math.max(0, Math.min(2500, value)); },
    getBoundingClientRect: () => ({top: 0}),
    querySelectorAll: selector => selector === '[data-chat-page]' ? pages : [],
    addEventListener() {}, removeEventListener() {},
  };
  const window = {};
  const task = vm.runInNewContext(`(function(){${source}})()`, {
    window, document: {getElementById: () => node},
    dioxus: {send: value => samples.push(value), recv: () => new Promise(() => {})},
    requestAnimationFrame: () => 1, cancelAnimationFrame() {},
  });
  return {node, window, samples, task};
}

test('opening a conversation measures the latest messages before any Rust response', async () => {
  const f = fixture();
  try {
    f.window.__dxfChatScrollMeasure('channel', 'dm');
    assert.equal(f.node.scrollTop, 2500);
    const sample = f.samples.at(-1);
    assert.equal(sample.mode, 'channel');
    assert.equal(sample.top, 2500);
    assert.equal(sample.pages.at(-1).top, -500);
    f.node.scrollTop = 200;
    f.window.__dxfChatScrollMeasure('channel', 'community');
    assert.equal(f.node.scrollTop, 2500);
    assert.equal(f.samples.at(-1).channel, 'community');
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});

test('ordinary resize and manual scrolling keep the selected history position', async () => {
  const f = fixture();
  try {
    f.window.__dxfChatScrollMeasure('channel', 'chat');
    f.node.scrollTop = 700;
    f.window.__dxfChatScrollMeasure('resize', 'chat');
    assert.equal(f.node.scrollTop, 700);
    f.window.__dxfChatScrollMeasure('scroll', 'chat');
    assert.equal(f.node.scrollTop, 700);
  } finally { f.window.__dxfChatScrollOff(); await f.task; }
});
