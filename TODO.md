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

## Security

- **A username may contain control characters, and nothing strips them.**
  `protocol::canonical_username` trims and truncates to 32; it has never
  filtered `is_control()`, and `auth::verify_identify` treats the name as opaque
  bytes in the SHA-256 preimage — so `"al\nice"` is a perfectly valid, signable
  display name that arrives intact.
  It is not only usernames. Guild, channel and role names are the same shape —
  `create_guild` only trims, and `sanitize_channel_name`/`sanitize_role` trim
  and length-check without filtering — as are the pubkeys and the HTTP method,
  path and upgrade header the request log repeats.
  The log-forgery half is closed for all of them: **the rule in the server is
  that only a type which cannot contain a line break is formatted with `%`** —
  `Uuid`, `SocketAddr`, `StatusCode` — and every free-text value uses `?`, which
  quotes and escapes. What remains is that the strings themselves are
  unconstrained, so anything that ever renders them another way — a JSON log, an
  export, a moderation tool — has to make the same choice again.
  **The one deliberate exception is `%e` / `%err`.** Errors stay on `Display`
  because `Debug` on an error chain is materially worse to read at 3am, and the
  path is indirect: an error would have to embed attacker-supplied text
  verbatim. Worth revisiting if a sqlx error is ever seen carrying a user value.
  **The reason it is not simply filtered is the trap.** `canonical_username` is
  the *signed preimage*, computed by both ends before signing and before
  verifying. Adding a filter changes what a new server verifies, so a client
  older than the change signs a different string and is refused — which is
  exactly the lockout `A-02` cost this project once, recorded at length in that
  function's own doc comment. Closing it properly means either accepting that
  break for the rare name that contains a control character, or versioning the
  canonicalisation. Both are decisions rather than edits.
- **The gateway is plaintext, and `wss://` looks supported without being.**
  `tokio-tungstenite` is pinned at 0.24 in the workspace `Cargo.toml` and built
  **without a TLS feature** — `tungstenite 0.24`'s own dependency list carries no
  TLS crate (`Cargo.lock:8262-8277`) — so a `wss://` URL cannot be dialled at all.
  **This entry used to say there was "no TLS backend in `Cargo.lock`", and that
  was wrong.** `rustls`, `tokio-rustls`, `native-tls` and `openssl` are all in the
  lockfile, and `cargo tree -p dioxusfun -i rustls` shows rustls is already
  compiled *into the client* — the workspace standardises on it for HTTP
  (`Cargo.toml:27`, `reqwest` with `rustls-tls`). So enabling TLS on the gateway
  socket costs no new dependency and no new supply-chain surface; only the hard
  part below is actually hard. Meanwhile
  `net.rs:48-53` and `:120-125` normalize `https://` to `wss://` and accept a
  `wss://` scheme as valid input. The two together are the trap: an operator who
  types a `wss://` address gets a connect failure with no hint that TLS was never
  compiled in, and one who types `ws://` gets no warning at all.
  So every gateway frame — messages, LiveKit tokens, presence — crosses the wire
  in the clear on every path: to a rendezvous relay, across a LAN under
  `allow_lan`, and to a remote server. `client/Info.plist` already states this
  outright ("Nothing this app speaks is TLS"), where it is the justification for
  disabling App Transport Security; it is recorded here as the security gap it is
  rather than only as a bundling footnote.
  Worth being clear about what is and is not exposed. Voice and screen-share
  *media* are DTLS-SRTP encrypted in transit by WebRTC, so the gap is the control
  channel — though media terminates at whichever SFU carries it, which is the
  rendezvous's own when the relay is in use.
  The fix is not a one-liner, which is why this is recorded rather than done: a
  self-hosted server at a home IP has no domain and no CA, so it needs either
  certificate pinning against the host's Nostr key — a custom verifier, whose
  failure mode is the silent one where accepting everything looks like working —
  or a transport that authenticates by public key. Both routes are being worked
  out on a branch rather than here.

## Repo & CI

