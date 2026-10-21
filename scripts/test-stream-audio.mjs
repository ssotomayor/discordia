import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../client/src/features/screenshare.rs', import.meta.url), 'utf8');
const bridge = source.split('pub(crate) const SCREEN_JS: &str = r#"')[1].split('"#;')[0];

function sinkFixture(enumerateDevices, setSinkId) {
  const warnings = [];
  const context = vm.createContext({
    audioTracks: { alice: { setSinkId } },
    navigator: enumerateDevices ? { mediaDevices: { enumerateDevices } } : {},
    console: { warn(...args) { warnings.push(args); } },
  });
  vm.runInContext(bridge.slice(bridge.indexOf('  const audioElements = {}'), bridge.indexOf('  function setStreamVolume')), context);
  vm.runInContext('audioElements.alice = {};', context);
  return { set: value => context.setSink(value), warnings };
}

const flushPromises = () => new Promise(resolve => setImmediate(resolve));

test('system default clears an explicit sink without requiring device enumeration', async () => {
  const calls = [];
  let enumerations = 0;
  const f = sinkFixture(async () => { enumerations++; return [{ kind: 'audiooutput', label: 'Headphones', deviceId: 'headphones' }]; }, async id => calls.push(id));
  f.set('Headphones');
  await flushPromises();
  f.set(null);
  await flushPromises();
  assert.deepEqual(calls, ['headphones', '']);
  assert.equal(enumerations, 1);
  const withoutEnumeration = sinkFixture(null, async id => calls.push(id));
  withoutEnumeration.set(null);
  await flushPromises();
  assert.equal(calls.at(-1), '');
});

test('an old device lookup cannot override a newer selection', async () => {
  let finishLookup;
  const calls = [];
  const f = sinkFixture(() => new Promise(resolve => { finishLookup = resolve; }), async id => calls.push(id));
  f.set('Headphones');
  f.set(null);
  await flushPromises();
  finishLookup([{ kind: 'audiooutput', label: 'Headphones', deviceId: 'headphones' }]);
  await flushPromises();
  assert.deepEqual(calls, ['']);
});

test('a pending sink change settles before the newest sink is applied', async () => {
  const calls = [];
  let finishFirst;
  const f = sinkFixture(async () => [{ kind: 'audiooutput', label: 'Headphones', deviceId: 'headphones' }], id => {
    calls.push(id);
    return id ? new Promise(resolve => { finishFirst = resolve; }) : Promise.resolve();
  });
  f.set('Headphones');
  await flushPromises();
  f.set(null);
  await flushPromises();
  assert.deepEqual(calls, ['headphones']);
  finishFirst();
  await flushPromises();
  assert.deepEqual(calls, ['headphones', '']);
});

test('failed sink changes are reported and do not block the next selection', async () => {
  const calls = [];
  const f = sinkFixture(async () => [{ kind: 'audiooutput', label: 'Headphones', deviceId: 'headphones' }], async id => {
    calls.push(id);
    if (id) throw new Error('device unavailable');
  });
  f.set('Headphones');
  await flushPromises();
  assert.equal(f.warnings.length, 1);
  f.set(null);
  await flushPromises();
  assert.deepEqual(calls, ['headphones', '']);
});

