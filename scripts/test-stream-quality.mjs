import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import vm from 'node:vm';

const source = readFileSync(new URL('../client/src/features/screenshare.rs', import.meta.url), 'utf8');
const bridge = source.split('pub(crate) const SCREEN_JS: &str = r#"')[1].split('"#;')[0];
new vm.Script(bridge);
const start = bridge.indexOf('  function screenQualityReason(');
const end = bridge.indexOf('  async function startShare()', start);
let now = 1000;
let incoming = { width: 1280, height: 720, fps: 30 };
const reports = new Map();
const screenPubs = new Map();
const policy = new Map();
const participants = new Map();
const healthReports = new Map();
const screenTrack = { sid: 'screen' };
const context = vm.createContext({
  qualityReports: reports,
  screenPubs,
  screenParticipants: participants,
  qualityPolicy: policy,
  healthReports,
  viewerVisible: true,
  attached: {},
  LK: () => ({ VideoQuality: { LOW: 0, MEDIUM: 1, HIGH: 2 },
    ConnectionQuality: { Poor: 'poor', Excellent: 'excellent' } }),
  console: { warn: () => {}, error: () => {}, log: () => {} },
  Date: { now: () => now },
  videoTrackFor: () => screenTrack,
  videoStatsReport: async () => new Map([['video', {
    type: 'inbound-rtp', kind: 'video', frameWidth: incoming.width,
    frameHeight: incoming.height, framesPerSecond: incoming.fps,
  }]]),
});
vm.runInContext(bridge.slice(start, end), context);
const full = { width: 2560, height: 1440, primaryWidth: 2560, primaryHeight: 1440,
  captureFps: 60, primaryFps: 60, adaptive: true, bandwidth: false };

test('lower layer indicates receiver connection adaptation', () => {
  assert.equal(context.screenQualityReason(full, incoming), 'receiver');
  assert.equal(context.screenQualityReason(full, { width: 2560, height: 1440 }), '');
  assert.equal(context.screenQualityReason({ ...full, primaryWidth: 0, primaryHeight: 0 }, incoming), 'receiver');
});
test('sender bandwidth limitation is distinguished from CPU and lower source resolution', () => {
  const limited = { ...full, primaryWidth: 1280, primaryHeight: 720 };
  assert.equal(context.screenQualityReason({ ...limited, bandwidth: true }, incoming), 'sender');
  assert.equal(context.screenQualityReason(limited, incoming), '');
  assert.equal(context.screenQualityReason({ ...full, width: 2560, height: 1440 }, { width: 2560, height: 1440 }), '');
  assert.equal(context.screenQualityReason({ ...full, adaptive: false }, incoming), '');
});
test('frame rate loss only labels sender when bandwidth is the reported cause', () => {
  const slow = { ...full, primaryFps: 30 };
  assert.equal(context.screenQualityReason({ ...slow, bandwidth: true }, { width: 2560, height: 1440 }), 'sender');
  assert.equal(context.screenQualityReason(slow, { width: 2560, height: 1440 }), '');
});
test('notice waits for sustained reduction, clears on recovery and expires without status', async () => {
  reports.set('screen', { status: full, at: now, reason: '', since: now });
  assert.equal((await context.previewStats('viewer')).qualityReason, undefined);
  now += 4001;
  assert.equal((await context.previewStats('viewer')).qualityReason, 'receiver');
  incoming = { width: 2560, height: 1440, fps: 60 };
  assert.equal((await context.previewStats('viewer')).qualityReason, undefined);
  incoming = { width: 1280, height: 720, fps: 30 };
  now += 11000;
  assert.equal((await context.previewStats('viewer')).qualityReason, undefined);
});