- **macOS clippy gates one profile of two.** The gap the Windows job just
  closed, in the one job nobody wrote it down for. `macos-build` runs
  `cargo clippy -p dioxusfun --all-targets -- -D warnings` once, with
  `debug_assertions` on; `desktop-build` and `windows-build` both run it twice,
  because `unused_variables` and friends fire only with the flag off — which is
  how nine of them reached a published build (`c685828`). So a regression of
  that class in `sysvideo/` or `sysaudio/macos.rs` would still reach a
  published macOS build with nothing red.
  Cheaper to close here than it was on Windows: `macos-build` has no job-level
  `RUSTFLAGS` to be replaced (the comment there says `crt-static` is an MSVC
  concern), so the step is `RUSTFLAGS: "-C debug-assertions=off"` and nothing
  else — the trap that made the Windows one worth a paragraph does not exist on
  this job. The cost is the same: a second compile, on a runner that is already
  the slowest per minute in the matrix.
  Not done in the same PR as the Windows one on purpose. Nobody here has a Mac
  to run either profile by hand first, and the Windows change went in on the
  strength of having been run locally — a macOS step would be going in on the
  strength of having been reasoned about, with a red `master` as the failure
  mode if it takes findings with it, exactly as turning that job's first
  profile on took four.
- **The Windows portable and setup ship the wrong icon.** Reported from a real
  download: both carry an old icon rather than the Discordia mark. Worth
  starting from the comment in `client/Dioxus.toml`, because it says this was
  already diagnosed once — the CLI embeds the *first* `.ico` in the `icon` list
  into the executable's resources, and with none listed it falls back to its own
  bundled Dioxus logo, which is what shipped before. So the first question is
  which artifact was downloaded: a pre-release from before that fix would show
  exactly this, and the bug would be nothing but a stale download. If a current
  build still does it, the `.ico` is reaching NSIS (`installer_icon`) and not the
  executable.
  **The third candidate is ruled out by measurement: the `.ico` is not stale.**
  It holds seven PNG frames — 16/24/32/48/64/128/256 — and every one of them is
  pixel-identical to a fresh `resvg` render of the current `icon.svg` at that
  size: 0 differing pixels of 65,536 at 256px, 0 of 256 at 16px, and the same at
  every size between. They differ from a fresh render only in compressed bytes,
  which is the PNG encoder, not the image. So this file already follows the
  recipe in `Dioxus.toml` — rasterised per size from the SVG rather than
  downscaled — and the drift the Assets entry describes is the `.icns` alone,
  not the icon set as a whole.
  What that leaves is the two candidates above, and neither can be settled by
  reading the config: it needs a `dx bundle` on Windows and an inspection of the
  produced `.exe`'s resources against `assets/icon.ico`.
- **Windows SmartScreen blocks the download as an unknown publisher.** Also
  reported from a real machine: "Windows protected your PC", and it takes
  More info → Run anyway to get past. Expected for unsigned binaries and
  entirely a distribution problem rather than a code one, but it is the first
  thing every new user meets, and "click through the malware warning" is a poor
  first instruction for a project whose pitch is that you should not have to
  trust the operator. Authenticode signing needs a certificate (OV is cheap and
  still earns the warning until reputation accrues; EV clears it immediately and
  costs materially more) — so this is a spend-money decision, not an
  engineering one, which is why it belongs here rather than in a backlog of
  fixes. macOS has the same shape and the same kind of answer — see the next
  entry, which has since been confirmed against a real recipient.
- **The macOS build is not notarised, so anyone we send it to has to defeat
  Gatekeeper by hand.** `bundle-macos.sh` signs the bundle and the DMG and
  verifies the seal, but there is no `notarytool submit` / `xcrun stapler` step,
  and the identity it signs with is an `Apple Development` certificate — a
  development cert, which Gatekeeper does not trust for distribution. Any
  transfer that stamps `com.apple.quarantine` (browser, chat app, AirDrop, Mail)
  therefore fails first-launch assessment on the recipient's Mac, usually as
  **"Discordia is damaged and can't be opened"** rather than anything mentioning
  signatures. Workaround the recipient must run:
  `xattr -dr com.apple.quarantine /Applications/Discordia.app`.
  It has a second symptom that reads as a bug in our code: after granting Screen
  Recording, macOS's own **Quit & Reopen** button silently fails, because the
  relaunch is re-assessed and refused. The picker's error text now says so, which
  is a label on the problem, not a fix.
  Deferred because it is a purchase, not a code change: notarisation needs a
  `Developer ID Application` certificate, i.e. a personal Apple Developer Program
  membership. The only Developer ID in this machine's keychain belongs to an
  employer and must not be used for this project. Note the currently-used
  `Apple Development` cert is itself issued under that employer's team
  (`299HJ3G3BP`) — worth resolving at the same time if the project should be free
  of that association. When it does land: `Entitlements.plist` is the file that
  grows (notarisation requires the hardened runtime, which `--options runtime`
  already sets), and the notarise step should be gated on an env var the way
  `DISCORDIA_SIGNING_IDENTITY` is, so an unset credential still builds.
