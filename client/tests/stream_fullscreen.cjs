const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { test } = require('node:test');
const script = fs.readFileSync(`${__dirname}/../assets/stream-fullscreen-keys.js`, 'utf8');

function fixture() {
  const window = new EventTarget();
  const sent = [];
  let active = false;
  const context = vm.createContext({
    window,
    document: { querySelector: () => active ? {} : null },
    dioxus: { send: value => sent.push(value) },
  });
  const install = () => vm.runInContext(`(function () { ${script} })()`, context);
  const key = value => {
    const event = new Event('keydown', { cancelable: true });
    event.key = value;
    window.dispatchEvent(event);
    return event;
  };
  return { window, sent, install, key, setActive: value => { active = value; } };
}

test('Escape exits only while a stream is fullscreen and leaves other keys alone', async () => {
  const f = fixture(), done = f.install();
  assert.equal(f.key('Escape').defaultPrevented, false);
  f.setActive(true);
  assert.equal(f.key('Enter').defaultPrevented, false);
  assert.equal(f.key('Escape').defaultPrevented, true);
  assert.deepEqual(f.sent, [null]);
  f.window.__dxfStreamEscapeOff();
  await done;
});

test('unmount removes the key listener and finishes the eval', async () => {
  const f = fixture(), done = f.install();
  f.setActive(true);
  f.window.__dxfStreamEscapeOff();
  await done;
  assert.equal(f.key('Escape').defaultPrevented, false);
  assert.deepEqual(f.sent, []);
  assert.equal(f.window.__dxfStreamEscapeOff, undefined);
});

test('remount replaces the listener without an older eval removing the new handler', async () => {
  const f = fixture(), old = f.install(), current = f.install();
  await old;
  f.setActive(true);
  f.key('Escape');
  assert.deepEqual(f.sent, [null]);
  f.window.__dxfStreamEscapeOff();
  await current;
});
