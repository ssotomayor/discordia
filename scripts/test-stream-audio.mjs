import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../client/src/features/screenshare.rs', import.meta.url), 'utf8');
const bridge = source.split('pub(crate) const SCREEN_JS: &str = r#"')[1].split('"#;')[0];

function fixture() {
  const subscriptions = [], volumes = [];
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
    window: { postMessage() {} }, console,
    refreshViewerSubscriptions() {},
    baseIdentity: id => id.replace(/#(video|audio)$/, ''),
  });
  vm.runInContext(bridge.slice(bridge.indexOf('  let nativeStreamAudio'), bridge.indexOf('  function requestCameraDimensions')), context);
  vm.runInContext(bridge.slice(bridge.indexOf('  function setInlineScreens'), bridge.indexOf('  function setViewerVisibility')), context);
  return { context, subscriptions, volumes, attachments: () => attachments, removals: () => removals };
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
  f.context.setDetachedScreens(['alice']);
  assert.equal(f.attachments(), 1);
  assert.equal(f.subscriptions.at(-1), true);
  f.context.setDetachedScreens([]);
  assert.equal(f.removals(), 1);
  assert.equal(f.subscriptions.at(-1), false);
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
