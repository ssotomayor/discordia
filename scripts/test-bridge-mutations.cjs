const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

const root = path.resolve(__dirname, '..');
const sourcePath = path.join(root, 'client/src/features/chat.rs');
const composer = 'client/tests/chat_composer.cjs';
const attachment = 'client/tests/chat_attachment_bridge.cjs';
const mutations = [
  {
    name: 'shift-enter-submits', test: composer,
    from: "if (e.shiftKey) document.execCommand('insertLineBreak');",
    to: "if (false) document.execCommand('insertLineBreak');",
  },
  {
    name: 'enter-interrupts-ime', test: composer,
    from: 'if (!within(e) || e.isComposing) return;',
    to: 'if (!within(e)) return;',
  },
  {
    name: 'paste-flattens-lines', test: composer,
    from: String.raw`.replace(/\r\n?/g, '\n')`,
    to: String.raw`.replace(/[\r\n]+/g, ' ')`,
  },
  {
    name: 'emoji-loses-shortcode-delimiters', test: composer,
    from: "out += ':' + n.dataset.code + ':';",
    to: 'out += n.dataset.code;',
  },
  {
    name: 'browser-breaks-disappear', test: composer,
    from: "if (!last || partial) out += '\\n';",
    to: "if (false) out += '\\n';",
  },
  {
    name: 'block-boundaries-disappear', test: composer,
    from: "if (index && (block || previousBlock)) out += '\\n';",
    to: "if (false) out += '\\n';",
  },
  {
    name: 'paste-listener-duplicates-on-remount', test: attachment,
    from: 'if (window.__dxfPasteWired) return;',
    to: 'void window.__dxfPasteWired;',
  },
  {
    name: 'image-paste-intercepts-other-fields', test: attachment,
    from: "if (!e.target.closest || !e.target.closest('#dxf-chat-drop')) return;",
    to: 'void e.target;',
  },
  {
    name: 'image-paste-intercepts-other-files', test: attachment,
    from: "items[i].kind === 'file' && items[i].type.indexOf('image/') === 0",
    to: "items[i].kind === 'file'",
  },
];

function mutate(source, mutation) {
  assert.equal(source.split(mutation.from).length - 1, 1,
    `${mutation.name}: mutation anchor must occur exactly once`);
  return source.replace(mutation.from, mutation.to);
}

if (process.argv[2] === '--mutant') {
  const mutation = mutations.find(item => item.name === process.argv[3]);
  assert(mutation, 'Unknown mutation');
  const readFileSync = fs.readFileSync;
  const modified = mutate(readFileSync(sourcePath, 'utf8'), mutation);
  // In-memory interception leaves the checkout intact, even after a failed test.
  fs.readFileSync = function (file, options) {
    if (typeof file === 'string' && path.resolve(file) === sourcePath) {
      const encoding = typeof options === 'string' ? options : options?.encoding;
      return encoding ? Buffer.from(modified).toString(encoding) : Buffer.from(modified);
    }
    return readFileSync.apply(this, arguments);
  };
  require(path.join(root, mutation.test));
} else {
  const source = fs.readFileSync(sourcePath, 'utf8');
  for (const mutation of mutations) mutate(source, mutation);
  const run = args => spawnSync(process.execPath, args, {
    cwd: root, encoding: 'utf8', timeout: 10_000,
  });
  for (const test of new Set(mutations.map(mutation => mutation.test))) {
    const result = run([test]);
    assert.equal(result.status, 0, `Baseline failed: ${test}\n${result.error || result.stderr}`);
    console.log(`BASELINE ${test}`);
  }
  const survivors = [];
  for (const mutation of mutations) {
    const result = run([__filename, '--mutant', mutation.name]);
    if (result.status === 0) {
      survivors.push(mutation.name);
      console.log(`SURVIVED ${mutation.name}`);
      continue;
    }
    // A syntax error, timeout or broken harness is not evidence of regression coverage.
    assert(!result.error && result.status === 1 && /^AssertionError \[ERR_ASSERTION\]:/m.test(result.stderr),
      `Invalid mutation result: ${mutation.name}\n${result.error || result.stderr}`);
    console.log(`KILLED ${mutation.name}`);
  }
  console.log(`${mutations.length - survivors.length}/${mutations.length} mutations killed`);
  assert.deepEqual(survivors, [], `Surviving mutations: ${survivors.join(', ')}`);
}
