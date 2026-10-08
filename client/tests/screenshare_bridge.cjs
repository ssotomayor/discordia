const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
require('./video_lifecycle_bridge.cjs');

const source = fs.readFileSync(path.join(__dirname, '../src/features/screenshare.rs'), 'utf8');
let script = source.split('const SCREEN_JS: &str = r#"')[1].split('"#;')[0];
script = script.replace('return { connect: connect', 'return { testClearTracks: clearRemoteTracks, testAudioTracks: audioTracks, testTracks: tracks, testScreenPubs: screenPubs, testQualityPolicy: qualityPolicy, testSetRoom(r) { room = r; }, testSetRemoteTrack(t) { remoteShareVideoTrack = t; screenStatsEnabled = true; }, testPollRemoteStats: pollRemoteScreenStats, testSetLocalTrack(t) { localShareVideoTrack = t; screenCaptureTrack = { getSettings() { return { width: 1920, height: 1080, frameRate: 60 }; } }; screenStatsEnabled = true; }, testPollLocalStats: pollScreenStats, connect: connect');
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
}, console, setTimeout() {}, clearTimeout() {}, setInterval() { return 1; }, clearInterval() {}, navigator: {} };
vm.runInNewContext(script, context);
const bridge = context.window.dxScreen;
{
  let started = 0;
  const early = { ...context, window: { postMessage() {} }, setInterval() { started++; return 1; } };
  const enable = source.match(/format!\("(window\.__dxfScreenStatsEnabled=.*?)"\)/)[1].replaceAll('{enabled}', 'true');
  vm.runInNewContext(enable, early);
  vm.runInNewContext(script, early);
  assert.equal(started, 1, 'Diagnostics enabled before bridge initialization must start polling');
  early.window.dxScreen.setStatsEnabled(false);
  vm.runInNewContext(script, early);
  assert.equal(started, 1, 'closed Diagnostics must remain disabled on reinitialization');
}
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
bridge.setStreamVolume(0, 'alice#audio');
assert.equal(bridge.testAudioTracks.alice.volume, 0);
assert.equal(bridge.testAudioTracks.bob.volume, 0.8);
bridge.setStreamVolume(0.4, 'alice#video');
assert.equal(bridge.testAudioTracks.alice.volume, 0.4);
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

