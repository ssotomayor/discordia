const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const { test } = require('node:test');
const script = fs.readFileSync(`${__dirname}/../assets/text-menu.js`, 'utf8');

function fixture() {
  class Element {
    constructor(tag = 'div', attrs = {}) {
      this.tagName = tag.toUpperCase(); this.attrs = attrs; this.childNodes = [];
      this.listeners = new Map(); this.style = { setProperty() {} }; this.dataset = {};
      this.isConnected = true; this.type = attrs.type || 'text'; this.value = '';
      this.textContent = ''; this.title = ''; this.id = attrs.id || '';
    }
    setAttribute(key, value) { this.attrs[key] = value; }
    getAttribute(key) { return this.attrs[key] || ''; }
    append(...children) { for (const child of children) { child.parent = this; this.childNodes.push(child); } }
    remove() { this.isConnected = false; if (this.parent) this.parent.childNodes = this.parent.childNodes.filter(c => c !== this); }
    contains(node) { return this === node || this.childNodes.some(child => child.contains(node)); }
    matches(selector) {
      return selector.split(',').some(value => {
        value = value.trim();
        if (value.startsWith('#')) return this.id === value.slice(1);
        if (value.startsWith('[')) {
          const [, key, expected] = value.match(/^\[([^=\]]+)(?:="([^"]*)")?\]$/);
          if (key.startsWith('data-context-')) {
            const name = key.slice(5).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
            if (Object.hasOwn(this.dataset, name)) return expected === undefined || this.dataset[name] === expected;
          }
          return Object.hasOwn(this.attrs, key) && (expected === undefined || this.attrs[key] === expected);
        }
        return this.tagName === value.toUpperCase();
      });
    }
    closest(selector) { return this.matches(selector) ? this : this.parent?.closest(selector) || null; }
    querySelectorAll(selector) { return this.childNodes.flatMap(child => [...(child.matches(selector) ? [child] : []), ...child.querySelectorAll(selector)]); }
    querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
    addEventListener(name, fn) { if (!this.listeners.has(name)) this.listeners.set(name, []); this.listeners.get(name).push(fn); }
    removeEventListener(name, fn) { this.listeners.set(name, (this.listeners.get(name) || []).filter(f => f !== fn)); }
    dispatchEvent(event) { event.target ||= this; for (const fn of this.listeners.get(event.type) || []) fn(event); }
    focus() { document.activeElement = this; }
    click() { this.dispatchEvent({ type: 'click' }); }
    getBoundingClientRect() { return { width: 220, height: 200 }; }
    setSelectionRange(start, end) { this.selectionStart = start; this.selectionEnd = end; }
    setRangeText(text, start, end) { this.value = this.value.slice(0, start) + text + this.value.slice(end); }
    select() { this.setSelectionRange(0, this.value.length); }
  }
  const document = new Element();
  document.body = new Element(); document.createElement = tag => new Element(tag);
  document.execCommand = (...args) => commands.push(args);
  const commands = [], requests = [];
  let receive;
  const selection = { rangeCount: 0, isCollapsed: true };
  const window = new Element();
  window.getSelection = () => selection;
  window.__dxfTextMenuLabels = { copy: 'Copy', message: 'Copy message', cut: 'Cut', paste: 'Paste', all: 'Select all', react: 'Add reaction', error: 'Clipboard error' };
  vm.runInNewContext(script, { window, document, navigator: { platform: 'Win32' }, innerWidth: 800, innerHeight: 600,
    getComputedStyle: () => ({ getPropertyValue: () => '#123', fontFamily: 'sans-serif' }),
    Event: class { constructor(type, options) { this.type = type; Object.assign(this, options); } },
    dioxus: { send: value => requests.push(value), recv: () => new Promise(resolve => { receive = resolve; }) },
  });
  function event(type, target, extra = {}) {
    const e = { type, target, clientX: 799, clientY: 599, preventDefault() { this.defaultPrevented = true; }, ...extra };
    document.dispatchEvent(e); return e;
  }
  const menu = () => document.body.childNodes.find(e => e.className === 'dxf-text-menu');
  return { Element, document, window, requests, commands, event, menu,
    field(value = 'abcd') { const el = new Element('textarea'); el.value = value; el.setSelectionRange(1, 3); document.body.append(el); return el; },
    labels: () => menu().querySelectorAll('button').map(b => b.childNodes[0]?.textContent || b.textContent),
    choose(label) { const b = menu().querySelectorAll('button').find(b => (b.childNodes[0]?.textContent || b.textContent) === label); assert.ok(b, label); b.click(); },
    async reply(value) { receive(value); await new Promise(resolve => setImmediate(resolve)); },
  };
}

