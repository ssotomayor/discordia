const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

const source = fs.readFileSync(path.join(__dirname, '../src/features/screenshare.rs'), 'utf8');
let script = source.split('const SCREEN_JS: &str = r#"')[1].split('"#;')[0];
script = script.replace('return { connect: connect', 'return { testClearTracks: clearRemoteTracks, testAudioTracks: audioTracks, testTracks: tracks, connect: connect');
const containers = new Map();
function element() {
  return {
    style: {}, attributes: {}, children: [],
    setAttribute(k, v) { this.attributes[k] = v; },
    getAttribute(k) { return this.attributes[k]; },
    removeAttribute(k) { delete this.attributes[k]; },
    appendChild(child) { this.children.push(child); },
    querySelectorAll() { return []; },
    remove() { this.removed = true; },
  };
}
const context = { window: { postMessage() {} }, document: {
  body: element(), getElementById(id) { return containers.get(id); },
}, console, setTimeout() {}, clearTimeout() {}, navigator: {} };
vm.runInNewContext(script, context);
const bridge = context.window.dxScreen;
function track() {
  return { elements: [], detached: [], volume: 0,
    attach() { const el = element(); this.elements.push(el); return el; },
    detach(el) { this.detached.push(el); },
    setVolume(value) { this.volume = value; },
  };
}
for (const id of ['alice', 'bob']) {
  containers.set(`screenshare-viewer-${id}`, element());
  bridge.testAudioTracks[id] = track();
  bridge.testTracks[`${id}|screen`] = track();
  bridge.attach(id, `screenshare-viewer-${id}`, 'screen');
}
bridge.setStreamVolume(0.4, 'alice');
bridge.setStreamVolume(0.8, 'bob');
assert.equal(bridge.testAudioTracks.alice.volume, 0.4);
assert.equal(bridge.testAudioTracks.bob.volume, 0.8);
bridge.attach('bob', 'screenshare-viewer-bob', 'screen');
assert.equal(bridge.testTracks['bob|screen'].detached.length, 1);
assert.equal(bridge.testTracks['alice|screen'].detached.length, 0);
assert.equal(bridge.testAudioTracks.alice.detached.length, 0);
containers.delete('screenshare-viewer-alice');
bridge.detach('screenshare-viewer-alice');
assert.equal(bridge.testAudioTracks.alice.detached.length, 1);
assert.equal(bridge.testTracks['alice|screen'].detached.length, 1);
assert.equal(bridge.testAudioTracks.bob.detached.length, 0);
assert.equal(bridge.testTracks['bob|screen'].detached.length, 1);
bridge.setNativeStreamAudio(true);
assert.equal(bridge.testAudioTracks.bob.detached.length, 1);
bridge.setNativeStreamAudio(false);
assert.equal(bridge.testAudioTracks.bob.elements.length, 2);
assert.equal(bridge.testAudioTracks.bob.volume, 0.8);
console.log('Multiple streams retain independent video, audio, volume and teardown.');

(async function () {
  bridge.testTracks['alice|screen'].getRTCStatsReport = async () => new Map([
    ['a', { type: 'inbound-rtp', kind: 'audio', framesPerSecond: 99 }],
    ['v', { type: 'inbound-rtp', kind: 'video', frameWidth: 1920, frameHeight: 1080, framesPerSecond: 9 }],
  ]);
  bridge.testTracks['bob|screen'].getRTCStatsReport = async () => new Map([
    ['v', { type: 'inbound-rtp', kind: 'video', frameWidth: 1280, frameHeight: 720, framesPerSecond: 30 }],
  ]);
  assert.equal((await bridge.previewStats('alice')).fps, 9);
  assert.equal((await bridge.previewStats('alice')).width, 1920);
  assert.equal((await bridge.previewStats('bob')).fps, 30);
  assert.equal(await bridge.previewStats('missing'), null);
  const bobAudio = bridge.testAudioTracks.bob;
  bridge.testClearTracks();
  assert.equal(bobAudio.detached.length, 2);
  console.log('Preview measurements belong to each stream and preserve actual FPS.');
})().catch(error => { console.error(error); process.exitCode = 1; });
