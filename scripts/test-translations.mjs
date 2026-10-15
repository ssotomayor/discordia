import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { test } from 'node:test';

const root = fileURLToPath(new URL('../client/', import.meta.url));
const messages = JSON.parse(readFileSync(join(root, 'locales/messages.json'), 'utf8'));

function placeholders(value) {
  return [...value.matchAll(/\{\d+\}/g)].map(match => match[0]).sort();
}

test('every catalog message has Spanish and Portuguese with the same arguments', () => {
  assert.ok(Object.keys(messages).length > 1000);
  for (const [source, translations] of Object.entries(messages)) {
    assert.equal(translations.length, 2, source);
    for (const translated of translations) {
      assert.ok(translated.trim(), source);
      assert.deepEqual(placeholders(translated), placeholders(source), `${source}: ${translated}`);
    }
  }
});

test('literal translation calls cannot silently fall back because a catalog entry is missing', () => {
  function scan(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) { scan(path); continue; }
      if (!entry.name.endsWith('.rs') || entry.name === 'i18n.rs') continue;
      const source = readFileSync(path, 'utf8');
      for (const match of source.matchAll(/i18n::(?:text|label|plural)\(\s*("(?:[^"\\]|\\.)*")/g)) {
        const key = JSON.parse(match[1]);
        assert.ok(Object.hasOwn(messages, key), `${path}: ${key}`);
      }
    }
  }
  scan(join(root, 'src'));
});
