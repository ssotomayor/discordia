const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const { webcrypto } = require('node:crypto');
const { ReadableStream, WritableStream, TransformStream } = require('node:stream/web');

const assets = path.join(__dirname, '../assets');
const sdk = require(path.join(assets, 'livekit-client.umd.js'));
assert.equal(sdk.version, '2.22.4');
assert.equal(typeof sdk.Room, 'function');
assert.equal(typeof sdk.ExternalE2EEKeyProvider, 'function');
assert.equal(sdk.Track.Source.ScreenShare, 'screen_share');
assert.equal(sdk.Track.Source.ScreenShareAudio, 'screen_share_audio');
const shim = fs.readFileSync(path.join(assets, 'e2ee-worker-shim.js'), 'utf8');
const current = fs.readFileSync(path.join(assets, 'livekit-client.e2ee.worker.js'), 'utf8');

class AudioFrame {
  constructor(bytes) { this.data = Uint8Array.from(bytes).buffer; this.timestamp = 123456; }
  getMetadata() { return { synchronizationSource: 1234 }; }
}

class VideoFrame extends AudioFrame {
  constructor(bytes) { super(bytes); this.type = 'key'; }
  getMetadata() { return { synchronizationSource: 4321, payloadType: 102, mimeType: 'video/H264' }; }
}

function containsKey(value) {
  if (value instanceof CryptoKey) return true;
  return value && typeof value === 'object' && Object.values(value).some(containsKey);
}

function worker(source) {
  const pending = [];
  const messages = [];
  const context = {
    crypto: webcrypto, CryptoKey, TextEncoder, TextDecoder,
    Uint8Array, Uint32Array, ArrayBuffer, DataView,
    ReadableStream, WritableStream, TransformStream,
    RTCEncodedAudioFrame: AudioFrame, RTCEncodedVideoFrame: VideoFrame,
    console, setTimeout, clearTimeout,
    postMessage(message) {
      assert.equal(Boolean(containsKey(message)), false, 'keys must remain inside the worker');
      messages.push(message);
      const waiter = pending.findIndex(item => item.kind === message.kind);
      if (waiter >= 0) pending.splice(waiter, 1)[0].resolve(message);
    },
  };
  context.self = context;
  vm.createContext(context);
  vm.runInContext(source + '\n' + shim, context);
  return {
    send(kind, data) { context.onmessage({ data: { kind, data } }); },
    wait(kind) {
      return new Promise((resolve, reject) => {
        const timeout = setTimeout(() => reject(new Error(`worker did not emit ${kind}`)), 3000);
        pending.push({ kind, resolve(message) { clearTimeout(timeout); resolve(message); } });
      });
    },
    messages,
  };
}

async function ready(instance, sharedKey) {
  const initialized = instance.wait('initAck');
  instance.send('init', { loglevel: 'silent', keyProviderOptions: {
    sharedKey, ratchetSalt: 'LKFrameEncryptionKey', ratchetWindowSize: 8,
    failureTolerance: 10, keyringSize: 16, keySize: 128,
  } });
  await initialized;
  const enabled = instance.wait('enable');
  instance.send('setKeyRaw', { keyString: 'worker-interoperability-test', keyIndex: 0, participantIdentity: 'participant' });
  instance.send('enable', { enabled: true, participantIdentity: 'participant' });
  await enabled;
}

async function transform(instance, operation, frame) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error(`${operation} did not produce a frame`)), 3000);
    instance.send(operation, {
      participantIdentity: 'participant', trackId: frame.type ? 'video' : 'audio',
      codec: frame.type ? 'h264' : undefined,
      readableStream: new ReadableStream({ start(controller) { controller.enqueue(frame); controller.close(); } }),
      writableStream: new WritableStream({ write(result) { clearTimeout(timeout); resolve(result); } }),
    });
  });
}

async function roundtrip(senderSource, receiverSource, sharedKey = true) {
  const sender = worker(senderSource), receiver = worker(receiverSource);
  await Promise.all([ready(sender, sharedKey), ready(receiver, sharedKey)]);
  for (const Frame of [AudioFrame, VideoFrame]) {
    const payload = Uint8Array.from({ length: 80 }, (_, index) => index + 16);
    const plain = Frame === VideoFrame ? Uint8Array.from([0, 0, 0, 1, 0x65, 0xb8, ...payload]) : payload;
    const encrypted = await transform(sender, 'encode', new Frame(plain));
    assert.notDeepEqual(new Uint8Array(encrypted.data), plain, 'enabled encryption must change the payload');
    const decoded = await transform(receiver, 'decode', encrypted);
    assert.deepEqual(new Uint8Array(decoded.data), plain);
  }
  if (!sharedKey) {
    const ratcheted = sender.wait('ratchetKey');
    sender.send('ratchetRequest', { participantIdentity: 'participant', keyIndex: 0 });
    await ratcheted;
  }
  assert.equal(sender.messages.some(message => message.kind === 'error'), false);
  assert.equal(receiver.messages.some(message => message.kind === 'error'), false);
}

(async () => {
  await roundtrip(current, current);
  await roundtrip(current, current, false);
  if (process.env.LIVEKIT_REFERENCE_WORKER) {
    const reference = fs.readFileSync(process.env.LIVEKIT_REFERENCE_WORKER, 'utf8');
    await roundtrip(current, reference);
    await roundtrip(reference, current);
  }
  console.log('Real encryption workers preserve media, key ordering and key isolation.');
})().catch(error => { console.error(error); process.exitCode = 1; });
