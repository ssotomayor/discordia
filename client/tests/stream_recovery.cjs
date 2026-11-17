const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync('client/src/features/screenshare.rs', 'utf8');
const script = source.split('const SCREEN_JS: &str = r#"')[1].split('"#;')[0];
let now = 1000;
const request = { generation: 1 };
const subscriptions = [], retries = [], pending = [];
const publication = { track: { sid: 'screen' }, setSubscribed: enabled => subscriptions.push(enabled) };
const scope = vm.createContext({ connectionRequest: request, room: { state: 'connected' },
  screenWanted: () => true, screenPublication: () => publication, screenRecoveries: new Map(),
  localShareVideoTrack: null, localCameraTrack: null,
  Date: { now: () => now }, setTimeout: fn => pending.push(fn),
  reportRoomProblem() {}, post: (kind, data) => retries.push({ kind, data }), console,
  requestInitialQuality(identity, track) { assert.equal(track, publication.track); },
});
const begin = script.indexOf('  function recoverScreen(');
const end = script.indexOf('  let viewerTargets', begin);
vm.runInContext(script.slice(begin, end), scope);
scope.recoverScreen('user', request);
assert.deepEqual(subscriptions, [false]);
pending.shift()();
assert.deepEqual(subscriptions, [false, true]);
scope.recoverScreen('user', request);
assert.equal(subscriptions.length, 2, 'a continuing freeze cannot cause a resubscribe storm');
for (let attempt = 0; attempt < 2; attempt++) {
  now += 20000; scope.recoverScreen('user', request); pending.shift()();
}
now += 20000;
scope.recoverScreen('user', request);
assert.equal(retries.length, 1, 'three unsuccessful repairs escalate to the video room');
scope.localShareVideoTrack = {};
now += 20000; scope.recoverScreen('user', request);
assert.equal(retries.length, 1, 'receiver recovery cannot end a browser-owned local capture');
scope.screenRecoveries.clear();
scope.localShareVideoTrack = null;
scope.room.state = 'reconnecting';
scope.recoverScreen('user', request);
assert.equal(subscriptions.length, 6, 'SDK reconnection owns transport recovery until it finishes');
scope.room.state = 'connected';
scope.recoverScreen('user', request);
scope.connectionRequest = { generation: 2 };
pending.shift()();
assert.equal(subscriptions.at(-1), false, 'a delayed subscribe must not target a replacement room');
console.log('Post-start video recovery: cooldown, escalation, local capture and generation guards passed.');