test('receiver limitation pins the stable layer and only probes High after a backed-off wait', async () => {
  reports.clear();
  screenPubs.clear();
  policy.clear();
  participants.clear();
  const calls = [];
  screenPubs.set('screen', { setVideoQuality: (quality) => calls.push(quality) });

  const reduced = { width: 1280, height: 720, fps: 30 };
  const primary = { width: 2560, height: 1440, fps: 60 };
  const receiver = (since) => reports.set('screen', { status: full, at: now, reason: 'receiver', since });
  const recovered = (since) => reports.set('screen', { status: full, at: now, reason: '', since });

  incoming = reduced;
  receiver(now);
  await context.previewStats('viewer');
  assert.equal(calls.length, 0, 'not until the reduction is sustained');

  now += 3000;
  receiver(now - 3000);
  await context.previewStats('viewer');
  assert.deepEqual(calls, [1], 'pins the stable lower layer, never High');

  calls.length = 0;
  now += 10000;
  receiver(now - 10000);
  await context.previewStats('viewer');
  assert.deepEqual(calls, [], 'polling must not re-pin Medium or postpone the probe');

  calls.length = 0;
  receiver(now - 10000);
  await context.previewStats('viewer');
  assert.equal(calls.length, 0, 'no probe before the hold');
  now += 20001;
  receiver(now - 30001);
  await context.previewStats('viewer');
  assert.deepEqual(calls, [2], 'probes High while still receiving 720p after the original hold');

  calls.length = 0;
  incoming = reduced;
  now += 3000;
  receiver(now - 3000);
  await context.previewStats('viewer');
  assert.deepEqual(calls, [], 'allow time for the requested High layer to arrive');
  now += 9001;
  receiver(now - 12001);
  await context.previewStats('viewer');
  assert.deepEqual(calls, [1], 're-pins Medium after a failed probe');

  calls.length = 0;
  receiver(now - 12001);
  await context.previewStats('viewer');
  assert.equal(calls.length, 0, 'no probe yet');
  now += 30001;
  receiver(now - 42002);
  await context.previewStats('viewer');
  assert.equal(calls.length, 0, 'still inside the doubled hold');
  now += 30000;
  receiver(now - 72002);
  await context.previewStats('viewer');
  assert.deepEqual(calls, [2], 'probes again after the doubled hold');
  incoming = primary;
  now += 1000;
  recovered(now);
  await context.previewStats('viewer');
  now += 12001;
  recovered(now - 12001);
  await context.previewStats('viewer');
  assert.equal(policy.get('screen').fails, 0, 'successful recovery clears the backoff');
  assert.equal(policy.get('screen').probeAt, 0);
});

test('a Poor participant blocks the High probe until its connection recovers', async () => {
  reports.clear();
  screenPubs.clear();
  policy.clear();
  participants.clear();
  const calls = [];
  screenPubs.set('screen', { setVideoQuality: (quality) => calls.push(quality) });
  const reduced = { width: 1280, height: 720, fps: 30 };

  incoming = reduced;
  reports.set('screen', { status: full, at: now, reason: 'receiver', since: now - 3000 });
  await context.previewStats('viewer');
  assert.deepEqual(calls, [1], 'pins Medium first');

  calls.length = 0;
  incoming = reduced;
  participants.set('screen', { connectionQuality: 'poor' });
  reports.set('screen', { status: full, at: now, reason: 'receiver', since: now - 3000 });
  now += 30001;
  reports.get('screen').at = now;
  await context.previewStats('viewer');
  assert.equal(calls.length, 0, 'no probe while the participant is Poor');

  participants.set('screen', { connectionQuality: 'excellent' });
  now += 1000;
  reports.get('screen').at = now;
  await context.previewStats('viewer');
  assert.deepEqual(calls, [2], 'probes High once the connection is not Poor');
});

