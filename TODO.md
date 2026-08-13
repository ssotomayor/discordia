# TODO

Running list of deferred work, scoped to "things we know we want but
chose not to ship in the commit that surfaced them." Newest items at
the top within each section.

## Project

- **No `LICENSE` file.** Without one the repo defaults to all rights reserved:
  nobody can legally fork, modify or contribute, which contradicts the
  contributing sections in `README.md` and `CLAUDE.md`. Deferred because it is
  a decision for both copyright holders, not a code change — `git shortlog -sne`
  shows two authors and there is no CLA, DCO or `CONTRIBUTING.md`, so every
  contributor owns their commits and a later relicense needs all of them.
  Nothing in the dependency tree constrains the choice: of 848 packages the
  only copyleft is four MPL-2.0 crates (file-scoped, compatible either way),
  and every load-bearing dep (livekit, deep_filter, dioxus, wry, cpal) is
  MIT/Apache-2.0.
  The choice interacts with how the project might earn money. Selling the
  *service* (hosting, support, managed rendezvous, zaps) works under AGPL-3.0
  and needs no CLA — AGPL also stops a competitor hosting a closed fork of our
  own work. Selling *licence exceptions* (the classic AGPL dual-licence model)
  needs the right to relicense, so it needs a CLA in place before the first
  outside PR is merged. Deciding late forecloses the second option silently.

## Discordia design adoption

- **Deferred decorative elements from the comps.** The design shows badges with
  no backend yet: day-streak (🔥 "100 day streak"), "Send a tip", "Relay op",
  and org-"Verified". "Key verified" is shown (Identify already proves it) and
  the level/XP system is real (message-count based). Wire the rest to real
  signals when the backends exist (streaks → activity tracking; tips → a Nostr
  zap flow; relay-op → NIP-65 relay list).
- **Theme popover anchoring.** The appearance panel is a centered modal; the
  comp shows it as a popover anchored under the top-bar palette icon. Purely
  positional polish.
- **Level curve tuning.** `level_progress` is a simple 10/level-step curve;
  revisit the numbers (and maybe award XP for reactions/voice) once it's seen
  in use.

## Repo & CI

- **CI runs none of the client's or grid-layout's tests.** The `test` job runs
  `cargo test` over four crates — protocol, server, bot SDK, rendezvous — and the
  comment above it says CI covers the server-side crates "where all the tests
  live". That stopped being true: `cargo test -p dioxusfun` is 30 passing tests
  and `-p dioxus-grid-layout` another 28, so **59 tests exist that no job has
  ever run**. They cover identity and NIP-06 derivation, the resampler and mixer
  arithmetic, the emoji shortcode scanner, the connection-stats rates, and the
  collision and clamping maths behind the panel layout — none of it
  platform-specific, all of it the kind that fails silently.
  The reason for the split is real (the client needs GTK/WebKit to compile, and
  the `test` job deliberately does not install them) but it points at the answer
  rather than away from it: `desktop-build` already installs those deps and
  already compiles the client for clippy, so `cargo test -p dioxusfun -p
  dioxus-grid-layout` belongs in that job, not a new one. The `#[ignore]`d
  CVPixelBuffer test would stay ignored either way — that one is tracked
  separately under Voice / audio.
- **The `dx` pin is not checked against `Cargo.lock`.** The CLI is now pinned to
  the crate version — `DIOXUS_CLI_VERSION` in `ci.yml`, and the same literal in
  `windows-release.yml` — which stops it drifting on every dx release. What is
  left is that nothing verifies the pin still matches: bump `dioxus` and forget
  these, and CI goes back to printing "dx and dioxus versions are incompatible!"
  and bundling anyway. Two literals in two files, kept in step by comments. A
  step that reads the `dioxus` version out of `Cargo.lock` and compares would
  make it structural; `dx --version` is already run right after the install, so
  it has the other half in hand.
- **No macOS clippy.** CI now compiles the client on all three platforms, but
  lints it only on Linux — so the `cfg(target_os = "macos")` half of the client,
  which is where every ScreenCaptureKit and CoreVideo `unsafe` block lives, is
  compiled and never linted. Turning `-D warnings` on for the darwin job means
  landing whatever it finds blind, which is why it was not done in the same
  change that added the job.
