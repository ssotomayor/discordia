import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { join } from 'node:path';
import { test } from 'node:test';
import { Script, runInNewContext } from 'node:vm';

const root = fileURLToPath(new URL('../client/', import.meta.url));
const catalogSource = readFileSync(join(root, 'locales/messages.json'), 'utf8');
const messages = JSON.parse(catalogSource);

function placeholders(value) {
  return [...value.matchAll(/\{\d+\}/g)].map(match => match[0]).sort();
}

function matchLabelKeys(source) {
  return [...source.matchAll(/i18n::label\(\s*match\b[^{}]*\{([^{}]*)\}\s*\)/g)]
    .flatMap(match => [...match[1].matchAll(/=>\s*("(?:[^"\\]|\\.)*")/g)]
      .map(arm => JSON.parse(arm[1])));
}

test('translation coverage includes literal labels selected by match arms', () => {
  const source = 'crate::i18n::label(match route { Route::Ipv4 => "IPv4", Route::Local => "LAN", })';
  assert.deepEqual(matchLabelKeys(source), ['IPv4', 'LAN']);
  assert.deepEqual(matchLabelKeys('match route { Route::Local => "not a translated label" }'), []);
});

test('catalog keys are unique before JSON parsing can hide duplicates', () => {
  const keys = [...catalogSource.matchAll(/^\s*("(?:[^"\\]|\\.)*")\s*:/gm)]
    .map(match => JSON.parse(match[1]));
  assert.equal(new Set(keys).size, keys.length, 'duplicate catalog key');
});

test('every catalog message has Spanish, Portuguese, Simplified Chinese and Japanese with the same arguments', () => {
  assert.ok(Object.keys(messages).length > 1000);
  for (const [source, translations] of Object.entries(messages)) {
    assert.equal(translations.length, 4, source);
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
      for (const key of matchLabelKeys(source)) {
        assert.ok(Object.hasOwn(messages, key), `${path}: match label ${key}`);
      }
      for (const match of source.matchAll(/i18n::(?:text|label|plural)\(\s*("(?:[^"\\]|\\.)*")/g)) {
        const key = JSON.parse(match[1]);
        assert.ok(Object.hasOwn(messages, key), `${path}: ${key}`);
      }
      for (const match of source.matchAll(/i18n::plural\(\s*"(?:[^"\\]|\\.)*"\s*,\s*("(?:[^"\\]|\\.)*")/g)) {
        const key = JSON.parse(match[1]);
        assert.ok(Object.hasOwn(messages, key), `${path}: plural ${key}`);
      }
    }
  }
  scan(join(root, 'src'));
});

test('built-in game scripts translate registered messages and preserve player text', () => {
  const source = readFileSync(join(root, 'src/features/activities.rs'), 'utf8');
  const keys = [...source.match(/const BUILTIN_MESSAGES:[\s\S]*?=\s*&\[([\s\S]*?)\];/)[1]
    .matchAll(/"(?:[^"\\]|\\.)*"/g)].map(match => JSON.parse(match[0]));
  for (const key of keys) assert.ok(Object.hasOwn(messages, key), key);
  const pages = [...source.matchAll(/const (?:DICE|RPS)_HTML: &str = r##"([\s\S]*?)"##;/g)];
  assert.equal(pages.length, 2);
  for (const page of pages) {
    const script = page[1].match(/<script>([\s\S]*?)<\/script>/)[1];
    new Script(script.replace('/* BUILTIN_TRANSLATIONS */', 'const translations = {};'));
    for (const match of script.matchAll(/\btr\(\s*(?:'([^']*)'|"([^"]*)")/g)) {
      assert.ok(keys.includes(match[1] ?? match[2]), match[0]);
    }
    const interpolate = script.match(/const tr = [^\n]+/)[0];
    for (const column of [-1, 0, 1, 2, 3]) {
      const dictionary = Object.fromEntries(keys.map(key => [key, column < 0 ? key : messages[key][column]]));
      const tr = runInNewContext(`const translations = ${JSON.stringify(dictionary)}; ${interpolate}; tr`);
      const expected = column < 0 ? 'Hi, Ana {0} <b>' : ['Hola, Ana {0} <b>', 'Olá, Ana {0} <b>', '你好，Ana {0} <b>', 'こんにちは、Ana {0} <b>'][column];
      assert.equal(tr('Hi, {0}', 'Ana {0} <b>'), expected);
    }
  }
});
