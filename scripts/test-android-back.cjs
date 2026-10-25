const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { test } = require('node:test');
const vm = require('node:vm');

const source = readFileSync('client/assets/android-back.js', 'utf8');

test('native back is consumed and dispatched once per press, then cleaned up', async () => {
  const listeners = [];
  const sent = [];
  const window = { addEventListener(name, callback, options) {
    assert.equal(name, 'pagehide');
    assert.equal(options.once, true);
    listeners.push(callback);
  } };
  const pending = vm.runInNewContext(`(async () => {${source}\n})()`, { window, dioxus: { send: value => sent.push(value) } });
  assert.equal(window.__dxfAndroidBack(), true);
  assert.equal(window.__dxfAndroidBack(), true);
  assert.deepEqual(sent, [null, null]);
  listeners[0]();
  await pending;
  assert.equal(window.__dxfAndroidBack, undefined);
});

test('late cleanup cannot remove the replacement page handler', async () => {
  let close;
  const window = { addEventListener(_name, callback) { close = callback; } };
  const pending = vm.runInNewContext(`(async () => {${source}\n})()`, { window, dioxus: { send() {} } });
  const replacement = () => true;
  window.__dxfAndroidBack = replacement;
  close();
  await pending;
  assert.equal(window.__dxfAndroidBack, replacement);
});