- **`Discordia.html` should be named and moved, not deleted — it is the design
  comp.** 488,666 bytes at the repo root, titled "Bundled Page", arrived in
  `88d8f1d`, and referenced by nothing in the workspace. This entry used to ask
  whoever added it to say what it is, and that is now answered: it contains
  "100 day streak", "Send a tip", "Relay op" and "Key verified" — the exact
  badges the design-adoption section describes as things "the design shows" — and
  it is the only design asset in the repo. So the entries about the comp point at
  this file, and deleting it would take their reference with it.
  What is left is that nothing says so. It sits at the repo root under a
  meaningless title, which is precisely why it read as dead weight. Moving it to
  something like `docs/design/` and naming it would close this; the reason that
  is deferred rather than done is that it is another author's file and its path
  may be pasted somewhere outside the repo.
  **The stray-CSS half of this entry was wrong and is fixed.** It claimed this
  file was the source of an unused drop-shadow rule in the committed
  `tailwind.out.css`. It was not — the rule survived excluding it. The evidence
  behind the original claim was a `source(none)` build, which disables *every*
  automatic source at once and so could not attribute the rule to any particular
  one. `assets/tailwind.css` now builds with `source(none)` plus one explicit
  `@source` over `src/**/*.rs`, which drops that rule and exactly nothing else
  (measured). See the comment there: the funniest of the real culprits was this
  file, whose own description of the rule was keeping it alive.

## Assets

- **The `.icns` is downscaled from the PNG, not rasterised from the SVG.** The
  stale-icon problem is fixed — it now carries the current tile-less mark — but
  it was rebuilt with `sips` from `icon-1024.png` rather than per-size from
  `icon.svg` as the `Dioxus.toml` recipe asks, because no SVG rasteriser
  (`resvg`, `rsvg-convert`, ImageMagick) was installed. (`resvg` is installed on
  the Windows host now, and the `.ico` there measures as correctly rasterised —
  see the Windows icon entry under Repo & CI — but `iconutil` is macOS-only, so
  the `.icns` still cannot be rebuilt from here.) macOS draws 128px and up
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

- **The macOS local-network grant is declared but never exercised.**
  `client/Info.plist` now carries `NSLocalNetworkUsageDescription`, added
  prophylactically alongside the App Transport Security fix rather than in
  response to a failure. macOS 15+ gates LAN access behind a per-app grant that
  an unbundled build inherits from the terminal, so the path that would prove
  this — `allow_lan` self-hosting from a *bundled* app, with a friend joining
  across the same network — has not been run. Two things to check when it is: that
  the grant prompt appears at all (a missing usage description can mean silent
  denial rather than a prompt), and that a denial surfaces as something better
  than a connect timeout. Until then the key is insurance, not a verified fix.
- **Reservation display fields are persisted but never read.** `claim_name`
  writes `name`, `description` and `public` into `reservations.json`, and
  `Registry::load` does deserialize the whole `Reservation` back — but `relay.rs`
  touches exactly one field of it, `owner_pubkey`, via `reservation_owner()`.
  (This entry used to say `load()` reads only `slug`, which stopped being true;
  the point did not.) A reconnecting host re-supplies the display fields from its
  `Register` frame, so they survive a restart without being applied to anything.
  Fine if this is groundwork for an offline browse listing; dead weight
  otherwise.
- **A relay older than `can_publish` still grants publish, and nothing detects
  it.** `MintRequest` now carries the flag and `POST /voice-token` honours it, so
  a rendezvous-delegated mint narrows the subscribe-only `{pubkey}#audio`
  connection the same way the local one does. But the field is
  `#[serde(default = "publish_by_default")]` on the relay — `true` — because a
  *host* older than it does not send it and must keep working. The mirror of
  that: a *relay* older than it ignores the field and signs publish, exactly as
  it did before, and the gateway gets a valid token back with no way to tell.
  Same shape as "An old client that mutes reads as deafened" under Voice /
  audio: nothing on this wire carries a version, so a mixed-version deployment
  can only be documented, not detected. Closing it properly means versioning the
  rendezvous protocol, which is a bigger decision than this field was.
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

