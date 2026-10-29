const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(`${__dirname}/../src/features/chat.rs`, 'utf8');
const script = source.match(/const COMPOSER_JS: &str = r#"([\s\S]*?)"#;/)[1]
  .replace('__ID__', 'composer').replace('__GEN__', 'test');
const text = value => ({ nodeType: 3, nodeValue: value });
const element = (tagName, children = [], dataset = {}) => ({
  nodeType: 1, tagName, childNodes: children, dataset,
});

function fixture() {
  const listeners = new Map();
  const messages = [];
  const inserted = [];
  const operations = [];
  const commands = [];
  const el = element('DIV');
  el.innerHTML = '';
  el.contains = node => node === el || el.childNodes.includes(node);
  el.focus = () => operations.push('focus');
  let before = [];
  const range = {
    startContainer: el,
    startOffset: 0,
    cloneRange() { operations.push('remember'); return { ...this }; },
    deleteContents() { operations.push('delete'); },
    collapse() {},
    insertNode(node) { inserted.push(node); },
    setStartAfter(node) { operations.push(['caret-after', node]); },
    setStart() {},
    setEnd() {},
    setEndBefore() {},
    cloneContents() { return { childNodes: before }; },
  };
  const selection = {
    rangeCount: 1,
    isCollapsed: true,
    getRangeAt: () => range,
    removeAllRanges() {},
    addRange() {},
  };
  const window = { getSelection: () => selection };
  const document = {
    getElementById: id => id === 'composer' ? el : null,
    createTextNode: text,
    createElement: tag => element(tag.toUpperCase()),
    createRange: () => range,
    execCommand: (...args) => { commands.push(args); return true; },
    addEventListener: (name, handler) => listeners.set(name, handler),
    removeEventListener: (name, handler) => {
      if (listeners.get(name) === handler) listeners.delete(name);
    },
  };
  const context = vm.createContext({ window, document, dioxus: {
    send: message => messages.push(JSON.parse(JSON.stringify(message))),
    recv: () => new Promise(() => {}),
  } });
  vm.runInContext(`(function () { ${script} })()`, context);
  return {
    el, messages, inserted, operations, commands, selection,
    api: window.__dxfComposers.composer,
    before: nodes => { before = nodes; },
    event(name, properties = {}) {
      let prevented = false;
      const event = { target: el, preventDefault: () => { prevented = true; }, ...properties };
      listeners.get(name)(event);
      return prevented;
    },
  };
}

{
  const f = fixture();
  assert.equal(f.event('keydown', { key: 'Enter' }), true);
  assert.deepEqual(f.messages, [{ k: 'submit' }]);
  f.api.suggest(true);
  f.event('keydown', { key: 'Enter' });
  assert.deepEqual(f.messages.at(-1), { k: 'pick' });
  f.messages.length = 0;
  assert.equal(f.event('keydown', { key: 'Enter', shiftKey: true }), true);
  assert.deepEqual(f.commands, [['insertLineBreak']]);
  assert.equal(f.messages.length, 0);
}

{
  const f = fixture();
  assert.equal(f.event('keydown', { key: 'Enter', shiftKey: true, isComposing: true }), false);
  assert.equal(f.event('keydown', { key: 'Enter', target: {} }), false);
  assert.equal(f.messages.length, 0);
  assert.equal(f.inserted.length, 0);
}

{
  const f = fixture();
  f.selection.isCollapsed = false;
  assert.equal(f.event('paste', { clipboardData: {
    items: [{ kind: 'string' }],
    getData: type => { assert.equal(type, 'text/plain'); return 'one\r\n\r\ntwo\rthree\n'; },
  } }), true);
  assert.deepEqual(f.commands, [['insertText', false, 'one\n\ntwo\nthree\n']]);
  assert.equal(f.event('paste', { clipboardData: {
    items: [{ kind: 'file' }],
    getData: () => { throw new Error('Files belong to the attachment bridge'); },
  } }), false);
  assert.equal(f.commands.length, 1);
}

{
  const f = fixture();
  f.selection.isCollapsed = false;
  f.event('selectionchange');
  f.api.insertEmoji('wave', 'wave.png', false, false);
  assert(f.operations.includes('delete'));
  assert.equal(f.inserted[0].dataset.code, 'wave');
  assert(f.operations.some(op => Array.isArray(op) && op[0] === 'caret-after' && op[1] === f.inserted[0]));
}

{
  const f = fixture();
  const emoji = element('IMG', [], { code: 'wave' });
  f.el.childNodes = [text('one\n\n'), emoji, text(' two'), element('BR'), text('three')];
  f.before([text('one\n\n'), emoji, text(' tw')]);
  f.event('input');
  assert.deepEqual(f.messages.at(-1), { k: 'input', text: 'one\n\n:wave: two\nthree', before: 'one\n\n:wave: tw' });
}

{
  const f = fixture();
  f.el.childNodes = [text('one'), element('BR'), element('BR')];
  f.before([text('one'), element('BR')]);
  f.event('input');
  assert.deepEqual(f.messages.at(-1), { k: 'input', text: 'one\n', before: 'one\n' });
  f.el.childNodes = [element('DIV', [text('one')]), element('DIV', [element('BR')]),
    element('P', [text('three'), element('BR')])];
  f.before([element('DIV', [text('one')]), element('DIV', [element('BR')]),
    element('P', [text('th')])]);
  f.event('input');
  assert.deepEqual(f.messages.at(-1), { k: 'input', text: 'one\n\nthree', before: 'one\n\nth' });
  f.el.childNodes = [element('BR')];
  f.el.innerHTML = '<br>';
  f.before([]);
  f.event('input');
  assert.equal(f.messages.at(-1).text, '');
  assert.equal(f.el.innerHTML, '');
}

{
  const f = fixture();
  assert.equal(f.event('keydown', { key: 'b', ctrlKey: true }), true);
  assert.equal(f.event('keydown', { key: 'I', metaKey: true }), true);
  assert.equal(f.event('keydown', { key: 'u', ctrlKey: true }), true);
  assert.deepEqual(f.inserted.map(n => n.nodeValue), ['**', '**', '*', '*', '__', '__']);
  assert.equal(f.commands.length, 0, 'the browser\'s own bold never runs: it is lost on send');
  assert.equal(f.messages.filter(m => m.k === 'input').length, 3);
  assert.equal(f.event('keydown', { key: 'b', ctrlKey: true, shiftKey: true }), false);
  assert.equal(f.event('keydown', { key: 'b', ctrlKey: true, target: {} }), false);
  assert.equal(f.inserted.length, 6);
}

{
  for (const [value, caret, start, end] of [
    ['hola @Mar', 9, 5, 9],
    ['@Álvaro', 3, 0, 7],
    ['hola @', 6, 5, 6],
    ['hola @Maria resto', 8, 5, 11],
  ]) {
    const f = fixture();
    const node = text(value);
    f.el.childNodes = [node];
    const range = f.selection.getRangeAt(0);
    range.startContainer = node;
    range.startOffset = caret;
    range.setStart = (_, offset) => f.operations.push(['replace-start', offset]);
    range.setEnd = (_, offset) => f.operations.push(['replace-end', offset]);
    f.event('selectionchange');
    f.api.insertText('@María ', true);
    assert(f.operations.some(op => Array.isArray(op) && op[0] === 'replace-start' && op[1] === start), value);
    assert(f.operations.some(op => Array.isArray(op) && op[0] === 'replace-end' && op[1] === end), value);
    assert.equal(f.inserted[0].nodeValue, '@María ');
    assert(f.operations.includes('delete'));
  }
}

{
  const f = fixture();
  const draft = text('mensaje sin enviar');
  f.el.childNodes = [draft];
  const range = f.selection.getRangeAt(0);
  range.startContainer = draft;
  range.startOffset = 7;
  f.event('selectionchange');
  const sent = f.messages.length;
  f.api.focus();
  assert(f.operations.includes('focus'));
  assert.equal(f.el.childNodes[0].nodeValue, 'mensaje sin enviar');
  assert.equal(f.selection.getRangeAt(0).startOffset, 7);
  assert.equal(f.messages.length, sent);
}

console.log('Chat composer tests passed');
