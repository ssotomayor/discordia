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
  writes `name`, `description` and `public` into `reservations.json`, but
  `load()` only reads `slug` and `reservation_owner()` only `owner_pubkey`. A
  reconnecting host re-supplies them from its `Register` frame, so they survive
  a restart without being applied to anything. Fine if this is groundwork for an
  offline browse listing; dead weight otherwise.
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
- **A macOS watcher can get stuck on "Connecting to stream…" indefinitely.**
  Reproduced once against a Windows sharer. Two theories were tested and both are
  wrong: the LiveKit JS SDK loads fine from its CDN (~100ms), and
  `RTCPeerConnection` / `new Room()` both work in the webview. So the webview is
  capable of subscribing and rendering, and the cause is still unknown. The JS
  controller needs persistent diagnostics — room participants, their
  publications, subscription state, and whether `attach` found a track — reported
  back to Rust, so the next occurrence explains itself instead of costing another
  guess-build-test cycle.

## Voice / audio

- **Force-quitting the app orphans the bundled SFU.** `LivekitSubprocess` relies
  on tokio's `kill_on_drop`, which only runs if the parent unwinds — a `SIGKILL`
  (force quit, or a debugger) leaves `livekit-server` running, reparented to
  launchd, holding port 7880. One was found alive more than a day after its
  parent died, and a stale SFU squatting on the port is a confusing way for the
  next session to fail. Sweep any `livekit-server` started from our own temp
  directory before spawning, or have the child watch for its parent going away.
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
