# Activity audio and Music Lab

The optional Rust music service, installed bundle, queue, provider probes and
native publisher live in [RecknockT/discordia-music](https://github.com/RecknockT/discordia-music).
Its README covers running, installation and current limitations. Discordia
does not install or start it by default; listeners do not run the service.

| Discordia boundary | Location / behavior |
|---|---|
| Consent | `ActivityCapability::Audio` requires `Instance`; adding audio requires renewed approval |
| Authority | Gateway `activity-audio` build feature off by default; current participant/app/channel access rechecked after signing |
| Publisher | Instance app identity only; audio publication, no subscriptions or data publication |
| Listener | Current player/spectator with app access; private per-connection reply, no publication |
| Admission | Dedicated activity room and 60-second token; shared placement/external media minters rejected |
| Key envelope | `nostr::activity_audio`: NIP-44 binds publisher, recipient, instance/channel/epoch, request and publication |
| Trusted receiver | `activity_audio.rs`, `assets/activity-audio.js`; joins only after matching grant and encrypted key, subscribes only to that app's music audio |
| Isolation | No tokens/keys delivered to activity pages; reserved `__dxaudio` frames terminate in the host |
| Lifecycle | Participant/role changes stop old reception and request fresh credentials; exit, access removal and server disconnect stop it |
| Audio | Separate encryption worker, one attached track, volume control; no microphone capture or voice keys |

An Audio-consented installed activity may call `instance.audio.join`,
`instance.audio.leave` and `instance.audio.volume` (`volume`: 0–1). The host
reports `instance.audio` statuses, including connecting, playing, reconnecting,
waiting for audio, playback blocked, credential timeout, permission denial and
connection/encryption failure. No raw SDK errors or credentials reach the page.

After authorizing listener admission, the gateway sends a targeted `__dxaudio: key_request` with request UUID
and membership epoch. The service must reply privately with `__dxaudio: key`,
`context` and NIP-44 `ciphertext`. The host validates that context and decrypts
with the account identity; it never accepts a plaintext page-supplied key.

Music Lab metadata mode requests only `Instance`. Its opt-in `playback` mode
connects local files, publication and key exchange, with separate default identity
and explicit `Audio` approval. All players control play/pause/next; spectators
listen and control their own volume. The desktop client feature `activity-audio`
also enables the embedded host broker; both host variants default off. End-to-end provider
playback and mobile activity windows are not supported. A deterministic epoch
or expired admission token alone does not revoke an existing listener: the
service must stop the old publication and rotate its key on membership changes.

```powershell
cargo test -p discordia --lib activity_audio::tests
cargo test -p discordia-nostr --test activity_audio
cargo test -p discordia-server --no-default-features --features activity-audio --test activity_audio
cargo test -p discordia-server --no-default-features --test activity_audio_disabled
node --test client/tests/activity_audio.cjs
node client/tests/e2ee_worker.cjs
```

Receiver tests cover out-of-order credentials, stale requests, sender/scope
checks, expiry, replacement races, blocked playback, worker errors and cleanup.
Real-worker tests cover UTF-8 passphrase and raw-byte key paths without moving
CryptoKeys across the worker boundary. A real local gateway/SFU test validates two native SDK receivers, spectator
admission, shared controls, rekey/revocation and position preservation. Complete
desktop GUI/WebView playback still requires manual validation. Ordered membership
snapshots prevent coalesced leave/rejoin from retaining a key; a synchronous
request fence prevents canceled asynchronous starts from recreating reception.
