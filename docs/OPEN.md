# Open work

Deferred work goes here, never only in a commit message. **Numbers are
identifiers, not positions** — commits and PRs cite them, so an entry keeps its
number when it closes or moves. Gaps are expected.

Closed and retired entries are not kept; `git log docs/` has them.

## Phases

| Phase | State |
|---|---|
| P1 persistence · P2 deploy artifacts · P4 safety · P5b catalog · P5a core · P6 export/import + named rendezvous | done, tested |
| P3 web/PWA client | deferred, needs a browser |
| P5a tail — delta-sync resume, 2k-connection benchmark | open |
| P6 tail — signed "guild moved" redirect, cross-instance media copy | open (34) |
| P7 cluster mode | demand-gated |
| Postgres backend · role hierarchy + channel overwrites · level curve · Nostr zaps | direction, not scheduled |

## Live

**Project**
- 1 · No `LICENSE` file — defaults to all rights reserved, contradicting the README.

**Security**
- 6 · The gateway is plaintext and `wss://` looks supported without being.
- 32 · A relay older than `can_publish` grants publish, and nothing on the wire detects it.
- 73 · The rendezvous challenge nonce is not asserted to be fresh.
- 83 · Nothing checks `Origin` on the WebSocket upgrade, and the CORS layer
  allows any. WebSockets are not subject to CORS, so any page a user visits can
  open a socket to their own `127.0.0.1` server or to one on the LAN. It cannot
  authenticate — the Schnorr goes over a per-connection nonce — but everything
  reachable before `Identify` is reachable from a web page.
- 84 · The join proof of work is precomputable. `pow_challenge` is
  `"{guild_id}:{pubkey}"`, with no server secret and no expiry, so the work can
  be done offline before ever contacting the server and the answer never stops
  being valid — a ban and a rejoin on the same key reuse it. At 16 bits it is
  ~65k hashes, single-digit milliseconds, so a thousand identities cost under a
  minute. Fixing it means a server secret with a window and more bits, which is
  latency charged to everyone who joins honestly.
- 94 · `status` is chosen from three buttons and the server takes any string.
  It is filtered and capped now, but the closed set is not enforced, so a
  client can set a presence nothing knows how to draw.
- 95 · The name and free-text filters are on the write path only. Both ingest
  paths skip them: `archive.rs` `import_guild` clones the archive's guild,
  roles, channels and members straight into the store, and `load_or_seed`
  inserts every row as it was written. So a hostile archive — the very thing
  cross-instance migration (34) is for — lands unfiltered, and anything stored
  before this filter existed stays that way.

**Repo & CI**
- 8 · The Windows portable and setup ship the wrong icon.
- 9 · Windows SmartScreen blocks the download as an unknown publisher.
- 10 · The macOS build is not notarised.
- 11 · `Discordia.html` should be named and moved, not deleted.
- 12 · The `.icns` is downscaled from the PNG, not rasterised from the SVG.
- 71 · Nothing checks that a test would fail if the code it guards broke.
- 81 · `dx serve` on macOS writes its own `Info.plist` and ignores
  `[bundle.macos] info_plist_path`, so the webview keeps ATS at its default and
  cannot reach a cleartext SFU. `cargo run` is covered by `build.rs`; the CLI's
  only override, `[application] macos_info_plist`, resolves against the shell's
  cwd, so setting it breaks `dx` run from the repo root.

**Tests with no coverage** (from the mutation run)
- 75 · The Nostr DM service loop and `net.rs` are still mostly unguarded. What
  is covered is the part that was lifted onto `AppState` — the arrival, read and
  ring rules, and now the message merge — because `apply` and `insert_message`
  both take a `Signal` and testing them needs a Dioxus runtime. Every other arm
  is untested, and the way to cover one is to lift its decision the same way.
- 76 · Input validation is half tested. Usernames, the image checks and the
  history page cap have tests; the emoji payload cap, the role and emoji
  per-guild limits and the catalog page cap do not. The catalog cap needs more
  than 500 guilds to observe, which is why the test that pretended to cover it
  was deleted rather than kept.
- 87 · The silent Windows update is not tested end to end. The pieces are —
  moving the running program aside, the rollback rule, where the cast-off is
  swept from — but nothing exercises `/S` against a real NSIS install, which
  needs a Windows box with Discordia installed and two published releases.
- 85 · The identify handshake timeout has no test. It is 30 seconds, and a test
  that waits one out does not belong in a suite that has to stay fast; testing
  it needs the timeout to be injectable, which is config surface for a
  test-only knob.

**Server / guild controls**
- 14 · Roster paging.
- 15 · Docker image untested.
- 17 · LiveKit force-eviction on kick — a kicked client is told to hang up, not evicted.
- 18 · Operator UX polish: no visual distinction for a system-guild operator.
- 22 · Nobody has performed the channel-reorder *drag*; only the menu path.
- 23 · `MentionEveryone` permission, cut from v1.
- 97 · The rules prompt names the guild only when the catalog already holds it,
  which an invite-code join does not. `JoinChallenge` carries no name, and
  adding one is a protocol change for a line of dialog copy.

**Bots & activities**
- 25 · Privileged intents have a confirm step, not a verification flow.
- 27 · Activity remote URLs need a CSP story and an allowlist.
- 28 · Per-call activity consent — `message.send` is granted wholesale at launch.

**Decentralisation**
- 30 · The macOS local-network grant is declared but never exercised.
- 34 · Signed "guild moved" redirect after an export/import migration.
- 39 · Nothing on this wire can tell an old client from a new one.

**Client UX**
- 36 · The Windows client is a console application, so every release ships a `cmd` window.
- 77 · The macOS `.app` keeps only half of what 36 gave Windows.
- 37 · The fade-in never re-triggers on the connect and identity-setup panels.

