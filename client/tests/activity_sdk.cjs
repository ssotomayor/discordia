const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

const source = fs.readFileSync(path.join(__dirname, '../src/activity_window.rs'), 'utf8');
const script = (name) => source.split(`const ${name}: &str = r#"`)[1].split('"#;')[0];

(async () => {
  const posted = [];
  const window = { ipc: Object.freeze({ postMessage: (s) => posted.push(JSON.parse(s)) }) };
  vm.runInNewContext(script('SDK_JS'), { window, JSON, Promise, Object, String });

  const user = window.discordia.call('user.get');
  const denied = window.discordia.call('message.send', { content: 'x' });
  assert.deepEqual(posted, [
    { id: 1, method: 'user.get', params: {} },
    { id: 2, method: 'message.send', params: { content: 'x' } },
  ]);
  window.__dxfReply(2, false, 'permission denied');
  window.__dxfReply(1, true, { username: 'alice' });
  assert.deepEqual(await user, { username: 'alice' });
  await assert.rejects(denied, (e) => e === 'permission denied');
  window.__dxfReply(1, true, 'late duplicate is ignored');

  const seen = [];
  const off = window.discordia.on('tick', (d) => seen.push(d));
  window.__dxfEvent('tick', 1);
  off();
  window.__dxfEvent('tick', 2);
  assert.deepEqual(seen, [1]);
  assert.throws(() => { 'use strict'; window.discordia.call = null; Object.defineProperty(window, 'discordia', { value: {} }); });

  function RTCPeerConnection() {}
  const hardened = { RTCPeerConnection, webkitRTCPeerConnection: RTCPeerConnection, Notification() {}, fetch() {} };
  const navigator = { mediaDevices: {}, geolocation: {} };
  hardened.window = hardened;
  hardened.navigator = navigator;
  hardened.Object = Object;
  vm.runInNewContext(script('HARDEN_JS'), hardened);
  assert.equal(hardened.RTCPeerConnection, undefined);
  assert.equal(hardened.webkitRTCPeerConnection, undefined);
  assert.equal(hardened.Notification, undefined);
  assert.equal(navigator.mediaDevices, undefined);
  hardened.RTCPeerConnection = RTCPeerConnection;
  assert.equal(hardened.RTCPeerConnection, undefined, 'cannot be put back');
  console.log('activity sdk: ok');
})().catch((e) => { console.error(e); process.exit(1); });