test('quality reports must belong to the sending participant and preserve the warning timer', () => {
  let handler;
  const room = { on: (event, callback) => { handler = callback; } };
  const dataContext = vm.createContext({ room, thisRoom: room,
    lk: { RoomEvent: { DataReceived: 'data' } }, TextDecoder, qualityReports: reports,
    Date: { now: () => now },
  });
  const start = bridge.indexOf('    thisRoom.on(lk.RoomEvent.DataReceived,');
  const end = bridge.indexOf('    thisRoom.on(lk.RoomEvent.EncryptionError,', start);
  vm.runInContext(bridge.slice(start, end), dataContext);
  reports.clear();
  const sender = { trackPublications: new Map([['screen', {}]]) };
  const packet = new TextEncoder().encode(JSON.stringify({ ...full, sid: 'screen' }));
  handler(packet, { trackPublications: new Map() }, null, 'discordia.screen-quality.v1');
  assert.equal(reports.size, 0);
  handler(packet, sender, null, 'unrelated');
  assert.equal(reports.size, 0);
  handler(packet, sender, null, 'discordia.screen-quality.v1');
  assert.equal(reports.size, 1);
  handler(new TextEncoder().encode(JSON.stringify({ ...full, sid: 'screen', cpu: 'true' })), sender, null, 'discordia.screen-quality.v1');
  assert.equal(reports.get('screen').status.cpu, undefined, 'malformed optional health fields cannot replace a valid report');
  reports.get('screen').reason = 'receiver';
  const since = reports.get('screen').since;
  now += 3000;
  handler(packet, sender, null, 'discordia.screen-quality.v1');
  assert.equal(reports.get('screen').since, since);
  assert.equal(reports.get('screen').reason, 'receiver');
  now += 11000;
  handler(packet, sender, null, 'discordia.screen-quality.v1');
  assert.equal(reports.get('screen').reason, '');
  assert.equal(reports.get('screen').since, now);
  assert.equal(reports.get('screen').at, now);
});

test('hidden viewers pause video while only inline-watched screens subscribe', () => {
  const subscribed = new Map();
  const participant = (identity, source) => ({ identity,
    trackPublications: new Map([[identity, { kind: 'video', source,
      setSubscribed: (enabled) => subscribed.set(identity, enabled) }]]),
  });
  const room = { remoteParticipants: new Map([
    ['other', participant('other', 'screen')], ['detached', participant('detached', 'screen')],
    ['self', participant('self', 'screen')], ['camera', participant('camera', 'camera')],
  ]) };
  const visibilityContext = vm.createContext({ room, selfPreviewIdentity: 'self',
    healthReports,
    refreshAudioWatches: () => {},
    selfPreviewEnabled: false, applySelfPreviewSubscription: () => {},
    baseIdentity: (identity) => identity, kindOf: (pub) => pub.source,
  });
  const start = bridge.indexOf('  let viewerTargets = null;');
  const end = bridge.indexOf('  let nativeStreamAudio', start);
  vm.runInContext(bridge.slice(start, end), visibilityContext);
  visibilityContext.setDetachedScreens(['detached']);
  visibilityContext.setInlineScreens(['other']);
  visibilityContext.setViewerVisibility(false);
  assert([...subscribed.values()].every((value) => !value));
  visibilityContext.setViewerVisibility(true);
  assert.equal(subscribed.get('other'), true, 'an inline-watched screen subscribes');
  assert.equal(subscribed.get('camera'), true);
  assert.equal(subscribed.get('detached'), false, 'the detached viewer owns that screen');
  assert.ok(!subscribed.get('self'), 'the self preview is not the plain subscription');

  subscribed.clear();
  visibilityContext.setInlineScreens([]);
  assert.equal(subscribed.get('other'), false, 'a screen nobody watches stays paused');

  visibilityContext.setViewerTargets(['other']);
  assert.equal(subscribed.get('camera'), false);
  visibilityContext.setViewerTargets(null);
  assert.equal(subscribed.get('camera'), true);
  subscribed.set('camera', false);
  visibilityContext.setDetachedScreens([]);
  assert.equal(subscribed.get('camera'), false, 'docking a screen preserves camera subscriptions');
});