**Camera / video**
- 42 · No camera quality control; fixed 720p30 / 1.2 Mbit.
- 44 · Windows shows WebView2's own camera prompt where macOS shows TCC once.

**Screen sharing**
- 45 · The native picker has no thumbnails.
- 46 · No self-preview on the native capture path.
- 48 · A screen-room reconnect republishes the camera but not the share.
- 51 · The `CVPixelBuffer` double-release test never runs.
- 52 · A macOS watcher can get stuck on "Connecting to stream…" indefinitely.

**Share audio**
- 53 · macOS system-audio capture delivers nothing and reports success.
- 54 · Native window shares still carry the whole machine's audio.
- 55 · Window shares on Windows still depend on the picker.
- 56 · No native system-audio capture on Linux.
- 58 · WASAPI has never been seen answering slowly; the branch is untested.

**Voice / audio**
- 60 · Raw mode is a request nothing confirms. A *failure* is honest — a failed
  `Capture::start` sets `mic_bypass_error` and the panel says the microphone was
  opened the usual way. But `SetClientProperties(AUDCLNT_STREAMOPTIONS_RAW)` can
  return `S_OK` on an endpoint that ignores it, and nothing reads back whether
  the effects actually came out. "On, no error" means asked, not achieved.
- 61 · The raw path is Windows-only. macOS and Linux are equally unanswered —
  `rawmic::supported()` is `cfg!(target_os = "windows")` and the toggle is left
  out of the panel off Windows, so the gap is a missing feature, not a silent
  failure.
- 63 · The default mic sensitivity may cut ordinary speech, and the number
  cannot be settled with synthetic signals: the bar of 50 wants about −30 dBFS
  at the microphone, but a synthetic hop has a crest factor near 2 where speech
  has 3 to 5, so it understates what a real voice delivers. Needs a recording.
  `where_the_gate_opens_once_the_agc_has_normalised_the_level` is the harness.
- 64 · The APM never runs. In `webrtc-sys/src/audio_track.cpp`, `options_` is
  written by `set_options` and read only by `options()` — nothing else in the
  file touches it — so AEC, NS and AGC on a pushed `NativeAudioSource` are
  requests nothing honours. The `set_apm` wrapper that read its own store back
  and logged the round trip as success is gone; what remains is the argument
  `NativeAudioSource::new` demands, passed as `Default`. AGC is
  `client/src/agc.rs` and NS is DeepFilterNet; echo cancellation is 82.
- 98 · The AGC defeats the transmit gate on the default settings. The gate
  judges the peak *after* the AGC, and the AGC normalises toward one level —
  which is the level difference the gate exists to read. Measured with the real
  `Agc` and `GateState`: after two seconds of speech the gain is already up, so
  a pause at −50 dBFS or louder keeps the gate open for all 500 hops measured,
  never closing. Turning noise cancellation on hides it, because DeepFilterNet
  drops room tone under the AGC's floor — but it is off by default. Gating the
  pre-AGC peak fixes the room tone and reintroduces 63, so the two must be
  settled together, 63 first. Was 65, which recorded only the denoiser half.
- 99 · The meter hides 98. `show_pre` in `channels.rs` draws the "before" ghost
  bar only when noise cancellation is on, so on the defaults a person sees one
  normalised bar against a threshold marker that looks meaningful.
- 82 · No echo cancellation anywhere, on any platform. Speakers into an open mic
  feed back, and the defences are a headset and a transmit gate that 98 says is
  open anyway on the default settings.
- 66 · Call audio degrades during a screen share and nothing explains it.
- 68 · Two app instances self-hosting on one machine fight over the SFU.
- 69 · Per-user volumes are session-scoped.

**Nostr / DMs**
- 70 · A DM goes to the relays *we* chose, not the ones the recipient reads.
- 79 · A delete is a local watermark — the events stay on the relays and on the
  other person's machine, and no NIP-09 request is sent.

**Design adoption**
- 2 · Deferred decorative elements from the comps (day streak, "Send a tip").
- 3 · The palette icon is not where the comp puts it.
- 88 · A guild tile has no unread dot: `AppState` counts unread for DMs only
  (`dm_unread`), so there is nothing per guild or per channel to draw from. The
  rail is ready for it — the tile is already a positioned box.
- 89 · A message attachment cannot show its filename or dimensions. Images
  become `media:<sha256>.<ext>` sentinels (trap 3), which carry neither.
- 90 · No multi-line composing. `MessageContent` splits on `\n` and renders
  the breaks, so multi-line messages display, but the draft is a single-line
  `input` in a `form` — Enter submits implicitly and Shift does not change
  that. Needs a `textarea` plus an `onkeydown` that sends on bare Enter, and
  an auto-grow, or the box scrolls one row at a time.

## Accepted trade-offs — recorded, not tracked

- 47 · Windows captures the screen in the webview. WebView2 is Chromium; `getDisplayMedia` works.
- 49 · Stream audio is subscribed on publication, not on watch.
- 57 · Every Windows activation leaks its 12-byte blob, deliberately.
- 59 · The Windows blob's lifetime rule is an observation, not a contract.
- 86 · Two advisories are ignored in `.cargo/audit.toml`, each with its reason
  written beside it: `rsa` arrives through `jsonwebtoken` and never runs an RSA
  operation here, and `tract-nnef` is pinned by a model that will not load on a
  newer one. Ignores without a reason are bugs.
- 80 · A DM delete compares our clock to the sender's `created_at`. Two clocks
  apart by N seconds hide a genuinely new message for N. Per-relay "finished
  replaying" would date it locally instead, but `RelayEvent::Event` withholds
  the relay on purpose — the pool dedupes by id, so first-to-arrive is a race.