- **The Windows client is a console application, so every release ships a `cmd`
  window beside it.** Nothing in the workspace sets `windows_subsystem` — a grep
  across every file finds no occurrence — so the linker leaves the default, and
  the binary comes out as subsystem 3 (`IMAGE_SUBSYSTEM_WINDOWS_CUI`). Read out
  of the PE headers of the two exes a debug build produces here,
  `dioxusfun-server.exe` and `dioxusfun-rendezvous.exe`: both 3. For those two it
  is correct — they are console tools. For the desktop app it is a black window
  that opens with the UI and stays for the session, and one-click self-host
  fills it with the bundled SFU's own logs, `livekit_bundle` spawning it with
  `stdout`/`stderr` on `inherit()`. Including an `ERROR` from `hwstats` about
  having no Windows CPU backend, which is LiveKit disabling capacity management
  on purpose and reads to a user like a crash.
  The two halves of the fix have to land together, because the first one alone
  makes it worse. `#![cfg_attr(not(debug_assertions), windows_subsystem =
  "windows")]` takes the console away from the parent, and Windows then
  allocates a *fresh* one for a console child — so the shared window becomes a
  stray `cmd` belonging to `livekit-server`. The other half is `CREATE_NO_WINDOW`
  in `creation_flags` plus piped stdio on that spawn.
  What makes it more than two flags is where the output should go instead. The
  client writes to the console in 96 places — 92 `eprintln!` and 4 `println!`,
  66 of them in `features::voice` alone, and `host.rs`'s four are the self-host
  narration (`[host] livekit ready at …`, `[host] livekit unavailable: {e}`).
  A windowless process has no stderr for any of that to reach, so it does not
  become quieter, it becomes unreadable.
  And the obvious destination is not available. `devlog` writes to a file, but
  `dlog!` compiles to nothing in release *on purpose* — the module docs say why:
  a shipped client must not grow an unbounded record of who was in which voice
  channel. The console is therefore the only diagnostic channel a release build
  has today, which is the same reason it cannot simply be taken away. Closing
  this means deciding what replaces it — a bounded, non-identifying log; an
  opt-in `--verbose` that calls `AttachConsole`; or accepting that release
  self-host is diagnosed by reproducing under `dx serve`. That decision is the
  work, not the two flags.
  Not verified against a release build: `target/` here holds debug artifacts
  only, and the reasoning above is from the source and from debug PE headers.
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

## Camera / video

- **`ClientMessage::SetScreenShare` still carries a `channel_id` nothing reads.**
  The flag moved onto `VoiceState`, so the server takes the channel from the
  sender's own voice state and ignores the field. It is kept on the wire because
  removing it would break a client older than the change: serde drops fields the
  struct does not name, so an old client talking to a new server still works,
  while the reverse would not. Removable once no old clients are expected — which
  nothing on this wire can detect, so it is a judgement call rather than a check.
- **`ScreenShareState` is now redundant for current clients.** It is still sent,
  derived from `screen_sharing` on the voice states rather than from a map of its
  own, purely so a client older than that flag keeps its LIVE badge. Same
  removal condition, and the same inability to detect when it is met. Deriving it
  at least means the two can no longer disagree.
- **An old client can render a webcam in the screen tile.** Camera video shares
  the screen room and the bare-pubkey identity, told apart only by
  `TrackSource`; a client older than `features::camera` keys video tracks by
  identity alone, so if someone is sharing a screen *and* a camera it will show
  whichever arrived last. Nothing on the server can mitigate it — it is inherent
  to reusing the room, which is the same property that made the camera cost no
  new token or identity. Resolves itself as clients update.
- **No camera quality control.** Fixed at 720p30 / 1.2 Mbit with simulcast on.
  Screen sharing has a preset table (`QUALITY_PRESETS`) and a picker; the camera
  has neither. Worth having once there is any evidence about what people's
  uplinks actually do with camera + screen at the same time — two video uplinks
  is the case to measure, and on Windows both encoders live in one WebView2
  process.
