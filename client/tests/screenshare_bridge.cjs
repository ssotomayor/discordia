const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

const source = fs.readFileSync(path.join(__dirname, '../src/features/screenshare.rs'), 'utf8');
let script = source.split('const SCREEN_JS: &str = r#"')[1].split('"#;')[0];
script = script.replace('return { connect: connect', 'return { testClearTracks: clearRemoteTracks, testAudioTracks: audioTracks, testTracks: tracks, testSetRoom(r) { room = r; }, testSetRemoteTrack(t) { remoteShareVideoTrack = t; screenStatsEnabled = true; }, testPollRemoteStats: pollRemoteScreenStats, connect: connect');
const unsubscribe = script.match(/thisRoom\.on\(lk\.RoomEvent\.TrackUnsubscribed, (function \(track, pub, participant\) \{[\s\S]*?\n    \})\);/)[1];
const subscribe = script.match(/thisRoom\.on\(lk\.RoomEvent\.TrackSubscribed, (function \(track, pub, participant\) \{[\s\S]*?\n    \})\);/)[1];
script = script.replace('return { testClearTracks:', 'return { testSubscribe: function(thisRoom) { return ' + subscribe + '; }, testUnsubscribe: function(thisRoom) { return ' + unsubscribe + '; }, testClearTracks:');
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
const messages = [];
const context = { window: { postMessage(message) { messages.push(message); } }, document: {
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
bridge.setStreamVolume(0.25, 'alice#video');
assert.equal(bridge.testAudioTracks.alice.volume, 0.25);
bridge.setStreamVolume(0.4, 'alice');
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

const activeRoom = {};
bridge.testSetRoom(activeRoom);
const unsubscribeActive = bridge.testUnsubscribe(activeRoom);
const currentVideo = Object.assign(track(), { kind: 'video' });
const staleVideo = Object.assign(track(), { kind: 'video' });
let currentAudio = Object.assign(track(), { kind: 'audio' });
const staleAudio = Object.assign(track(), { kind: 'audio' });
containers.set('screenshare-viewer-carol', element());
bridge.testTracks['carol#video|screen'] = currentVideo;
bridge.testAudioTracks.carol = currentAudio;
bridge.attach('carol', 'screenshare-viewer-carol', 'screen');
bridge.testSubscribe({})(staleVideo, { source: 'screen_share' }, { identity: 'carol#video' });
assert.equal(bridge.testTracks['carol#video|screen'], currentVideo, 'an old room cannot replace active tracks');
const replacementAudio = Object.assign(track(), { kind: 'audio' });
bridge.testSubscribe(activeRoom)(replacementAudio, {}, { identity: 'carol#video' });
assert.equal(currentAudio.detached.length, 1);
assert.equal(replacementAudio.elements.length, 1, 'replacement audio attaches to the viewer');
currentAudio = replacementAudio;
unsubscribeActive(staleVideo, { source: 'screen_share' }, { identity: 'carol#video' });
assert.equal(bridge.testTracks['carol#video|screen'], currentVideo, 'a late unsubscribe must preserve the replacement video');
unsubscribeActive(staleAudio, {}, { identity: 'carol#video' });
assert.equal(bridge.testAudioTracks.carol, currentAudio, 'a late unsubscribe must preserve replacement audio');
bridge.testUnsubscribe({})(currentVideo, { source: 'screen_share' }, { identity: 'carol#video' });
assert.equal(bridge.testTracks['carol#video|screen'], currentVideo, 'an old room cannot remove tracks belonging to the active room');
unsubscribeActive(currentVideo, { source: 'screen_share' }, { identity: 'carol#video' });
assert.equal(bridge.testTracks['carol#video|screen'], undefined, 'the current track still detaches normally');
unsubscribeActive(currentAudio, {}, { identity: 'carol#video' });
assert.equal(bridge.testAudioTracks.carol, undefined);
console.log('Late unsubscribe events preserve replacement tracks and ignore old rooms.');


containers.set('camera-self', element());
containers.set('screen-self', element());
const nativeCamera = bridge.testTracks['self#video|camera'] = track();
const nativeScreen = bridge.testTracks['self#video|screen'] = track();
bridge.attach('self', 'camera-self', 'camera');
bridge.attach('self', 'screen-self', 'screen');
assert.equal(nativeCamera.elements.length, 1);
assert.equal(nativeCamera.elements[0].style.transform, 'scaleX(-1)');
assert.equal(nativeScreen.elements.length, 1);
bridge.detach('camera-self');
assert.equal(nativeCamera.detached.length, 1);
assert.equal(nativeScreen.detached.length, 0);
bridge.attach('self', 'camera-self', 'camera');
bridge.detach('screen-self');
assert.equal(nativeCamera.detached.length, 1);
assert.equal(nativeScreen.detached.length, 1);
console.log('Native camera and screen share one identity and detach independently.');

(async function () {
  context.window.testTime = vm.runInNewContext('Date.now()', context);
  vm.runInNewContext('Date.now = () => window.testTime', context);
  let samples = 0;
  bridge.testTracks['alice|screen'].getRTCStatsReport = async () => {
    samples++;
    return new Map([
    ['a', { type: 'inbound-rtp', kind: 'audio', framesPerSecond: 99 }],
    ['v', { type: 'inbound-rtp', kind: 'video', frameWidth: 1920, frameHeight: 1080, framesPerSecond: 9 }],
    ]);
  };
  bridge.testTracks['bob|screen'].getRTCStatsReport = async () => new Map([
    ['v', { type: 'inbound-rtp', kind: 'video', frameWidth: 1280, frameHeight: 720, framesPerSecond: 30 }],
  ]);
  assert.equal((await bridge.previewStats('alice')).fps, 9);
  assert.equal((await bridge.previewStats('alice')).width, 1920);
  assert.equal(samples, 1, 'concurrent UI and Diagnostics must reuse recent measurements');
  context.window.testTime += 600;
  await Promise.all([bridge.previewStats('alice'), bridge.previewStats('alice')]);
  assert.equal(samples, 2, 'expired samples should refresh once for concurrent requests');
  assert.equal((await bridge.previewStats('bob')).fps, 30);
  assert.equal(await bridge.previewStats('missing'), null);
  const bobAudio = bridge.testAudioTracks.bob;
  bridge.testClearTracks();
  assert.equal(bobAudio.detached.length, 2);
  console.log('Preview measurements belong to each stream and preserve actual FPS.');

  const selfScreen = track();
  bridge.testTracks['self#video|screen'] = selfScreen;
  containers.set('screenshare-self', element());
  const subscriptions = [];
  const ownPublication = { kind: 'video', source: 'screen_share', setSubscribed(value) { subscriptions.push(value); } };
  const untouchedPublication = { kind: 'video', source: 'camera', setSubscribed() { assert.fail('camera subscription changed'); } };
  const otherPublication = { kind: 'video', source: 'screen_share', setSubscribed() { assert.fail('other viewer subscription changed'); } };
  bridge.testSetRoom({ remoteParticipants: new Map([
    ['self#video', { identity: 'self#video', trackPublications: new Map([['screen', ownPublication], ['camera', untouchedPublication]]) }],
    ['bob#video', { identity: 'bob#video', trackPublications: new Map([['screen', otherPublication]]) }],
  ]) });
  bridge.setSelfPreview('self', true);
  assert.equal(selfScreen.elements.length, 1);
  bridge.setSelfPreview('self', false);
  assert.equal(selfScreen.detached.length, 1);
  bridge.attach('self', 'screenshare-self', 'screen');
  assert.equal(selfScreen.elements.length, 1);
  bridge.setSelfPreview('self', true);
  assert.equal(selfScreen.elements.length, 2);
  assert.deepEqual(subscriptions, [true, false, true]);
  console.log('Background preview unsubscribes only the native self screen and resumes independently.');

  let finishOldReport;
  const oldTrack = { sid: 'old', getRTCStatsReport() { return new Promise(resolve => { finishOldReport = resolve; }); } };
  const videoReport = new Map([
    ['v', { type: 'inbound-rtp', kind: 'video', framesDecoded: 12, freezeCount: 2, totalFreezesDuration: 1.5, framesDropped: 3, nackCount: 4, pliCount: 5, keyFramesDecoded: 6 }],
  ]);
  const newTrack = { sid: 'new', async getRTCStatsReport() { return videoReport; } };
  bridge.testSetRemoteTrack(oldTrack);
  const pending = bridge.testPollRemoteStats();
  await Promise.resolve();
  bridge.testSetRemoteTrack(newTrack);
  const before = messages.length;
  finishOldReport(videoReport);
  await pending;
  assert.equal(messages.length, before);
  await bridge.testPollRemoteStats();
  const diagnostic = messages.at(-1);
  assert.equal(diagnostic.trackSid, 'new');
  assert.equal(diagnostic.freezeCount, 2);
  assert.equal(diagnostic.freezeDurationSeconds, 1.5);
  assert.equal(diagnostic.framesDropped, 3);
  assert.equal(diagnostic.nackCount, 4);
  assert.equal(diagnostic.pliCount, 5);
  assert.equal(diagnostic.keyFramesDecoded, 6);
  console.log('Freeze diagnostics identify the received track and discard stale asynchronous reports.');
})().catch(error => { console.error(error); process.exitCode = 1; });
