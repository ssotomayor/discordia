const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(`${__dirname}/../src/features/chat.rs`, 'utf8');
const script = source.match(/const PASTE_JS: &str = r#"([\s\S]*?)"#;/)[1];
const listeners = [];
const messages = [];
const context = vm.createContext({
  window: {},
  document: { addEventListener: (name, handler) => {
    assert.equal(name, 'paste');
    listeners.push(handler);
  } },
  dioxus: { send: message => messages.push(message) },
});
vm.runInContext(script, context);
vm.runInContext(script, context);
assert.equal(listeners.length, 1);
function paste(inChat, items) {
  let prevented = false;
  listeners[0]({
    target: { closest: () => inChat },
    clipboardData: { items },
    preventDefault: () => { prevented = true; },
  });
  return prevented;
}
const image = { kind: 'file', type: 'image/png', getAsFile: () => {
  throw new Error('Image bytes must be read in Rust');
} };
assert.equal(paste(false, [image]), false);
assert.equal(paste(true, [{ kind: 'string', type: 'text/plain' }]), false);
assert.equal(paste(true, [image, image]), true);
assert.equal(messages.length, 1);
assert.equal(JSON.stringify(messages[0]), '{"k":"paste"}');
assert(!source.includes('FileReader'));
console.log('Chat attachment bridge tests passed');
