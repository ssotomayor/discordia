import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../client/assets/gif-playback.js', import.meta.url), 'utf8');
const gif = 'data:image/gif;base64,R0lGODlh';

function fixture({ hover = false, loaded = true, remote = false, focused = true } = {}) {
  const listeners = new Map(), pageListeners = new Map(), windowListeners = new Map(), observed = new Set();
  const attrs = new Map([['src', remote ? 'https://example.com/avatar.gif' : gif]]);
  if (hover) attrs.set('data-gif-hover', 'true');
  let intersections, mutations, posters = 0;
  const img = {
    complete: loaded, naturalWidth: 512, naturalHeight: 512, isConnected: true,
    getAttribute: key => attrs.get(key) ?? null,
    setAttribute: (key, value) => attrs.set(key, value),
    hasAttribute: key => attrs.has(key),
    matches: selector => selector === 'img', closest: () => null,
    querySelectorAll: () => [],
    addEventListener: (name, listener) => listeners.set(name, listener),
    removeEventListener: name => listeners.delete(name),
  };
  const document = {
    hidden: false, documentElement: {}, querySelectorAll: () => [img],
    hasFocus: () => focused,
    addEventListener: (name, listener) => pageListeners.set(name, listener),
    createElement: () => ({
      getContext: () => ({ drawImage() {} }),
      toDataURL() {
        if (remote) throw new Error('tainted');
        posters++;
        return `data:image/png;base64,poster${posters}`;
      },
    }),
  };
  vm.runInNewContext(source, {
    window: { addEventListener: (name, listener) => windowListeners.set(name, listener) }, document,
    IntersectionObserver: class {
      constructor(callback) { intersections = callback; }
      observe(target) { observed.add(target); }
      unobserve(target) { observed.delete(target); }
    },
    MutationObserver: class {
      constructor(callback) { mutations = callback; }
      observe() {}
    },
  });
  return {
    img, attrs, document, listeners, observed, posters: () => posters,
    visible(value) { intersections([{ target: img, isIntersecting: value, intersectionRatio: value ? 1 : 0 }]); },
    mutate() { mutations([{ type: 'attributes', target: img }]); },
    hidden(value) { document.hidden = value; pageListeners.get('visibilitychange')(); },
    focus(value) { focused = value; windowListeners.get(value ? 'focus' : 'blur')(); },
  };
}

test('guild icons animate only while hovered and visible', () => {
  const f = fixture({ hover: true });
  const poster = f.attrs.get('src');
  assert.match(poster, /^data:image\/png/);
  f.visible(true);
  assert.equal(f.attrs.get('src'), poster);
  f.listeners.get('mouseenter')();
  assert.equal(f.attrs.get('src'), gif);
  f.visible(false);
  assert.equal(f.attrs.get('src'), poster);
  f.visible(true);
  assert.equal(f.attrs.get('src'), gif);
  f.listeners.get('mouseleave')();
  assert.equal(f.attrs.get('src'), poster);
});

test('banners and chat GIFs resume on visibility without hover', () => {
  const f = fixture();
  f.visible(true);
  assert.equal(f.attrs.get('src'), gif);
  f.visible(false);
  assert.match(f.attrs.get('src'), /^data:image\/png/);
  f.visible(true);
  assert.equal(f.attrs.get('src'), gif);
  assert.equal(f.posters(), 1);
});

test('hidden windows stop GIFs and restore only visible ones', () => {
  const f = fixture();
  f.visible(true);
  f.hidden(true);
  assert.match(f.attrs.get('src'), /^data:image\/png/);
  f.hidden(false);
  assert.equal(f.attrs.get('src'), gif);
  f.visible(false);
  f.hidden(true);
  f.hidden(false);
  assert.match(f.attrs.get('src'), /^data:image\/png/);
});

test('poster writes do not replace the original or regenerate posters', () => {
  const f = fixture();
  f.mutate();
  f.mutate();
  f.visible(true);
  f.mutate();
  assert.equal(f.attrs.get('src'), gif);
  assert.equal(f.posters(), 1);
});

test('a replaced image gets its own poster and animation source', () => {
  const f = fixture();
  const replacement = `${gif}AAAA`;
  f.attrs.set('src', replacement);
  f.mutate();
  assert.equal(f.posters(), 2);
  f.visible(true);
  assert.equal(f.attrs.get('src'), replacement);
  f.attrs.set('src', 'data:image/png;base64,static');
  f.mutate();
  assert.equal(f.observed.size, 0);
  assert.equal(f.listeners.size, 0);
});

test('removed images release observers and handlers', () => {
  const f = fixture();
  f.img.isConnected = false;
  f.mutate();
  assert.equal(f.observed.size, 0);
  assert.equal(f.listeners.size, 0);
});

test('images still loading are frozen after load', () => {
  const f = fixture({ loaded: false, hover: true });
  assert.equal(f.attrs.get('src'), gif);
  f.visible(true);
  f.img.complete = true;
  f.listeners.get('load')();
  assert.match(f.attrs.get('src'), /^data:image\/png/);
});

test('remote GIFs without CORS remain usable', () => {
  const f = fixture({ remote: true });
  f.visible(true);
  f.visible(false);
  assert.equal(f.attrs.get('src'), 'https://example.com/avatar.gif');
});

test('banners, stickers and chat GIFs pause when a visible window loses focus', () => {
  const f = fixture();
  f.visible(true);
  assert.equal(f.attrs.get('src'), gif);
  f.focus(false);
  assert.equal(f.document.hidden, false);
  assert.match(f.attrs.get('src'), /^data:image\/png/);
  f.mutate();
  f.listeners.get('load')();
  f.visible(true);
  assert.match(f.attrs.get('src'), /^data:image\/png/);
  f.focus(true);
  assert.equal(f.attrs.get('src'), gif);
});

test('regaining focus never resumes an offscreen or hidden GIF', () => {
  const f = fixture();
  f.visible(true);
  f.focus(false);
  f.visible(false);
  f.focus(true);
  assert.match(f.attrs.get('src'), /^data:image\/png/);
  f.hidden(true);
  f.visible(true);
  f.focus(false);
  f.focus(true);
  assert.match(f.attrs.get('src'), /^data:image\/png/);
});

test('GIFs loaded in an unfocused window stay paused until focus returns', () => {
  const f = fixture({ focused: false, loaded: false });
  f.visible(true);
  f.img.complete = true;
  f.listeners.get('load')();
  assert.match(f.attrs.get('src'), /^data:image\/png/);
  f.focus(true);
  assert.equal(f.attrs.get('src'), gif);
});

test('guild hover cannot resume an animation in an unfocused window', () => {
  const f = fixture({ hover: true });
  f.visible(true);
  f.listeners.get('mouseenter')();
  assert.equal(f.attrs.get('src'), gif);
  f.focus(false);
  f.listeners.get('mouseenter')();
  assert.match(f.attrs.get('src'), /^data:image\/png/);
});
