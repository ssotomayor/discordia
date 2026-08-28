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
- 5 · A username may contain control characters and nothing strips them.
- 6 · The gateway is plaintext and `wss://` looks supported without being.
- 32 · A relay older than `can_publish` grants publish, and nothing on the wire detects it.
- 73 · The rendezvous challenge nonce is not asserted to be fresh.

**Repo & CI**
- 7 · macOS clippy gates one profile of two.
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
- 72 · The raid detector has no test at all.
- 74 · Retention can stop deleting without anything noticing.
- 75 · The Nostr DM service loop and `net.rs` are largely unguarded.
- 76 · Input validation has no tests.

**Server / guild controls**
- 14 · Roster paging.
- 15 · Docker image untested.
- 17 · LiveKit force-eviction on kick — a kicked client is told to hang up, not evicted.
- 18 · Operator UX polish: no visual distinction for a system-guild operator.
- 22 · Nobody has performed the channel-reorder *drag*; only the menu path.
- 23 · `MentionEveryone` permission, cut from v1.

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
- 60 · "Bypass system audio processing" reports that raw mode was *asked for*, not achieved.
- 61 · The raw path is Windows-only; Linux is unanswered.
- 62 · The 30-vs-12 dB DeepFilterNet ceiling numbers cannot be re-run.
- 63 · The default mic sensitivity cuts ordinary speech.
- 64 · libwebrtc's noise suppressor may not run at all.
- 66 · Call audio degrades during a screen share and nothing explains it.
- 68 · Two app instances self-hosting on one machine fight over the SFU.
- 69 · Per-user volumes are session-scoped.

**Nostr / DMs**
- 70 · A DM goes to the relays *we* chose, not the ones the recipient reads.
- 78 · Deleting a conversation only exists in the home DM column; the
  in-session list in `features/channels.rs` reads the same `dms` and cannot.
- 79 · A delete is a local watermark — the events stay on the relays and on the
  other person's machine, and no NIP-09 request is sent.

**Design adoption**
- 2 · Deferred decorative elements from the comps (day streak, "Send a tip").
- 3 · The palette icon is not where the comp puts it.

## Accepted trade-offs — recorded, not tracked

- 47 · Windows captures the screen in the webview. WebView2 is Chromium; `getDisplayMedia` works.
- 49 · Stream audio is subscribed on publication, not on watch.
- 57 · Every Windows activation leaks its 12-byte blob, deliberately.
- 59 · The Windows blob's lifetime rule is an observation, not a contract.
- 65 · The transmit gate judges the denoised hop, so its operating point moves with the denoiser.
- 80 · A DM delete compares our clock to the sender's `created_at`. Two clocks
  apart by N seconds hide a genuinely new message for N. Per-relay "finished
  replaying" would date it locally instead, but `RelayEvent::Event` withholds
  the relay on purpose — the pool dedupes by id, so first-to-arrive is a race.