- **A `persistence` test failed once under a full-workspace run, undiagnosed.**
  Seen exactly once in five `cargo test --workspace` runs while adding the camera
  tests; three deliberate re-runs afterwards were clean, and both tests pass
  alone. Not the temp-dir collision that `temp_data_dir`'s comment describes —
  that helper is keyed by pid + nanos + counter and is genuinely unique. The
  suspicion is a timing window rather than shared state: the camera tests each
  spawn a gateway and use 700ms idle terminators, so the suite now runs more
  concurrent servers than it did. Recorded rather than chased because there is no
  captured assertion message to work from; the next occurrence should be run with
  `--nocapture` and `RUST_BACKTRACE=1` before anything is changed.
- **Windows shows WebView2's own camera prompt**, where macOS shows the TCC
  prompt once. Expected (wry auto-allows only clipboard-read), and persistence
  depends on the WebView2 user-data folder, but it is the kind of platform
  difference that gets reported as a bug. `ICoreWebView2Profile4::
  SetPermissionState` is the escape hatch if it misbehaves; `webview2-com-sys`
  is already a dependency.

## Screen sharing

- **The native picker has no thumbnails.** `ScreenSourcePicker` is a text list:
  screens, then apps with more than one window, then individual windows grouped
  by app. Discord's grid shows a live-ish still of each candidate, which makes
  picking the right window much faster when six of them are called "Untitled".
  `SCScreenshotManager` (macOS 14+, already in our bindings) would supply them,
  but it hands back a `CGImage` — turning that into something the webview can
  render means an ImageIO/CoreGraphics encode to PNG and a `data:` URL per
  entry, so it is a real chunk of work rather than a field on the struct.
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

- **A screen-room reconnect republishes the camera but not the share.**
  The JS controller reconnects with exponential backoff when the screen room
  drops. `connect()` now restores the *camera* on the far side of that, by
  republishing the `MediaStreamTrack` it still holds — but every screen publish
  path (`publishTrack`, `setScreenShareEnabled(true)`) still hangs off
  `requestAndStartShare`, which only a click reaches. So a sharer who loses that
  room gets a room with their face in it and no screen: the video stops reaching
  viewers, `screen_sharing` stays true, and the badge goes on claiming a share
  that is no longer happening until someone stops and starts it by hand.
  The camera's fix is the shape to copy, and the reason it was easy there is the
  reason it is harder here: a camera track can be republished as-is, whereas a
  screen share has to be *re-acquired*, and `getDisplayMedia` needs a user
  gesture a reconnect does not have. So this needs either the picker reopened, or
  the original track held across the room the way the camera's is. The native
  macOS path already sidesteps it, via the effect in `ScreenShareBridge` keyed on
  the voice-session epoch. (Was recorded as "republishes nothing"; the camera
  half stopped being true when `features::camera` landed.)
- **Stream audio is subscribed on publication, not on watch.** Flagged in
  `c86af67`'s review as "defer `set_subscribed` to watch-start", which would stop
  every member of a channel pulling a share's audio whether or not they open the
  watch window. Left as-is deliberately: `165f26d` connects early *precisely* so
  the first second of a stream is not silent. Two defensible positions, and the
  cost of the current one is only measurable with several watchers on one share.
- **A peer on an older build cannot watch a natively captured share.** The macOS
  publisher uses the `{pubkey}#video` identity (`17314c3`); older clients resolve
  sharers by bare pubkey only, so they subscribe to nothing and sit on
  "Connecting to stream…" with no error. Both ends need the newer build and
  neither can tell. Same missing piece as "An old client that mutes reads as
  deafened" under Voice / audio: nothing on this wire carries a version, so a
  break can only be documented, never detected.
- **The `CVPixelBuffer` double-release test never runs.** `sysvideo`'s regression
  test drives a real capture into a real video source, and is `#[ignore]`d
  because it needs a display and the Screen Recording grant. It guards the bug
  that trapped inside `CFRelease` on the capture queue the instant a share
  started — a memory error in `unsafe` FFI, which is the worst class to leave
  uncovered. Needs a macOS runner with a display, or a hand-run checklist before
  releases.
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

### Share audio (sysaudio)

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
  macOS is the only platform where nobody has seen a sample. Windows is settled
  the other way: `windows_loopback_delivers_real_samples` reaches a peak of ~0.35
  against a tone from another process, and finding that took a heap-corruption
  crash in `sysaudio::windows::activation_params` out with it.
  **Status unclear, and that is the actionable part.** `de01daf`
  ("fix(screen-share): restore macOS audio and stream recovery") rewrote 239
  lines of `sysaudio/macos.rs` afterwards and the backend now *has* a `fatal`
  channel and explicit start errors — but that commit shipped with an empty
  message, so nothing in the repo says whether it fixed the silence or only the
  reporting. Do not delete this entry on the strength of reading the code. The
  measurement that closes it is the original one: play audio, share, count
  samples.