- **`Discordia.html` is unreferenced.** 488,666 bytes at the repo root, titled
  "Bundled Page", with its CSS and JS embedded. It arrived in `88d8f1d` — the
  large roadmap-execution commit — and nothing in the workspace mentions it:
  `git grep 'Discordia.html'` outside the file itself returns nothing. The
  dead-code sweep in `03c7e48` removed `connect.mp3` and `discordia-logo.svg` and
  did not touch this. Left in place because deleting another author's file on a
  guess is worse than carrying it; whoever added it should say what it is.
  It is not inert, either. Tailwind v4 scans the project on its own on top of the
  `@source` in `assets/tailwind.css`, and this file is where the `.shadow` rule
  in the committed `tailwind.out.css` comes from — nothing in `client/src`
  produces it. Measured by regenerating with `source(none)`, which drops exactly
  that rule. So an unreferenced 488 KB page is contributing a class to the CSS
  compiled into the binary.

## Assets

- **The `.icns` is downscaled from the PNG, not rasterised from the SVG.** The
  stale-icon problem is fixed — it now carries the current tile-less mark — but
  it was rebuilt with `sips` from `icon-1024.png` rather than per-size from
  `icon.svg` as the `Dioxus.toml` recipe asks, because no SVG rasteriser
  (`resvg`, `rsvg-convert`, ImageMagick) was installed. macOS draws 128px and up
  in the Dock, where downscaling holds up; the 16px entry is the one that would
  benefit from real hinting, and unlike Windows' taskbar macOS only uses it in
  list views. Redo with `resvg` next time the artwork changes.

## Persistence & deploy (roadmap P1/P2 leftovers)

- **Blob GC.** Message-image blobs are content-addressed and shared, so the
  retention sweep deletes rows but not blobs. Needs refcounting (or a
  mark-and-sweep against live message rows) before media-heavy servers can
  reclaim that disk.
- **Roster paging.** `Ready`/`GuildJoined` still ship full member lists — fine
  at hundreds, wrong at thousands. (Catalog fan-out is *done*: `GuildCatalog`
  is now pull-based + paginated via `FetchCatalog`, P5b. Roster paging is the
  remaining piece of that redesign.)
- **Docker image untested.** Dockerfile + compose are written but no docker
  build has been run in this environment; validate on a real box.
- **Postgres backend.** The Store API and TEXT/INTEGER encodings were designed
  for it, but only the SQLite impl exists ($1-vs-? placeholder split means
  per-backend queries or a query layer when it lands).

## Guild owner controls (roles, membership, moderation)

- **The audit log records who acted and never shows it.**
  `AuditEntry.actor_pubkey` is written by every `audit()` call and persisted,
  but the panel in `guild_settings.rs` renders only time, action, target and
  detail — nothing reads the actor. "Who did this" is the question an audit log
  exists to answer. (`GuildEmoji.created_ms` and `.added_by` are write-only in
  the same way, with less at stake.)
- **LiveKit force-eviction on kick.** A kicked user's client is told to hang up
  (cleared `VoiceStateUpdate`), but a malicious client keeps a valid LiveKit
  token until its TTL. Use the LiveKit RemoveParticipant API (or short TTLs)
  for hard eviction.
- **Operator UX polish.** Operators of system guilds (self-host host, or
  `DIOXUSFUN_OPERATORS`) can now moderate the Lobby, but the client doesn't
  visually distinguish "operator of a system guild" from a normal owner, and
  there's no in-app way to see/set the central server's operator list.
- **Invite expiry / use limits.** One rotating high-entropy code per guild
  today; no TTL, no max-uses, no per-code attribution.
- **Channel reorder UI.** `Channel.position` exists and `UpdateChannel` sets
  it, but the client offers no drag-to-reorder yet.
- **`MentionEveryone` permission.** Cut from v1 — mentions are computed
  client-side from content, so the server can't enforce it without rewriting
  message content.
- **Role hierarchy / channel permission overwrites.** v1 uses the flat
  grant-subset rule instead; revisit if guilds grow moderation teams with
  tiers.

## Platform — bots (Tier 1) & activities (Tier 3)

- **`Capability::ChannelRead` is unreachable.** The `channel.get` RPC arm is
  guarded by it, but the only bundled activity declares
  `[UserRead, MessageSend]`, so the guard is always false and the call falls
  through to "permission denied". The sandbox shim doesn't expose `getChannel`
  either. Either wire it into an activity that needs it, or drop the capability
  — right now it reads as supported and isn't.
- **Privileged-intent gate.** Today the owner can grant `message_content` /
  `members` freely. Discord reviews these past a scale threshold. At minimum
  add an extra confirm step in the install UI; longer term, a verification flow.