test('camera requests follow their largest rendered view without lowering screen quality', () => {
  let requested;
  const camera = { sid: 'camera' };
  const room = { remoteParticipants: new Map([['person', {
    trackPublications: new Map([['camera', { trackSid: 'camera', setVideoDimensions: (dimensions) => { requested = dimensions; } }],
      ['screen', { trackSid: 'screen', setVideoDimensions: () => assert.fail('screen must request full quality') }]]),
  }]]) };
  const attached = {
    small: { kind: 'camera', track: camera, el: { getBoundingClientRect: () => ({ width: 160, height: 90 }) } },
    large: { kind: 'camera', track: camera, el: { getBoundingClientRect: () => ({ width: 640, height: 360 }) } },
  };
  const cameraContext = vm.createContext({ room, attached, window: { devicePixelRatio: 1 } });
  const start = bridge.indexOf('  function requestCameraDimensions(');
  const end = bridge.indexOf('  function attachInto(', start);
  vm.runInContext(bridge.slice(start, end), cameraContext);
  cameraContext.requestCameraDimensions(camera);
  assert.equal(requested.width, 640);
  assert.equal(requested.height, 360);
});

function healthSequence(status = full) {
  healthReports.clear();
  const key = {};
  let sample = { id: 'inbound', timestamp: 0, frameWidth: 2560, frameHeight: 1440,
    framesPerSecond: 60, packetsReceived: 0, packetsLost: 0, framesDecoded: 0,
    framesDropped: 0, totalDecodeTime: 0, freezeCount: 0, totalFreezesDuration: 0 };
  context.inboundHealth(key, sample, status, 0);
  return (changes = {}, report = status) => {
    sample = { ...sample, timestamp: sample.timestamp + 1000,
      packetsReceived: sample.packetsReceived + 1000,
      framesDecoded: sample.framesDecoded + 60,
      totalDecodeTime: sample.totalDecodeTime + 0.06, ...changes };
    return context.inboundHealth(key, sample, report, sample.timestamp);
  };
}

test('56 FPS, high jitter, low bitrate and an isolated lost packet do not blame the network', () => {
  const tick = healthSequence();
  for (let i = 0; i < 12; i++) assert.equal(tick({ framesPerSecond: 56, jitter: 0.15, packetsLost: 1, bytesReceived: 3000 }), '');
});

test('sustained loss with interrupted playback warns, then requires stable recovery', () => {
  const tick = healthSequence();
  for (let i = 1; i <= 3; i++) assert.equal(tick({ packetsLost: i * 60, framesPerSecond: 30 }), '');
  assert.equal(tick({ packetsLost: 240, framesPerSecond: 30 }), 'receiver-network');
  for (let i = 0; i < 3; i++) assert.equal(tick({ framesPerSecond: 60 }), 'receiver-network');
  assert.equal(tick({ framesPerSecond: 60 }), '');
});

test('decoder attribution needs processing cost, playback symptoms and measured low loss', () => {
  const tick = healthSequence();
  let result;
  for (let i = 1; i <= 5; i++) result = tick({ framesPerSecond: 30, totalDecodeTime: i * 1.2 });
  assert.equal(result, 'receiver-device');
  const unknown = healthSequence();
  for (let i = 1; i <= 5; i++) result = unknown({ framesPerSecond: 30, totalDecodeTime: i * 1.2, packetsLost: undefined });
  assert.equal(result, '', 'missing loss counters are not proof of a healthy network');
});

test('simulcast at 720p/30 does not count as a 1440p/60 playback failure', () => {
  const tick = healthSequence();
  for (let i = 1; i <= 6; i++) assert.equal(tick({ frameWidth: 1280, frameHeight: 720,
    framesPerSecond: 30, totalDecodeTime: i * 1.5 }), '');
});

test('display drops with healthy decoding and low loss identify the receiving device', () => {
  const tick = healthSequence();
  let result;
  for (let i = 1; i <= 6; i++) result = tick({ playbackId: 'visible-video', renderFrames: i * 60, renderDropped: i * 10 });
  assert.equal(result, 'receiver-device');
  assert.equal(tick({ playbackId: undefined, renderFrames: undefined, renderDropped: undefined }), '');
});

test('repeated freezes without attribution get a generic warning; static content does not', () => {
  const tick = healthSequence();
  let result;
  for (let i = 1; i <= 5; i++) result = tick({ freezeCount: i });
  assert.equal(result, 'unstable');
  const still = healthSequence();
  for (let i = 1; i <= 6; i++) assert.equal(still({ framesPerSecond: 1, framesDecoded: i, packetsReceived: i * 2 }), '');
});

