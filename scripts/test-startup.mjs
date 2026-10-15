import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../client/assets/startup.js', import.meta.url), 'utf8');

function fixture({ mounted = false, fonts = Promise.resolve() } = {}) {
  const attrs = new Map([['inert', ''], ['aria-hidden', 'true']]);
  const frames = [];
  let mutate, disconnected = false, removed = 0;
  const main = {
    querySelector: () => mounted ? {} : null,
    setAttribute: (key, value) => attrs.set(key, value),
    removeAttribute: key => attrs.delete(key),
  };
  vm.runInNewContext(source, {
    document: {
      getElementById: id => id === 'main' ? main : { remove: () => removed++ },
      fonts: fonts === null ? undefined : { ready: fonts },
    },
    requestAnimationFrame: callback => frames.push(callback),
    MutationObserver: class {
      constructor(callback) { mutate = callback; }
      observe(target, options) {
        assert.equal(target, main);
        assert.equal(options.childList, true);
        assert.equal(options.subtree, true);
      }
      disconnect() { disconnected = true; }
    },
  });
  return {
    attrs, frames,
    mount() { mounted = true; mutate(); },
    mutate: () => mutate(),
    removed: () => removed,
    disconnected: () => disconnected,
  };
}

const flush = () => new Promise(resolve => setImmediate(resolve));

test('cold start keeps the interface hidden until mounted, fonts ready and a frame scheduled', async () => {
  let ready;
  const f = fixture({ fonts: new Promise(resolve => { ready = resolve; }) });
  f.mutate();
  await flush();
  assert.equal(f.frames.length, 0);
  assert.equal(f.attrs.has('data-ready'), false);
  f.mount();
  await flush();
  assert.equal(f.disconnected(), true);
  assert.equal(f.frames.length, 0);
  assert.equal(f.attrs.has('inert'), true);
  assert.equal(f.attrs.get('aria-hidden'), 'true');
  ready();
  await flush();
  assert.equal(f.frames.length, 1);
  assert.equal(f.removed(), 0);
  f.frames.shift()();
  assert.equal(f.attrs.has('data-ready'), true);
  assert.equal(f.attrs.has('inert'), false);
  assert.equal(f.attrs.has('aria-hidden'), false);
  assert.equal(f.removed(), 1);
});

test('an already mounted interface is revealed once despite repeated mutations', async () => {
  const f = fixture({ mounted: true });
  f.mutate();
  f.mount();
  await flush();
  assert.equal(f.frames.length, 1);
  f.frames.shift()();
  f.mutate();
  await flush();
  assert.equal(f.frames.length, 0);
  assert.equal(f.removed(), 1);
});

test('missing or failed font readiness does not strand the loading screen', async () => {
  for (const fonts of [() => null, () => Promise.reject(new Error('font unavailable'))]) {
    const f = fixture({ mounted: true, fonts: fonts() });
    await flush();
    assert.equal(f.frames.length, 1);
    f.frames.shift()();
    assert.equal(f.removed(), 1);
  }
});
