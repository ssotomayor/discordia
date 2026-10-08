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
const context = vm.createContext({
  qualityReports: reports,
  screenPubs,
  screenParticipants: participants,
  qualityPolicy: policy,
  LK: () => ({ VideoQuality: { LOW: 0, MEDIUM: 1, HIGH: 2 },
    ConnectionQuality: { Poor: 'poor', Excellent: 'excellent' } }),
  console: { warn: () => {}, error: () => {}, log: () => {} },
  Date: { now: () => now },
  videoTrackFor: () => ({ sid: 'screen' }),
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
  assert.deepEqual(calls, [1], 'stays pinned while the reduction lasts');

  incoming = primary;
  calls.length = 0;
  recovered(now);
  await context.previewStats('viewer');
  assert.equal(calls.length, 0, 'no probe before the hold');
  now += 30001;
  recovered(now);
  await context.previewStats('viewer');
  assert.deepEqual(calls, [2], 'probes High once the hold elapses');

  calls.length = 0;
  incoming = reduced;
  now += 3000;
  receiver(now - 3000);
  await context.previewStats('viewer');
  assert.deepEqual(calls, [1], 're-pins Medium after a failed probe');

  incoming = primary;
  calls.length = 0;
  recovered(now);
  await context.previewStats('viewer');
  assert.equal(calls.length, 0, 'no probe yet');
  now += 30001;
  recovered(now);
  await context.previewStats('viewer');
  assert.equal(calls.length, 0, 'still inside the doubled hold');
  now += 30000;
  recovered(now);
  await context.previewStats('viewer');
  assert.deepEqual(calls, [2], 'probes again after the doubled hold');
});

test('a Poor participant blocks the High probe until its connection recovers', async () => {
  reports.clear();
  screenPubs.clear();
  policy.clear();
  participants.clear();
  const calls = [];
  screenPubs.set('screen', { setVideoQuality: (quality) => calls.push(quality) });
  const reduced = { width: 1280, height: 720, fps: 30 };
  const primary = { width: 2560, height: 1440, fps: 60 };

  incoming = reduced;
  reports.set('screen', { status: full, at: now, reason: 'receiver', since: now - 3000 });
  await context.previewStats('viewer');
  assert.deepEqual(calls, [1], 'pins Medium first');

  calls.length = 0;
  incoming = primary;
  participants.set('screen', { connectionQuality: 'poor' });
  reports.set('screen', { status: full, at: now, reason: '', since: now });
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