test('fresh sender reports distinguish connection and device limitations', () => {
  for (const [status, expected] of [[{ ...full, bandwidth: true }, 'sender-network'],
    [{ ...full, cpu: true }, 'sender-device'], [{ ...full, captureLimited: true }, 'sender-device']]) {
    const tick = healthSequence(status);
    let result;
    for (let i = 0; i < 5; i++) result = tick();
    assert.equal(result, expected);
    for (let i = 0; i < 4; i++) result = tick({}, null);
    assert.equal(result, '', 'expired metadata cannot keep attributing a sender limitation');
  }
});

test('local sender notices debounce, recover, and reset across sessions', async () => {
  healthReports.clear();
  const limited = { id: 'share-1', cpu: true };
  now = 0;
  assert.equal(await context.senderHealth(limited), '');
  now = 4000;
  assert.equal(await context.senderHealth(limited), '', 'a polling gap restarts observation');
  for (now = 5000; now <= 8000; now += 1000) await context.senderHealth(limited);
  assert.equal(await context.senderHealth(limited), 'local-device');
  assert.equal(await context.senderHealth({ id: 'share-2', bandwidth: true }), '');
  assert.equal(await context.senderHealth({ active: false }), '');
  assert.equal(healthReports.size, 0);
});

test('track replacement, counter reset and a hidden interval cannot inherit a playback warning', () => {
  const tick = healthSequence();
  for (let i = 1; i <= 5; i++) tick({ freezeCount: i });
  assert.equal(tick({ id: 'replacement', timestamp: 0, freezeCount: 0 }), '');
  const reset = healthSequence();
  for (let i = 1; i <= 5; i++) reset({ freezeCount: i });
  assert.equal(reset({ framesDecoded: 0, freezeCount: 0 }), '', 'counter rollback begins a fresh observation');
  const key = {};
  context.inboundHealth(key, { id: 'track', timestamp: 1000 }, full, 1000);
  const state = healthReports.get(key);
  state.shown = 'receiver-network';
  assert.equal(context.inboundHealth(key, { id: 'track', timestamp: 5000 }, full, 5000), '');
});

test('browser sender shares authenticated track status without enabling Diagnostics and ignores a replaced room', async () => {
  const sent = [];
  const localTrack = { sid: 'local-screen' };
  const localRoom = { localParticipant: { publishData: async (bytes, options) => sent.push({ status: JSON.parse(new TextDecoder().decode(bytes)), options }) } };
  const senderContext = vm.createContext({ healthReports: new Map(), localShareVideoTrack: localTrack,
    room: localRoom, screenCaptureTrack: { getSettings: () => ({ width: 1920, height: 1080, frameRate: 60 }) },
    videoStatsReport: async () => new Map([['out', { type: 'outbound-rtp', kind: 'video',
      frameWidth: 1920, frameHeight: 1080, framesPerSecond: 30, qualityLimitationReason: 'cpu' }]]),
    statsSampleKey: () => 'local-sample', Date: { now: () => now }, TextEncoder, console });
  vm.runInContext(bridge.slice(start, end), senderContext);
  now = 1000;
  assert.equal(await senderContext.senderHealth(null), '');
  assert.equal(sent.length, 1);
  assert.equal(sent[0].status.cpu, true);
  assert.equal(sent[0].status.sid, localTrack.sid);
  assert.equal(sent[0].options.topic, 'discordia.screen-quality.v1');
  now = 2000;
  await senderContext.senderHealth(null);
  assert.equal(sent.length, 1, 'status reports are paced independently of the polling frequency');
  now = 4000;
  await senderContext.senderHealth(null);
  assert.equal(sent.length, 2);
  senderContext.videoStatsReport = async () => { senderContext.room = {}; return new Map(); };
  assert.equal(await senderContext.senderHealth(null), '');
  assert.equal(sent.length, 2, 'a report awaited in an old room cannot publish into its replacement');
});