- **Native window shares still carry the whole machine's audio.**
  `sysaudio`'s macOS backend taps system-wide output, so sharing one window
  sends every app's sound with it. Now that the picker knows *which* app was
  chosen, ScreenCaptureKit can scope the audio to it — the missing piece the
  Windows note below also wants, and on macOS the answer is now available.
- **Window shares on Windows still depend on the picker.** Native capture only
  takes over whole-screen picks: loopback is machine-wide, so using it for a
  share the user scoped to a single window would leak every other app making
  noise. WASAPI's `INCLUDE_TARGET_PROCESS_TREE` mode would fix this properly,
  but `getDisplayMedia` never tells us which window (or PID) was picked, so
  there is nothing to point it at. Until then a window share carries audio only
  if the user ticks "Share audio" in the picker.
- **No native system-audio capture on Linux.** `client/src/sysaudio/` has
  backends for macOS (ScreenCaptureKit) and Windows (WASAPI process loopback);
  Linux reports unsupported and falls back to whatever `getDisplayMedia` hands
  back. A PipeWire backend would close it, but two things make it more than a
  third copy of the same file: the `pipewire` crate is C bindings (a build and
  runtime dependency the workspace otherwise avoids), and PipeWire has no
  per-process exclusion, so a whole-screen capture would include our own output
  and echo — the very thing the other two backends exist to prevent.
- **Every Windows activation leaks its 12-byte blob, deliberately.**
  `activation_params` allocates one per `activate` call and never frees it,
  because the engine keeps the pointer and nothing documents for how long. So the
  cost is 12 bytes per share — 24 when a share falls through to the PCM format —
  rather than 12 for the process. That is the trade: a process-wide blob saved
  those bytes and bought a lease, a retirement rule for activations abandoned on
  timeout, and a dead end with no recovery when a capture thread hung holding the
  lease. Unbounded in principle, since it scales with how many times a user
  starts a share; a thousand shares in one run is 12 KB, so nothing here is
  waiting on a fix. What would change the calculation is somewhere else starting
  to activate in a loop.
- **The activation-timeout path has never been executed.** `activate`'s
  `WaitForSingleObject` giving up is reasoned-about code: nobody has made WASAPI
  take longer than 5 s to answer, so the branch that abandons an operation and
  leaves it holding its blob has never run outside a reading of it. Same class of
  gap as the one `windows_loopback_delivers_real_samples` closed by being run
  once — that found a heap corruption on its first execution. A fault-injection
  seam would settle it; `server/tests/voice.rs`'s `ScriptedMinter` is the shape
  to copy.
- **The Windows blob's lifetime rule is measured, not documented.** Microsoft
  does not say how long `ActivateAudioInterfaceAsync` needs the `VT_BLOB` it is
  handed. "As long as the process" comes from probing: the engine had not freed
  the block, and 64 same-size allocations afterwards never landed on its
  address. If a future Windows *does* free it, we would be handing it a Rust
  allocation to release — which works today only because `CoTaskMemAlloc` and
  Rust's allocator both sit on the process heap, not because anything promises
  it. Measured on Windows 11 26200 and one machine; no other platform even
  compiles this file.

## Voice / audio

- **"Bypass system audio processing" reports that raw mode was *asked for*, not
  that the driver's effects are gone.** `rawmic::setup` sets
  `AUDCLNT_STREAMOPTIONS_RAW` and reports the failure if `SetClientProperties`
  refuses, which is the only signal WASAPI offers — there is no read-back
  saying the APO chain is actually out of the path. Compare `ActiveVoice::
  set_apm`, which writes the options and then reads them back precisely because
  a write that silently does nothing is the failure mode worth catching, and
  which found exactly that. So a driver that accepts the property and ignores it
  leaves the switch lit over an unchanged signal, and nothing in the client can
  tell. What would settle it is a measurement, not an API: the ignored
  `client/tests/live_sfu.rs` sweep already analyses a tone through the real
  path, and the same room noise recorded with the switch on and off would show
  whether the endpoint's suppressor is still working. Until then the panel's
  wording is a claim about what we requested.