- **Bot identity refresh.** A bot member's display name is the installer-chosen
  `name`; when the bot connects we don't reconcile its self-declared username.
  Decide which wins (installer label is probably right) and document it.
- **Activity remote URLs.** Activities are bundled/allowlisted and loaded via
  `srcdoc` (opaque origin). Loading arbitrary remote activity URLs needs a CSP
  story, a review/allowlist mechanism, and probably per-origin capability grants.
- **Per-call activity consent.** `message.send` is granted wholesale at launch.
  For higher-trust actions (e.g. a future `wallet.requestPayment`) prompt per
  call, not once at launch.
- **Activity channel binding.** `message.send` posts to whatever channel is
  selected *when the call fires*. Consider binding an activity to the channel it
  was launched from so switching channels mid-session can't misdirect a post.

## Decentralization / rendezvous

- **Reservation display fields are persisted but never read.** `claim_name`
  writes `name`, `description` and `public` into `reservations.json`, and
  `Registry::load` does deserialize the whole `Reservation` back — but `relay.rs`
  touches exactly one field of it, `owner_pubkey`, via `reservation_owner()`.
  (This entry used to say `load()` reads only `slug`, which stopped being true;
  the point did not.) A reconnecting host re-supplies the display fields from its
  `Register` frame, so they survive a restart without being applied to anything.
  Fine if this is groundwork for an offline browse listing; dead weight
  otherwise.
- **Screen tokens minted by a rendezvous ignore `can_publish`.** The local mint
  now grants publish per identity, so the subscribe-only `{pubkey}#audio`
  connection can no longer send. A gateway delegating to a rendezvous sends a
  `MintRequest`, which carries no grants at all, and the relay signs its own
  fixed set with publish on — so on that path the narrowing does not apply.
  Closing it means a grants field on the rendezvous wire and a matching change in
  `rendezvous/src/lib.rs`.
- **Name release / rename.** A host can *claim* a rendezvous name (proven by
  Schnorr signature, persisted) but there's no flow to release it or rename it —
  reservations are sticky once claimed. Add an owner-authenticated unclaim/rename
  (sign a `Release`/`Rename` op against a fresh challenge).
- **Signed "guild moved" redirect.** After an export/import migration, old
  members still point at the source instance. A signed redirect frame
  (verifiable against the owner key) that auto-forwards clients to the new URL
  is the missing reachability piece (roadmap P6 tail).

## Payments

- Payments are via **Nostr zaps** (kept intentionally over a re-added crypto
  wallet). Not yet wired end to end — the "Send a tip" badge in the design is a
  placeholder (see design-adoption section).

## Client UX

- **The fade-in never re-triggers on the connect and identity-setup panels.**
  Both wrapped their switched content in `div { key: "…", class: "fade-in" }` so
  that changing tab or step would replace the node and restart the CSS
  animation. It never did, in any profile: `rsx!` represents a key only on the
  **root** node of a body — `VNode::key` and `HotReloadedTemplate::key` are one
  per body, and `TemplateNode::Element` has no key field at all — so a nested
  key is dropped. Measured against dioxus 0.7.10 with a four-case probe: a root
  element and a root component call both reference the key expression, both
  nested forms do not. That is also why it hid since June — with
  `debug_assertions` off the nested ones warn `unused variable`, and in debug
  they do not, because the hot-reload literal pool captures every dynamic
  literal in the block, dead keys included.
  The dead attributes are gone, so the animation still plays on first mount and
  nothing else changed. Restoring the re-trigger means putting the key where a
  body root is — one per `match` arm, since each arm's `rsx!` is its own body —
  and that is worth establishing before writing it: for a single non-list child,
  nothing here showed that a changed key forces a remount rather than an
  in-place diff. Keys elsewhere in the client all sit inside a `for`, where they
  demonstrably work.
- **Live-session username rename.** The identity card's edit affordance updates
  the local identity + persists it, but takes effect only on the next Connect.
  Mid-session renames need a new protocol message
  (`ClientMessage::UpdateUsername`) + server-side member-row mutation +
  broadcast.

## Screen sharing

- **The native picker has no thumbnails.** `ScreenSourcePicker` is a text list:
  screens, then apps with more than one window, then individual windows grouped
  by app. Discord's grid shows a live-ish still of each candidate, which makes
  picking the right window much faster when six of them are called "Untitled".
  `SCScreenshotManager` (macOS 14+, already in our bindings) would supply them,
  but it hands back a `CGImage` — turning that into something the webview can
  render means an ImageIO/CoreGraphics encode to PNG and a `data:` URL per
  entry, so it is a real chunk of work rather than a field on the struct.
