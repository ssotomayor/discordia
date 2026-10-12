const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, '../src/features/screenshare.rs'), 'utf8');
const script = source.split('const SCREEN_JS: &str = r#"')[1].split('"#;')[0];
let requests = [], pending = [], stopped = 0;
const container = { innerHTML: '', appendChild(video) { this.video = video; } };
const context = {
  window: { postMessage() {} }, console, setTimeout, clearTimeout, setInterval, clearInterval,
  document: {
    getElementById(id) { return id === 'camera-setup-video' ? container : null; },
    createElement() { return { style: {}, play() { return Promise.resolve(); } }; },
  },
  navigator: { mediaDevices: {
    addEventListener() {},
    getUserMedia(options) {
      requests.push(options);
      return new Promise((resolve, reject) => pending.push({ resolve, reject }));
    },
  } },
};
vm.runInNewContext(script, context);
const bridge = context.window.dxScreen;
function stream(width, height) {
  const track = { stop() { stopped++; }, getSettings() { return { width, height }; } };
  return { getTracks() { return [track]; }, getVideoTracks() { return [track]; } };
}
async function run() {
  const opts = { deviceId: 'external', width: 1920, height: 1080, fps: 60 };
  const cancelled = bridge.previewCamera(opts, 'camera-setup-video');
  assert.equal(requests[0].audio, false);
  assert.equal(requests[0].video.deviceId.exact, 'external');
  assert.equal(requests[0].video.width.ideal, 1920);
  assert.equal(requests[0].video.frameRate.ideal, 60);
  bridge.stopCameraPreview();
  pending.shift().resolve(stream(1920, 1080));
  assert.match((await cancelled).error, /cancelled/);
  assert.equal(stopped, 1, 'Late permission responses must release their tracks');
  assert.equal(container.video, undefined, 'Cancelled previews must not attach');

  const first = bridge.previewCamera(opts, 'camera-setup-video');
  pending.shift().resolve(stream(1280, 720));
  const actual = await first;
  assert.equal(actual.width, 1280, 'Preview must report the actual fallback mode');
  assert.equal(container.video.muted, true);
  assert.equal(container.video.srcObject.getVideoTracks().length, 1);

  const replacement = bridge.previewCamera({ width: 854, height: 480, fps: 15 }, 'camera-setup-video');
  assert.equal(stopped, 2, 'Changing camera quality must release the previous device');
  assert.equal(requests[2].video.deviceId, undefined);
  pending.shift().resolve(stream(854, 480));
  await replacement;
  await bridge.disconnect();
  assert.equal(stopped, 3, 'Call teardown must release the unpublished preview');

  const denied = bridge.previewCamera(opts, 'camera-setup-video');
  pending.shift().reject(new Error('Permission denied'));
  assert.equal((await denied).error, 'Permission denied');

  const older = bridge.previewCamera(opts, 'camera-setup-video');
  const newer = bridge.previewCamera(opts, 'camera-setup-video');
  const oldRequest = pending.shift(), newRequest = pending.shift();
  newRequest.resolve(stream(1920, 1080));
  await newer;
  const currentVideo = container.video;
  oldRequest.resolve(stream(640, 480));
  await older;
  assert.equal(container.video, currentVideo, 'A stale response must not replace the latest preview');
  bridge.stopCameraPreview();
  assert.equal(stopped, 5);
  console.log('Camera preview lifecycle checks passed');
}
run().catch(error => { console.error(error); process.exitCode = 1; });