- **The raw path is Windows-only and Linux is not answered.** `rawmic::
  supported()` is `cfg!(target_os = "windows")`, and the setting hides
  elsewhere. macOS is genuinely nothing-to-do (cpal opens a plain HAL input
  unit, so the system's mic modes never apply). Linux is not: PipeWire and
  PulseAudio both routinely put `echo-cancel`/`rnnoise` modules in front of a
  source, and a client that wants the unprocessed device has to select it
  rather than ask for a flag. Nobody has run this repo's voice path on Linux
  yet, so it is filed here rather than guessed at.
- **The 30-vs-12 dB ceiling numbers cannot be re-run from the repo.** The entry
  below and `ClientSettings::denoise_atten_lim_db` both cite figures from a
  live session — 21.2% vs 17.6% gate drops, −3.9 dB vs −2.7 dB actually applied
  — and those figures are the whole argument for the control's existence and
  its default. `the_knobs_that_shape_voice_quality_are_measured` sweeps `apm`,
  `red`, `dtx` and `max_bitrate`; the ceiling is not a dimension in it, and
  DeepFilterNet is not in the sweep at all. The house rule is that a claim
  about voice quality has a number behind it; right now this one has a number
  behind it that only one machine ever produced. Adding it as a sweep dimension
  needs the noise-mixed signal the sweep already grew for the APM question — a
  pure sine tells a denoiser as little as it told the APM.
- **The default mic sensitivity cuts ordinary speech.** `default_mic_sensitivity`
  is 50 and the scale is peak ×1000, so the gate opens at **−26.0 dBFS** — 7.6 dB
  stricter than the 21 that `efcc23d`'s reporter was already struggling with, and
  that was with their input gain at 200% against this one's unity default.
  Reproduced on a two-machine LAN call: the far end heard the speaker fade out and
  come back. Measured through the outbound packet rate, 50/s while the gate is
  open and near zero while it is shut — same speaker, mic and room, one slider
  moved:

  ```text
  −26 dBFS (default)  50 19 14 43 4 6 38 50 50 44 51 … 26 … 12 … 5
  −36 dBFS            50 50 40 51 50 50 50 50 50 51 50 50 50 50 51 …
  ```

  So at the default the gate shuts mid-phrase and reopens, cutting word tails and
  unvoiced consonants rather than chattering randomly.
  **Do not just lower the number.** −36 is one mic in one room, and too low a
  threshold sends room noise to everyone with no working suppressor holding it
  back. The default was chosen on the assumption that AGC lifts quiet speech over
  it — the entry below finds that AGC inert, so settle that first. The other
  candidate is a first-run calibration: the VU bar already draws the threshold
  against a live meter, so "speak normally" could place it.
  **Already ruled out by measurement — do not retry it:** exposing
  DeepFilterNet's `ATTEN_LIM_DB` ceiling. Dropping it 30 → 12 dB moved gate drops
  21.2% → 17.6% on matched input levels, inside the run-to-run variance, and the
  sign flipped in the other two bands. The model applies −3.9 dB on average at a
  30 dB ceiling and −2.7 dB at a 12 dB one — it is a ceiling, and speech almost
  never reaches it, so lowering it moves about a decibel. Drops track *input* level and
  nothing else: below 0.05 raw peak, about half of all hops are gated whatever the
  ceiling is.
- **libwebrtc's noise suppressor may not run at all, which would make
  DeepFilterNet-off suppressor-*none*.** `apm_options` documents the contract —
  "exactly one suppressor should be in the path" — and applies it through
  `NativeAudioSource::set_audio_options`. Measured through that call it does
  nothing: eight runs of `the_knobs_that_shape_voice_quality_are_measured` against
  the bundled SFU, white noise at 0.15 peak over the 0.5 tone, comparing the share
  of energy left in the tone's ±40 Hz band.

  ```text
  noise, APM off   0.8984  0.9068  0.9020  0.9046   mean 0.9030
  noise, APM on    0.9035  0.9014  0.9045  0.9039   mean 0.9033
  ```

  +0.0004, with the sign flipping run to run. The instrument is not blind: the
  noise itself moves the same number 0.940 → 0.903, same sign every run, so it
  resolves an effect ten times smaller than a working suppressor would produce.
  **It matters because it is the default arrangement.** `noise_cancellation`
  defaults to false, so `apm_options` turns libwebrtc's suppressor *on* for every
  fresh install — the one configuration where an inert APM costs anything. A
  default install would then have no suppressor, no AGC, and the mic-sensitivity
  default above resting on a rescue that never comes.
  **Half of it is settled: the options do arrive.**
  `the_apm_options_survive_the_round_trip_into_the_source` writes both polarities
  and reads them back unchanged, so this is not our plumbing. What remains is
  stored-and-not-acted-on inside libwebrtc, on a source we feed frames to rather
  than one it captures from — the worse branch, because it is not ours to fix and
  would make the AGC switch decorative and the suppression switch decorative
  whenever DeepFilterNet is off.
  What would close it: a capture with DeepFilterNet off, real speech, and a
  spectrogram either side of the toggle; or reading where libwebrtc attaches the
  APM (`webrtc-sys/src/audio_track.cpp` stores into `options_`, and what reads
  `options_` is the thread to pull). Band-energy share is not perceptual, only one
  SNR (~10 dB) and noise colour was tried, and AEC cannot be tested this way at
  all — so **do not delete this entry on the strength of reading either one
  alone.**
- **The transmit gate judges the denoised hop, so its operating point moves when
  suppression is toggled.** `denoise_gate_loop` runs the model first and gates
  what comes out, so switching DeepFilterNet on drops the signal the gate is
  measuring while the threshold the user calibrated by ear stays where it was.
  `efcc23d` made that survivable — hysteresis, a released envelope, ramped
  edges — and left the ordering alone on purpose: gating the raw signal would
  hold the operating point still across the toggle, but it would also let a fan
  the model removes hold the gate open, which is the reason the order is what it
  is. Recorded here rather than only in that commit because it is a live
  trade-off, not a closed one: the alternative is a threshold that is
  re-derived when suppression changes, which keeps the ordering and removes the
  jump. Same shape as "Stream audio is subscribed on publication" under Screen
  sharing — two defensible positions, and the cost of the current one is a
  recalibration the user has to do by hand.
- **Call audio degrades during a screen share, and nothing explains it.**
  Reported by a user. `c6cb994` measured the obvious suspect — the gain effect
  re-sending identical values, peaking at 12 sends/second — found it too small to
  be the cause, fixed it anyway and wrote "That is still open". Nothing has
  looked at it since. `dlog!` trace points for the share teardown are still in
  `features::voice` and `features::screenshare` from that investigation; use
  them.
- **An old client that mutes reads as deafened.** Before the deafen button
  existed the client sent `deafened: muted`, so on a current server every mute
  from such a peer now shows as a deafen to everyone else. `update_voice_flags`
  says so in a comment. The flag was equally wrong before `c2c6ff2`; what changed
  is that something renders it. There is no protocol version to key off, which is
  the actual gap.
  **Half of that gap is now closed, in the direction that matters.** `Identify`
  carries `client_version` and the server logs it, so an operator can count what
  is connected — and a client old enough to have this bug is identifiable by
  *absence*: it sends no version at all, and the log says `unknown`. What is
  still missing is anything that could act on it. The field is self-declared and
  unauthenticated (the signature covers `nonce || pubkey || username` and not
  it), so it can inform a decision about whether these three compatibility
  entries can be closed; it cannot gate behaviour per peer.
- **Two app instances self-hosting on one machine fight over the SFU.** The
  orphan reclaim added with `PID_FILE` cannot tell "the recorded SFU is a
  leftover" from "the recorded SFU belongs to an instance that is still
  running": both look like a live process with our image name. So a second
  instance that self-hosts kills the first one's SFU and takes port 7880.
  That configuration was already broken — the port, the temp directory and the
  generated `livekit.yaml` are all fixed, so a second SFU could never have bound
  anyway; before the reclaim, the second instance silently *shared* the first's.
  It now fails differently rather than newly, which is why this is recorded
  rather than treated as a regression. Closing it properly means either a lock
  the running instance holds (and releases on any kind of exit, which is the
  same problem one level down) or making the port configurable so two instances
  can coexist.
- **Per-user volumes are session-scoped.** `AppState.user_volumes` /
  `stream_volumes` live for the run of the app, not across restarts. Persisting
  them means a keyed store that doesn't grow without bound (cap + LRU), which is
  why it isn't in `ClientSettings` today.