- **Native window shares still carry the whole machine's audio.**
  `sysaudio`'s macOS backend taps system-wide output, so sharing one window
  sends every app's sound with it. Now that the picker knows *which* app was
  chosen, ScreenCaptureKit can scope the audio to it — the missing piece the
  Windows note below also wants, and on macOS the answer is now available.
- **No self-preview picture on the native capture path.** The webview path shows
  the sharer their own outgoing video because the webview holds a local track to
  attach; the native publisher has no track in the webview, and LiveKit does not
  loop a publication back to its publisher. The window now reports what is being
  shared and a live frame count instead of a black box — enough to tell a working
  share from a dead one, which is what the box was failing to do — but it is not
  a picture. Getting one means either teeing frames into the webview as periodic
  stills (an encode per still, sharing the `CGImage`-to-`data:`-URL problem the
  picker thumbnails have) or rendering natively above the webview.
- **Windows still captures in the webview.** Not a defect: WebView2 is Chromium
  and `getDisplayMedia` works there, including the picker. Worth revisiting only
  if a native Windows path (WGC) buys something the picker doesn't — it would
  also give us the window/PID that `sysaudio`'s Windows note wants.

## Screen sharing (live findings, macOS)

- **macOS system-audio capture delivers nothing, and says it succeeded.**
  `sysaudio::start()` returns `Ok` and then produces zero samples, so a share
  carries no sound and nothing reports a failure — `set_system_audio` publishes a
  track that is silent for its whole life. Measured with a controlled test
  (sound playing via `afplay`, Screen Recording granted): 0 samples with a video
  capture running *and* 0 with none, so concurrent SCStreams are not the cause.
  Adding a `SCStreamOutputType::Screen` output alongside the audio one does not
  help either (tried, reverted). Prime suspect is macOS 15+/26 splitting system
  audio out of the Screen Recording grant — the settings pane is now "Screen &
  System **Audio** Recording" — so the tap may be denied separately and silently.
  Whatever the cause, `start()` returning `Ok` for a capture that yields nothing
  is the part that must change: it should fail loudly, the way the Windows
  backend's `fatal` channel does.
  **Status unclear, and that is the actionable part.** `de01daf`
  ("fix(screen-share): restore macOS audio and stream recovery") rewrote 239
  lines of `sysaudio/macos.rs` afterwards and the backend now *has* a `fatal`
  channel and explicit start errors — but that commit shipped with an empty
  message, so nothing in the repo says whether it fixed the silence or only the
  reporting. Do not delete this entry on the strength of reading the code. The
  measurement that closes it is the original one: play audio, share, count
  samples.
- **A macOS watcher can get stuck on "Connecting to stream…" indefinitely.**
  Reproduced once against a Windows sharer. Since then two of the candidates have
  been removed rather than ruled out: `webAudioMix` was being passed to the
  `Room` constructor, which governs subscription, and the SDK is no longer
  fetched from a CDN at all — it is compiled into the binary, so "the JS did not
  load" is now only possible as a bug in our own bundle. `RTCPeerConnection` and
  `new Room()` were tested and work in the webview.
  Still unreproduced since, so still open. The JS controller needs persistent
  diagnostics — room participants, their publications, subscription state, and
  whether `attach` found a track — reported back to Rust, so the next occurrence
  explains itself instead of costing another guess-build-test cycle.

## Voice / audio

The first four here were recorded only in commit messages until now. That is
where deferred work goes to be forgotten: `c86af67` listed five review
follow-ups in its body, three were still open a month later, and nothing was
tracking them.

- **A screen-room reconnect republishes nothing, and the UI still says "live".**
  The JS controller reconnects with exponential backoff when the screen room
  drops, but `connect()` only wires handlers — every publish path
  (`publishTrack`, `setScreenShareEnabled(true)`) hangs off
  `requestAndStartShare`, which only a click reaches. So a sharer who loses that
  room gets a fresh, empty room: video and audio both stop reaching viewers,
  `screen_sharing` stays true, and the badge goes on claiming a share that is
  no longer happening until someone stops and starts it by hand. Surfaced by
  review of the `stopLocalShareAudio` teardown, which is the audio-shaped half
  of it; the video half is the same gap and larger. Fixing it means recording
  enough about the live share (target, quality, whether audio was wanted) to
  re-publish on a fresh room — which the native path already does, via the
  effect in `ScreenShareBridge` keyed on the voice-session epoch.
