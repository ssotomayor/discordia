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
const nudgeAt = new Map();
const context = vm.createContext({
  qualityReports: reports,
  screenPubs,
  qualityNudgeAt: nudgeAt,
  LK: () => ({ VideoQuality: { LOW: 0, MEDIUM: 1, HIGH: 2 } }),
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

test('receiver limitation re-requests the high layer after a sustained, rate-limited window', async () => {
  reports.clear();
  screenPubs.clear();
  nudgeAt.clear();
  const calls = [];
  screenPubs.set('screen', { setVideoQuality: (quality) => calls.push(quality) });
  incoming = { width: 1280, height: 720, fps: 30 };
  reports.set('screen', { status: full, at: now, reason: 'receiver', since: now });
  await context.previewStats('viewer');
  assert.equal(calls.length, 0, 'not until the reduction is sustained');
  now += 4001;
  reports.get('screen').at = now;
  await context.previewStats('viewer');
  assert.deepEqual(calls, [1, 2], 'Medium then High re-runs the SFU allocator');
  calls.length = 0;
  now += 5000;
  reports.get('screen').at = now;
  await context.previewStats('viewer');
  assert.equal(calls.length, 0, 'within the 10 s cooldown');
  now += 6000;
  reports.get('screen').at = now;
  await context.previewStats('viewer');
  assert.deepEqual(calls, [1, 2]);

  calls.length = 0;
  reports.set('screen', { status: { ...full, primaryWidth: 1280, primaryHeight: 720, bandwidth: true },
    at: now, reason: 'sender', since: now - 5000 });
  await context.previewStats('viewer');
  assert.equal(calls.length, 0, 'a sender limitation is not the viewer’s to fix');

  nudgeAt.clear();
  reports.set('screen', { status: full, at: now, reason: 'receiver', since: now - 5000 });
  incoming = { width: 2560, height: 1440, fps: 60 };
  await context.previewStats('viewer');
  assert.equal(nudgeAt.has('screen'), false, 'recovery clears the cooldown');
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

test('hidden viewers pause video while detached streams and disabled self preview remain excluded', () => {
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
  visibilityContext.setViewerVisibility(false);
  assert([...subscribed.values()].every((value) => !value));
  visibilityContext.setViewerVisibility(true);
  assert.equal(subscribed.get('other'), true);
  assert.equal(subscribed.get('camera'), true);
  assert.equal(subscribed.get('detached'), false);
  assert.equal(subscribed.get('self'), false);
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