test('copy message preserves raw text and reuses only available message actions', async () => {
  const f = fixture();
  const row = new f.Element('div', { 'data-context-message': '<b>literal</b>\n:wave:' });
  const action = new f.Element('button'); action.dataset.contextAction = 'reply'; action.title = 'Reply';
  let replies = 0; action.addEventListener('click', () => replies++); row.append(action); f.document.body.append(row);
  assert.equal(f.event('contextmenu', row).defaultPrevented, true);
  assert.deepEqual(f.labels(), ['Reply', 'Copy message']);
  assert.equal(f.menu().style.left, '572px'); assert.equal(f.menu().style.top, '392px');
  f.choose('Copy message'); assert.equal(f.requests[0].text, '<b>literal</b>\n:wave:');
  await f.reply({ ok: true }); assert.equal(f.menu(), undefined);
  f.event('contextmenu', row); f.choose('Reply'); assert.equal(replies, 1);
  assert.equal(f.requests.length, 1, 'message actions use the existing permission-gated handler');
});

test('paste replaces the original selection and notifies controlled inputs', async () => {
  const f = fixture(), input = f.field(); let changes = 0;
  input.addEventListener('input', () => changes++);
  f.event('contextmenu', input); f.choose('Paste');
  input.setSelectionRange(0, 0);
  await f.reply({ ok: true, text: '<script>literal</script>\r\nnext' });
  assert.equal(input.value, 'a<script>literal</script>\nnextd'); assert.equal(changes, 1);
});

test('failed clipboard writes never delete cut text', async () => {
  const f = fixture(), input = f.field(); f.event('contextmenu', input); f.choose('Cut');
  await f.reply({ ok: false }); assert.equal(input.value, 'abcd');
  assert.equal(f.menu().querySelector('[role="status"]').textContent, 'Clipboard error');
});

test('dismissed menus and removed fields cannot receive delayed paste', async () => {
  for (const dismiss of [f => f.event('pointerdown', f.document.body), (f, field) => field.remove()]) {
    const f = fixture(), field = f.field(); f.event('contextmenu', field); f.choose('Paste');
    dismiss(f, field); await f.reply({ ok: true, text: 'late' }); assert.equal(field.value, 'abcd');
  }
});

test('passwords hide copy/cut and readonly fields hide paste/cut', () => {
  const f = fixture(), password = f.field(); password.tagName = 'INPUT'; password.type = 'password';
  f.event('contextmenu', password); assert.deepEqual(f.labels(), ['Paste', 'Select all']);
  const readonly = f.field(); readonly.readOnly = true;
  f.event('contextmenu', readonly); assert.deepEqual(f.labels(), ['Copy', 'Select all']);
});

test('image paste is restricted to the chat attachment bridge', async () => {
  const f = fixture(), input = f.field(); let images = 0; f.window.__dxfPasteSink = () => images++;
  f.event('contextmenu', input); f.choose('Paste'); await f.reply({ ok: true, image: true }); assert.equal(images, 0);
  const chat = new f.Element('div', { id: 'dxf-chat-drop' }); chat.append(input);
  f.event('contextmenu', input); f.choose('Paste'); await f.reply({ ok: true, image: true }); assert.equal(images, 1);
});

test('keyboard navigation, escape and teardown release the menu', () => {
  const f = fixture(), input = f.field(); f.event('contextmenu', input);
  f.event('keydown', input, { key: 'End' }); assert.equal(f.document.activeElement.childNodes[0].textContent, 'Select all');
  f.event('keydown', input, { key: 'Escape' }); assert.equal(f.menu(), undefined); assert.equal(f.document.activeElement, input);
  f.window.__dxfTextMenuOff(); assert.equal(f.event('contextmenu', input).defaultPrevented, undefined);
});

test('saved messages expose the personal bookmark action even without text', () => {
  const f = fixture(), row = new f.Element('article', { 'data-context-message': '' });
  const action = new f.Element('button'); action.dataset.contextAction = 'bookmark'; action.title = 'Remove bookmark';
  let removed = 0; action.addEventListener('click', () => removed++); row.append(action); f.document.body.append(row);
  f.event('contextmenu', row); assert.deepEqual(f.labels(), ['Remove bookmark']);
  assert.equal(f.menu().querySelector('[role="separator"]'), null);
  f.choose('Remove bookmark'); assert.equal(removed, 1);
});