- **Call audio degrades during a screen share, and nothing explains it.**
  Reported by a user. `c6cb994` measured the obvious suspect — the gain effect
  re-sending identical values, peaking at 12 sends/second — found it too small to
  be the cause, fixed it anyway and wrote "That is still open". Nothing has
  looked at it since. `dlog!` trace points for the share teardown are still in
  `features::voice` and `features::screenshare` from that investigation; use
  them.
- **Stream audio is subscribed on publication, not on watch.** Flagged in
  `c86af67`'s review as "defer `set_subscribed` to watch-start", which would stop
  every member of a channel pulling a share's audio whether or not they open the
  watch window. Left as-is deliberately: `165f26d` connects early *precisely* so
  the first second of a stream is not silent. Two defensible positions, and the
  cost of the current one is only measurable with several watchers on one share.
- **An old client that mutes reads as deafened.** Before the deafen button
  existed the client sent `deafened: muted`, so on a current server every mute
  from such a peer now shows as a deafen to everyone else. `update_voice_flags`
  says so in a comment. The flag was equally wrong before `c2c6ff2`; what changed
  is that something renders it. There is no protocol version to key off, which is
  the actual gap.
- **A peer on an older build cannot watch a natively captured share.** The macOS
  publisher uses the `{pubkey}#video` identity (`17314c3`); older clients resolve
  sharers by bare pubkey only, so they subscribe to nothing and sit on
  "Connecting to stream…" with no error. Both ends need the newer build and
  neither can tell. Same missing piece as the entry above: nothing on this wire
  carries a version, so a break can only be documented, never detected.
- **The `CVPixelBuffer` double-release test never runs.** `sysvideo`'s regression
  test drives a real capture into a real video source, and is `#[ignore]`d
  because it needs a display and the Screen Recording grant. It guards the bug
  that trapped inside `CFRelease` on the capture queue the instant a share
  started — a memory error in `unsafe` FFI, which is the worst class to leave
  uncovered. Needs a macOS runner with a display, or a hand-run checklist before
  releases.
- **Force-quitting the app orphans the bundled SFU.** `LivekitSubprocess` relies
  on tokio's `kill_on_drop`, which only runs if the parent unwinds — a `SIGKILL`
  (force quit, or a debugger) leaves `livekit-server` running, reparented to
  launchd, holding port 7880. One was found alive more than a day after its
  parent died, and a stale SFU squatting on the port is a confusing way for the
  next session to fail. Sweep any `livekit-server` started from our own temp
  directory before spawning, or have the child watch for its parent going away.
  **Not macOS-specific**, which this entry used to imply by describing only the
  launchd case. Reproduced on Windows: `taskkill /F` on `dioxusfun-server.exe`
  left `livekit-server-<hash>.exe` alive and still `LISTENING` on 7880, and it
  had to be killed by PID. `kill_on_drop` is a destructor, so any kill that
  skips unwinding — on any platform — leaves the child behind. That makes the
  "have the child watch for its parent" half of the fix the portable one.
- **No native system-audio capture on Linux.** `client/src/sysaudio/` has
  backends for macOS (ScreenCaptureKit) and Windows (WASAPI process loopback);
  Linux reports unsupported and falls back to whatever `getDisplayMedia` hands
  back. A PipeWire backend would close it, but two things make it more than a
  third copy of the same file: the `pipewire` crate is C bindings (a build and
  runtime dependency the workspace otherwise avoids), and PipeWire has no
  per-process exclusion, so a whole-screen capture would include our own output
  and echo — the very thing the other two backends exist to prevent.
- **Window shares on Windows still depend on the picker.** Native capture only
  takes over whole-screen picks: loopback is machine-wide, so using it for a
  share the user scoped to a single window would leak every other app making
  noise. WASAPI's `INCLUDE_TARGET_PROCESS_TREE` mode would fix this properly,
  but `getDisplayMedia` never tells us which window (or PID) was picked, so
  there is nothing to point it at. Until then a window share carries audio only
  if the user ticks "Share audio" in the picker.
- **Per-user volumes are session-scoped.** `AppState.user_volumes` /
  `stream_volumes` live for the run of the app, not across restarts. Persisting
  them means a keyed store that doesn't grow without bound (cap + LRU), which is
  why it isn't in `ClientSettings` today.