{
  const publication = (kind, source) => ({ kind, source, subscribed: null,
    setSubscribed(value) { this.subscribed = value; },
  });
  const aliceVideo = publication('video', 'screen_share');
  const bobVideo = publication('video', 'screen_share');
  const aliceCamera = publication('video', 'camera');
  const aliceAudio = publication('audio', 'screen_share_audio');
  const room = { remoteParticipants: new Map([
    ['alice#video', { identity: 'alice#video', trackPublications: new Map([['v', aliceVideo], ['c', aliceCamera], ['a', aliceAudio]]) }],
    ['bob#video', { identity: 'bob#video', trackPublications: new Map([['v', bobVideo]]) }],
  ]) };
  bridge.testSetRoom(room);
  bridge.setInlineScreens(['alice', 'bob']);
  bridge.setDetachedScreens(['alice']);
  assert.equal(aliceVideo.subscribed, false, 'the main window stops receiving a detached video');
  assert.equal(bobVideo.subscribed, true, 'other inline-watched videos stay subscribed');
  assert.equal(aliceCamera.subscribed, null, 'detaching a screen does not interrupt its camera');
  assert.equal(aliceAudio.subscribed, null, 'audio remains in the main window');
  bridge.setDetachedScreens(['alice', 'bob']);
  bridge.setStreamVolume(0.2, 'alice#video');
  bridge.setStreamVolume(0.6, 'bob#video');
  assert.equal(bridge.testAudioTracks.alice.volume, 0.2);
  assert.equal(bridge.testAudioTracks.bob.volume, 0.6);
  bridge.setStreamVolume(0, 'alice');
  assert.equal(bridge.testAudioTracks.alice.volume, 0);
  assert.equal(bridge.testAudioTracks.bob.volume, 0.6);
  bridge.setStreamVolume(0.4, 'alice');
  bridge.setStreamVolume(0.8, 'bob');
  bridge.setDetachedScreens([]);
  assert.equal(aliceVideo.subscribed, true, 'docking restores the inline video subscription');
  bridge.setInlineScreens([]);
  assert.equal(aliceVideo.subscribed, false, 'a screen nobody watches stays paused');
  assert.equal(bobVideo.subscribed, false, 'a screen nobody watches stays paused');
  bridge.setInlineScreens(['alice', 'bob']);
  bridge.setViewerTargets(['bob']);
  assert.equal(aliceVideo.subscribed, false);
  assert.equal(aliceCamera.subscribed, false, 'the external viewer never subscribes to cameras');
  assert.equal(bobVideo.subscribed, true);
  bridge.setViewerTargets(['alice']);
  assert.equal(aliceVideo.subscribed, true);
  assert.equal(bobVideo.subscribed, false, 'removed external streams stop consuming video');
  bridge.setViewerTargets(null);
  bridge.setDetachedScreens([]);
  console.log('External viewers transfer video subscriptions without duplicating camera or audio playback.');
}

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
    ['v', { type: 'inbound-rtp', kind: 'video', id: 'video-rtp', timestamp: 2000, bytesReceived: 1000000, framesPerSecond: 0, jitter: 0.014, framesDecoded: 12, freezeCount: 2, totalFreezesDuration: 1.5, framesDropped: 3, nackCount: 4, pliCount: 5, keyFramesDecoded: 6 }],
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
  assert.equal(diagnostic.timestampMs, 2000);
  assert.equal(diagnostic.bytes, 1000000);
  assert.equal(diagnostic.jitterSeconds, 0.014);
  assert.equal(diagnostic.encodedFps, 0);
  assert.equal('bitrateKbps' in diagnostic, false);
  const previousKey = diagnostic.sampleKey;
  bridge.setStatsEnabled(false);
  bridge.setStatsEnabled(true);
  await bridge.testPollRemoteStats();
  await Promise.resolve();
  await Promise.resolve();
  const newSession = messages.findLast(m => m.sampleKey && m.__dxf === 'screen-stats-in');
  assert.notEqual(newSession.sampleKey, previousKey);
  const outgoing = { async getRTCStatsReport() { return new Map([
    ['v', { id: 'out-video', type: 'outbound-rtp', timestamp: 4000, bytesSent: 500000,
      framesEncoded: 60, framesPerSecond: 60, targetBitrate: 8000000, remoteId: 'r' }],
    ['r', { jitter: 0.002, packetsLost: -1 }],
  ]); } };
  bridge.testSetLocalTrack(outgoing);
  await bridge.testPollLocalStats();
  const sent = messages.at(-1);
  assert.equal(sent.__dxf, 'screen-stats');
  assert.equal(sent.bytes, 500000);
  assert.equal(sent.timestampMs, 4000);
  assert.equal(sent.targetBitrate, 8000000);
  assert.equal(sent.jitterSeconds, 0.002);
  assert.equal(sent.packetsLost, -1);
  assert.notEqual(sent.sampleKey, newSession.sampleKey);
  console.log('Statistics transfer raw counters and isolate tracks and diagnostic sessions.');
  console.log('Freeze diagnostics identify the received track and discard stale asynchronous reports.');
  bridge.testClearTracks();
  assert.equal(bridge.testScreenPubs.size, 0, 'teardown forgets screen publications');
  assert.equal(bridge.testQualityPolicy.size, 0, 'teardown forgets the quality nudge cooldown');
  const statsRoom = {};
  bridge.setStatsEnabled(false);
  bridge.testSetRoom(statsRoom);
  const subscribeStats = bridge.testSubscribe(statsRoom);
  const unsubscribeStats = bridge.testUnsubscribe(statsRoom);
  const remaining = Object.assign(track(), { kind: 'video', sid: 'remaining', async getRTCStatsReport() {
    return new Map([['v', { type: 'inbound-rtp', mediaType: 'video', frameWidth: 2560, frameHeight: 1440, framesPerSecond: 60 }]]);
  } });
  const removed = Object.assign(track(), { kind: 'video', sid: 'removed', async getRTCStatsReport() { return videoReport; } });
  subscribeStats(remaining, { source: 'screen_share' }, { identity: 'remaining#video' });
  subscribeStats(removed, { source: 'screen_share' }, { identity: 'removed#video' });
  assert.equal(bridge.testScreenPubs.has('remaining'), true);
  assert.equal(bridge.testScreenPubs.has('removed'), true);
  unsubscribeStats(removed, { source: 'screen_share' }, { identity: 'removed#video' });
  assert.equal(bridge.testScreenPubs.has('removed'), false, 'an unsubscribed screen drops its publication');
  assert.equal(bridge.testQualityPolicy.has('removed'), false);
  bridge.testSetRemoteTrack(null);
  await bridge.testPollRemoteStats();
  assert.equal(messages.at(-1).trackSid, 'remaining', 'removing the latest track must preserve statistics for another stream');
  assert.equal(messages.at(-1).encodedWidth, 2560);
  assert.equal(messages.at(-1).encodedFps, 60);
  unsubscribeStats(remaining, { source: 'screen_share' }, { identity: 'remaining#video' });
  await bridge.testPollRemoteStats();
  assert.equal(messages.at(-1).active, false);
  const missingStats = Object.assign(track(), { kind: 'video', sid: 'pending', async getRTCStatsReport() { return new Map(); } });
  bridge.setStatsEnabled(false);
  subscribeStats(missingStats, { source: 'screen_share' }, { identity: 'pending#video' });
  bridge.testSetRemoteTrack(missingStats);
  await bridge.testPollRemoteStats();
  assert.equal(messages.at(-1).active, true);
  assert.match(messages.at(-1).error, /receiver statistics/);
  console.log('Diagnostics survive track removal, legacy mediaType reports and missing receiver statistics.');
})().catch(error => { console.error(error); process.exitCode = 1; });
