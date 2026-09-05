// Appended to livekit-client.e2ee.worker.js. A CryptoKey posted between the
// page and this worker is a structured clone, and WebKit wraps it with a
// master key kept in the macOS keychain — every rebuilt binary then prompts
// for the login password. So the key crosses as text and is imported here,
// and a ratchet result goes back without the CryptoKey it carries.
(function () {
  var inner = self.onmessage;
  var send = self.postMessage.bind(self);

  function withoutKeys(v) {
    if (typeof CryptoKey !== 'undefined' && v instanceof CryptoKey) return undefined;
    if (!v || typeof v !== 'object' || ArrayBuffer.isView(v) || v instanceof ArrayBuffer) return v;
    var out = {};
    for (var k in v) {
      var x = withoutKeys(v[k]);
      if (x !== undefined) out[k] = x;
    }
    return out;
  }

  self.postMessage = function (msg, transfer) {
    if (msg && msg.kind === 'ratchetKey' && msg.data) {
      msg = { kind: 'ratchetKey', data: withoutKeys(msg.data) };
    }
    return transfer === undefined ? send(msg) : send(msg, transfer);
  };

  // Messages are handled in arrival order, as they were before the shim: the
  // key import is asynchronous, and an `enable` that overtook it would run
  // the cryptors keyless for a moment.
  var chain = Promise.resolve();
  function inOrder(step) {
    var next = chain.then(step, step);
    chain = next.then(noop, noop);
    return next;
  }
  function noop() {}

  self.onmessage = function (ev) {
    var m = ev && ev.data;
    if (!m || m.kind !== 'setKeyRaw') {
      return inOrder(function () {
        return inner(ev);
      });
    }
    var d = m.data || {};
    // PBKDF2 over the UTF-8 bytes: what ExternalE2EEKeyProvider.setKey(string)
    // does, and what libwebrtc derives from the same bytes on the native side.
    return inOrder(function () {
      return crypto.subtle
        .importKey('raw', new TextEncoder().encode(String(d.keyString)), { name: 'PBKDF2' }, false, [
          'deriveBits',
          'deriveKey',
        ])
        .then(function (key) {
          return inner({
            data: {
              kind: 'setKey',
              data: {
                participantIdentity: d.participantIdentity,
                isPublisher: !!d.isPublisher,
                key: key,
                keyIndex: d.keyIndex,
                // The worker's own default; false would skip the key-status
                // reset that lets a rekey revive a receiver that gave up.
                updateCurrentKeyIndex: d.updateCurrentKeyIndex !== false,
              },
            },
          });
        })
        .catch(function (e) {
          send({ kind: 'error', data: { error: new Error('setKeyRaw: ' + String((e && e.message) || e)) } });
        });
    });
  };
})();