function fixture() {
  const subscriptions = [], volumes = [], reports = [];
  let attachments = 0, removals = 0;
  const track = {
    attach() { attachments++; return { style: {}, remove() { removals++; } }; },
    detach() {}, setVolume(value) { volumes.push(value); },
  };
  const publication = { kind: 'audio', setSubscribed(value) { subscriptions.push(value); } };
  const participant = { identity: 'alice#video', trackPublications: new Map([['audio', publication]]) };
  const context = vm.createContext({
    inlineScreens: [], detachedScreens: [], viewerTargets: null, attached: {},
    audioTracks: { alice: track },
    room: { remoteParticipants: new Map([['alice', participant]]) },
    navigator: {}, document: { body: { appendChild() {} } },
    window: { postMessage(message) { reports.push(message); } }, console,
    refreshViewerSubscriptions() {},
    baseIdentity: id => id.replace(/#(video|audio)$/, ''),
  });
  vm.runInContext(bridge.slice(bridge.indexOf('  let nativeStreamAudio'), bridge.indexOf('  function requestCameraDimensions')), context);
  vm.runInContext(bridge.slice(bridge.indexOf('  function setInlineScreens'), bridge.indexOf('  function setViewerVisibility')), context);
  return { context, subscriptions, volumes, reports, attachments: () => attachments, removals: () => removals };
}

test('joining without selecting a stream never subscribes or attaches its audio', () => {
  const f = fixture();
  f.context.setStreamVolume(1, 'alice');
  f.context.applyAudioSubscriptions();
  f.context.attachAudio('alice');
  assert.deepEqual(f.subscriptions, [false]);
  assert.deepEqual(f.volumes, [0]);
  assert.equal(f.attachments(), 0);
});

test('stopping a watch detaches audio and stale volume updates remain silent', () => {
  const f = fixture();
  f.context.setInlineScreens(['alice']);
  f.context.setStreamVolume(0.5, 'alice');
  assert.equal(f.attachments(), 1);
  assert.equal(f.volumes.at(-1), 0.5);
  f.context.setInlineScreens([]);
  f.context.setStreamVolume(1, 'alice');
  f.context.attachWatched();
  assert.equal(f.removals(), 1);
  assert.equal(f.subscriptions.at(-1), false);
  assert.equal(f.volumes.at(-1), 0);
  assert.equal(f.attachments(), 1);
});

test('a detached watch keeps fallback audio until the watch stops', () => {
  const f = fixture();
  f.context.setStreamVolume(1, 'alice');
  f.context.setDetachedScreens(['alice']);
  assert.equal(f.attachments(), 1);
  assert.equal(f.subscriptions.at(-1), true);
  f.context.setDetachedScreens([]);
  assert.equal(f.removals(), 1);
  assert.equal(f.subscriptions.at(-1), false);
});

test('a muted watch never attaches audio across replacement or native fallback', () => {
  const f = fixture();
  f.context.setStreamVolume(1, 'alice');
  f.context.setInlineScreens(['alice']);
  assert.equal(f.attachments(), 1);
  f.context.setStreamVolume(0, 'alice#video');
  assert.equal(f.removals(), 1);
  f.context.attachWatched();
  f.context.setDetachedScreens(['alice']);
  f.context.setInlineScreens([]);
  f.context.setNativeStreamAudio(true);
  f.context.setNativeStreamAudio(false);
  f.context.attachAudio('alice#audio');
  assert.equal(f.attachments(), 1);
  assert.equal(f.volumes.at(-1), 0);
  assert.equal(f.subscriptions.at(-1), true);
  assert.equal(f.reports.at(-1).present, true);
  f.context.setStreamVolume(0.5, 'alice');
  assert.equal(f.attachments(), 2);
  assert.equal(f.volumes.at(-1), 0.5);
});

test('watch selection waits for an explicit positive gain before attaching', () => {
  const f = fixture();
  f.context.setInlineScreens(['alice']);
  assert.equal(f.attachments(), 0);
  f.context.setStreamVolume(0, 'alice');
  f.context.attachWatched();
  assert.equal(f.attachments(), 0);
  f.context.setStreamVolume(0.5, 'alice');
  assert.equal(f.attachments(), 1);
});

test('native playback and external viewer rooms never attach WebView audio', () => {
  const f = fixture();
  f.context.setNativeStreamAudio(true);
  f.context.setInlineScreens(['alice']);
  assert.equal(f.attachments(), 0);
  assert.equal(f.subscriptions.at(-1), false);
  f.context.viewerTargets = ['alice'];
  f.context.setNativeStreamAudio(false);
  assert.equal(f.attachments(), 0);
  assert.equal(f.subscriptions.at(-1), false);
});
