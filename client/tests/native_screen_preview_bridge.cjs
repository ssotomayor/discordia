const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const script = fs.readFileSync(path.join(__dirname, '../assets/native-screen-preview.js'), 'utf8');
const sent = [], listeners = new Map(), raf = new Map();
let covered = false, frame = 0, mutations, resized;
const element = { isConnected: true, contains: node => node === element,
  rect: { left: 10, top: 20, width: 200, height: 100, right: 210, bottom: 120 },
  getBoundingClientRect() { return this.rect; } };
const document = { hidden: false, documentElement: {}, getElementById: () => element,
  elementFromPoint: () => covered ? {} : element,
  addEventListener: (name, fn) => listeners.set(name, fn), removeEventListener: name => listeners.delete(name) };
const window = { innerWidth: 400, innerHeight: 800,
  addEventListener: (name, fn) => listeners.set(`window:${name}`, fn), removeEventListener: name => listeners.delete(`window:${name}`) };
const context = vm.createContext({ document, window, dioxus: { send: value => sent.push(value) },
  getComputedStyle: () => ({ display: 'flex', visibility: 'visible' }),
  requestAnimationFrame: callback => { raf.set(++frame, callback); return frame; }, cancelAnimationFrame: id => raf.delete(id),
  MutationObserver: class { constructor(fn) { mutations = (records = [{ type: 'childList' }]) => fn(records); } observe() {} disconnect() {} },
  ResizeObserver: class { constructor(fn) { resized = fn; } observe() {} disconnect() {} },
});
function flush() { for (const [id, callback] of [...raf]) { raf.delete(id); callback(); } }
vm.runInContext(`(${script})('preview', 1)`, context);
assert.equal(sent.at(-1).visible, true);
assert.equal(sent.at(-1).width, 200);
assert.equal(raf.size, 0, 'idle preview must not run an animation loop');
mutations([{ type: 'attributes', target: { contains: () => false, closest: () => null } }]);
assert.equal(raf.size, 0, 'unrelated voice meter styles must not measure preview geometry');
resized(); mutations(); resized();
assert.equal(raf.size, 1, 'geometry updates must coalesce');
const before = sent.length;
flush();
assert.equal(sent.length, before, 'unchanged bounds must not cross JNI again');
covered = true; mutations(); flush();
assert.equal(sent.at(-1).visible, false, 'native video must not cover a dialog');
covered = false; mutations(); flush();
assert.equal(sent.at(-1).visible, true);
element.rect.left = -20; listeners.get('scroll')(); flush();
assert.equal(sent.at(-1).visible, false, 'a partially swiped panel must hide its native surface');
element.rect.left = 10; listeners.get('scroll')(); flush();
assert.equal(sent.at(-1).visible, true);
document.hidden = true; listeners.get('visibilitychange')(); flush();
assert.equal(sent.at(-1).visible, false);
document.hidden = false; listeners.get('visibilitychange')(); flush();
const previous = window.__dxfNativeScreenPreview;
vm.runInContext(`(${script})('preview', 2)`, context);
previous.stop();
assert.equal(window.__dxfNativeScreenPreview.revision, 2);
element.isConnected = false; mutations(); flush();
assert.equal(sent.at(-1).visible, false);
assert.equal(window.__dxfNativeScreenPreview, undefined);
assert.equal(raf.size, 0);
assert.equal(listeners.size, 0);
console.log('Native screen preview follows geometry, hides behind dialogs and stops without idle polling.');
