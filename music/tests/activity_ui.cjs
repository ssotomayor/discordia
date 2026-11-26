const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const {webcrypto} = require('node:crypto');
const page = fs.readFileSync(path.join(__dirname, '../assets/index.html'), 'utf8');
const script = page.match(/<script>([\s\S]*?)<\/script>/)[1];

async function boot(role = 'player') {
  const elements = new Map(), handlers = new Map(), calls = [], timers = new Map();
  let counter = 0;
  const element = () => ({textContent: '', children: [], value: '', disabled: false,
    append(...children) { this.children.push(...children); }, replaceChildren(...children) { this.children = children; }});
  const get = id => { if (!elements.has(id)) elements.set(id, element()); return elements.get(id); };
  const info = {me: 'me', app: 'music-app', participants: [{pubkey: 'me', name: 'Me', role}]};
  const context = vm.createContext({
    document: {getElementById: get, createElement: element}, crypto: webcrypto, Uint8Array,
    window: {addEventListener() {}},
    setTimeout: fn => { timers.set(++counter, fn); return counter; }, clearTimeout: id => timers.delete(id),
    discordia: {on: (event, fn) => handlers.set(event, fn), call: async (method, args) => {
      if (method === 'instance.info') return info;
      calls.push({method, args});
    }},
  });
  vm.runInContext(script, context);
  await new Promise(resolve => setImmediate(resolve));
  const message = (data, from = 'music-app') => handlers.get('instance.message')({from, data});
  const state = (extra = {}) => ({t: 'state', session_epoch: 'epoch-1', revision: 1, host: 'me', playback: 'not_connected', queue_limit: 8, queue: [], ...extra});
  if (role === 'player') message(state({ack: calls.at(-1).args.data.request_id}));
  else message(state());
  return {get, calls, message, state, context, timers};
}

test('the queue trusts only its installed service and treats titles as text', async () => {
  const ui = await boot();
  const track = {id: 'song-1', added_by: 'me', title: '<img src=x onerror=alert(1)>', source: {provider: 'youtube', canonical_url: 'https://youtube.com/'}, metadata: 'metadata_available'};
  ui.message(ui.state({revision: 99, queue: [track]}), 'another-participant');
  assert.equal(ui.get('queue').children.length, 0);
  ui.message(ui.state({revision: 2, queue: [track]}));
  const title = ui.get('queue').children[0].children[0].children[0];
  assert.equal(title.textContent, track.title);
  assert.equal(title.children.length, 0);
  assert.equal(title.innerHTML, undefined);
  ui.message(ui.state({revision: 1}));
  assert.equal(ui.get('queue').children.length, 1);
});

test('unrelated metadata updates cannot acknowledge a pending command', async () => {
  const ui = await boot();
  ui.get('url').value = 'https://youtu.be/dQw4w9WgXcQ';
  ui.get('add-form').onsubmit({preventDefault() {}});
  const request = ui.calls.at(-1).args.data.request_id;
  assert.match(request, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  assert.equal(ui.get('add').disabled, true);
  ui.message(ui.state({revision: 2, ack: null}));
  assert.equal(ui.get('add').disabled, true);
  ui.message(ui.state({revision: 3, ack: request}));
  assert.equal(ui.get('add').disabled, false);
  assert.equal(ui.timers.size, 0);
});

test('spectators cannot send queue commands or request source lookups', async () => {
  const ui = await boot('spectator');
  ui.get('add-form').onsubmit({preventDefault() {}});
  ui.get('clear').onclick();
  assert.equal(ui.calls.length, 0);
  assert.equal(ui.get('add').disabled, true);
  assert.equal(ui.get('clear').disabled, true);
});

test('a timeout releases controls and explains how to recover', async () => {
  const ui = await boot();
  ui.get('refresh').onclick();
  for (const callback of [...ui.timers.values()]) callback();
  assert.match(ui.get('error').textContent, /Refresh to check the queue/);
  assert.equal(ui.get('refresh').disabled, false);
  assert.equal(ui.timers.size, 0);
});

test('a service restart resets the revision baseline and clears an outstanding command', async () => {
  const ui = await boot();
  ui.message(ui.state({revision: 50}));
  ui.get('refresh').onclick();
  ui.message(ui.state({session_epoch: 'epoch-2', revision: 0}));
  assert.equal(ui.get('refresh').disabled, false);
  assert.match(ui.get('error').textContent, /service restarted/);
  assert.equal(ui.timers.size, 0);
});

test('an error for another command does not cancel this window\'s pending request', async () => {
  const ui = await boot();
  ui.get('refresh').onclick();
  ui.message({t: 'error', request_id: 'another-window-request', message: 'Other request failed'});
  assert.equal(ui.get('refresh').disabled, true);
  assert.equal(ui.get('error').textContent, '');
  const request = ui.calls.at(-1).args.data.request_id;
  ui.message({t: 'error', request_id: request, message: 'This request failed'});
  assert.equal(ui.get('refresh').disabled, false);
  assert.equal(ui.get('error').textContent, 'This request failed');
});
