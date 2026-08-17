# Discordia — Deployment & Scale Roadmap (v2)

> **Status (2026-08):** Phase 1 ✅ shipped (SQLite store, write-through
> metadata, DB-only messages, media blob offload, retention TTL + sweep,
> restart-survival test green). Phase 2 ✅ mostly shipped (env config,
> Dockerfile + compose, SELF_HOSTING.md, `FetchMessages.before_ms` cursor,
> GitHub Actions CI) — still open from P2: roster paging past a threshold,
> catalog fetch-on-demand interim, client infinite-scroll UI for the cursor.
> Phase 4 ✅ shipped (join gates: Open/Rules/PoW; SHA-256 proof-of-work;
> panic-mode lockdown with auto raid-detection; per-channel slowmode with
> moderator exemption; persistent audit log; guild templates friend/foss/
> community; owner UI in Server settings; 6 integration tests green).
> Phase 5 🟡 partial: the guild fan-out was already addressed to members (40
> targeted `deliver()` sites vs 9 `broadcast()`), so "topic delivery" is
> largely met at the addressing layer. Phase 5b ✅ shipped (catalog is now
> pull-based `FetchCatalog` + paginated, requester-only — removed 6
> broadcast-to-everyone catalog storms; 2 tests green). Phase 5a transport Bus
> ✅ shipped (correctness-validated): the broadcast-everything hub is replaced
> by a per-connection routing table — `deliver()` is now O(recipient
> connections) via a pubkey→conn index, `broadcast()` reaches all (rare). A
> slow consumer's bounded queue overflows → the connection is dropped and the
> client reconnects+resnapshots (the successor to lag→resync). 4 transport
> tests green (non-member exclusion, multi-device delivery, disconnect
> cleanup, backward pagination — the DM-privacy test went with the DMs, which
> no longer reach the gateway). **Still open on 5a:** seq-numbered
> *delta-sync* resume (currently a slow client gets a full re-snapshot, not a
> `since` delta), and the **2k-connection load / restart-recovery benchmark**
> (deferred by decision — landed on correctness tests only). Message + catalog
> pagination UI wired client-side ("Load earlier messages" / "Load more").
> Phase 3 (web/PWA) is
> **browser-test-gated**. Phase 6 🟡 partial: export/import core + CLI shipped
> (`archive.rs`, 3 tests green) and persistent rendezvous registrations shipped
> (claimable names, Schnorr-proven, see below); still open — signed "guild
> moved" redirect, media blob copy across instances, and the owner-side "Export
> guild" UI (there is no `ExportGuild` on the wire; the CLI is the only path).
> Phase 7 (cluster) not started, demand-gated.

Goal: serve every community size — a 5-person friend group through a 55k-member
public community — from **one codebase**, with setup effort that scales with the
community instead of being chosen up front, and identity that stays portable
(Nostr) no matter who hosts what.

v2 incorporates an adversarial review of v1. Biggest changes: a dedicated
**community-safety phase** (ban strength is our weakest hard requirement),
**mobile pulled earlier** and scoped honestly, **protocol scalability fixes
(delta sync, Lobby redesign) moved ahead of cluster work**, media out of the
DB from day one, and **cluster mode demoted** to "when a real community needs
it."

## Design stance (decided)

- **Self-hosted ≠ decentralized.** Decentralization lives in (a) Nostr identity
  (no account authority, no forced ID verification) and (b) the federation of
  independent instances + self-hostable rendezvous. A community's data is
  centralized with its operator — by design.
- **Some central services are fine** — as *opt-in conveniences* that are
  themselves self-hostable (rendezvous/directory, shared LiveKit, Blossom).
  Convenience, never lock-in.
- **Nobody re-onboards to grow.** Same binary at every size; graduation between
  run-modes moves data, not people.
- **Earn trust slowly.** The audience we'd serve explicitly distrusts
  fast-built AI projects. Boring reliability at small scale, public history,
  CI, and named maintainers are features; racing to a distributed cluster is
  an anti-feature.

## Run-modes (one binary — *intended* shape, not a shipped selector)

There is no mode switch today: the server reads `DIOXUSFUN_ADDR`,
`DIOXUSFUN_DATA_DIR`, `DIOXUSFUN_OPERATORS` and the `LIVEKIT_*` set
(`docs/SELF_HOSTING.md` is the authoritative list, and it matches the code), and
what distinguishes `embedded` from `standalone` today is who spawns the process,
not a config value. The table is the target.

| Mode | Who it's for | Storage | Bus | Voice | Install |
|---|---|---|---|---|---|
| `embedded` | friend groups | SQLite file | in-process | bundled LiveKit | one click in the client (exists today) |
| `standalone` | small→mid communities | SQLite or Postgres | in-process | bundled LiveKit or shared/Cloud | one binary / `docker compose up` |
| `cluster` | giants (tens of thousands) | Postgres | NATS + Redis presence | LiveKit cluster/Cloud | real ops (N nodes + LB) — **deferred until demanded** |

Two seams have to exist for this, and it is worth being exact about how much of
each one does today — the 2026-08 audit found this section claiming both as
built, which made cluster mode look closer than it is:

- **`Store`** — durable state. **A concrete struct** (`server/src/store.rs`), not
  a trait. Its API shape and TEXT/INTEGER encodings were deliberately chosen to
  be portable to Postgres, so the design work is done; extracting the trait is
  not, and neither is `DATABASE_URL`, which nothing reads yet.
- **`Bus`** — event delivery. **No such trait exists.** What Phase 5a actually
  shipped is a per-connection routing table inside `AppState`: `deliver()` is
  O(recipient connections) via a pubkey→conn index, `broadcast()` is rare. That
  solved the fan-out problem it was aimed at, on one node, which is why it is
  marked done above — but `LocalBus`/`NatsBus` are Phase-7 work that has not
  started.

Neither gap blocks anything today; both are in front of Phase 7 rather than
behind it.

## Central helpers (opt-in, each also self-hostable)

| Helper | Removes from setup | Status |
|---|---|---|
| Rendezvous + `/discover` directory | port-forwarding, IP-sharing, discovery | ✅ exists, with persistent claimable names (Phase 6) |
| Shared LiveKit via rendezvous `livekit_url` | running an SFU | ✅ pattern exists |
| Blossom media | hosting avatars/banners/guild art/message images | ✅ exists (message images move here in Phase 1) |
| Official hosted instance | all install — join like Discord | **deliberately deferred**: an open-registration hosted platform inherits exactly the age-verification/legal exposure that motivated the Discord exodus. Revisit with counsel + gated registration; the product must not depend on it. |

---

## Phase 1 — Persistence: `Store` trait + `SqlxStore` (the keystone)

Restart-survival, real history, and TTL for **every** deployment including
one-click self-host. Explicitly **not** treated as a mechanical port — two
security-sensitive sub-problems get first-class attention:

- **Transactional invariants.** Today's safety rules ride on DashMap entry
  locks (atomic ban = remove+insert; the grant-subset read-then-write; owner
  checks). Every multi-step invariant becomes a **single transactional `Store`
  method** (e.g. `ban_member` is one transaction), so the async port cannot
  reintroduce TOCTOU races. The adversarial-review rules (subset rule,
  moderator immunity, ban-checked-first joins) each get a concurrency test.
- **Permission-cache invalidation as a correctness feature.** Hot paths use a
  per-connection membership/permission cache; kick/ban/role-change events
  **synchronously invalidate** it (bus-delivered invalidation, not TTL expiry).
  Test: kicked user's next frame is rejected, no stale-cache window.
- Schema: users, profiles (+xp), guilds, channels, roles, member_roles,
  members, messages `(channel_id, created_at)` indexed, reactions, bans,
  invites, bot_installs. Migrations via sqlx. SQLite default (embedded),
  Postgres via `DATABASE_URL`. (There is no `dms` table: direct messages left
  the server entirely — see `client/src/nostr/`.)
- **Media out of message rows.** Message images stop being ≤3MB data-URLs in
  the DB: upload to Blossom (fallback: instance-local blob dir served over
  HTTP), messages store URLs. Do it now, while the schema is being written.
- **Storage governance** (solo-operator friendly — disk must be boundable):
  per-guild retention TTL with per-channel overrides (incl. "ephemeral
  channel" 24h/7d mode); size caps (max messages/channel, instance disk
  budget, prune-oldest-first); **separate, shorter media retention** (blobs
  are ~all the growth; expired media leaves a text stub); optional
  archive-then-delete to compressed JSONL (restorable via Phase-6 import);
  precedence: **operator ceilings bound owner choices**. Sweep job must
  reclaim space, not just delete rows (SQLite incremental vacuum / Postgres
  autovacuum tuning). Admin storage stats + budget warnings land with the
  Phase-2 ops work.
- Tests: existing integration suites against SQLite temp files; new
  restart-survival test; new race tests above.
- Checkpoint: all suites green on `SqlxStore(sqlite)`; embedded self-host
  persists across restarts; a kicked user cannot post through any cache window.

## Phase 2 — Deployable standalone + pagination

- Run-mode/config resolution. **Partly shipped:** `LIVEKIT_URL` and
  `DIOXUSFUN_OPERATORS` exist and are documented; `DIOXUSFUN_MODE`,
  `DATABASE_URL`, `BUS_URL` and `discordia.toml` do **not** — nothing in the
  workspace reads them, and they arrive with the backends they select rather
  than before. `client/src/host.rs` embedded path unchanged.
- Deploy artifacts: Dockerfile + `docker-compose.yml` (gateway + Postgres +
  LiveKit), single-binary+SQLite path with systemd example,
  `docs/SELF_HOSTING.md`.
- **History/roster pagination**: cursor-paginate `MessageHistory`; stop
  shipping full rosters in `Ready`/`GuildJoined` past a threshold
  (`FetchMembers` pages). Interim catalog fix: `GuildCatalog` becomes
  fetch-on-demand + coalesced refresh hints instead of full-broadcast-on-every-
  change (full redesign lands with Phase 5).
- **CI from here on**: GitHub Actions running fmt/clippy/tests on SQLite +
  Postgres services. (Trust signal + safety net for everything that follows.)
- Checkpoint: `docker compose up` on a fresh VPS = joinable server; CI green;
  500-member synthetic guild connects fast.

## Phase 3 — Web/PWA thin client (honest scope)

Mobile access is a hard requirement for the communities we target; it comes
before onboarding polish. Scoped as **a second client platform**, not a build
flag:

- **Web networking layer**: the gateway client is tokio/tokio-tungstenite
  (native-only). Introduce a transport abstraction; wasm side uses web-sys
  WebSocket. Shared: protocol types, `apply()` state machine, all views.
- **Key storage**: browser keys live in IndexedDB (extractable — an accepted,
  documented downgrade). Offer nsec import + "burner session key" mode;
  recommend desktop for long-lived identities.
- **Voice/screenshare**: LiveKit JS SDK for mic + screen (the JS-bridge
  pattern from `screenshare.rs`, extended to audio). cpal/native SDK stays
  desktop-only.
- **Push notifications — minimum viable**: Web Push for DMs/mentions requires
  a small opt-in relay component on the instance (VAPID). iOS PWA push
  limitations documented honestly; native apps stay deferred.
- Desktop-only paths (`window().drag()`, embedded server spawn, subprocess
  LiveKit) cfg-gated out.
- Checkpoint: phone browser → join a standalone instance by URL/code → text +
  voice work; DM push arrives on Android/desktop browsers.

## Phase 4 — Community safety & onboarding (the public-community phase)

Our weakest hard requirement: **bans key on pubkeys and Nostr keypairs are
free**, so ban evasion is trivial. This phase makes public communities
defensible *before* any large-scale ambition:

- **Join gates & applications**: per-guild gate = rules-accept, application
  queue (mod-approved joins), invite-only, or open. Templates wire these in.
- **Pluggable join verification (attestations).** The account is always the
  pubkey; guilds may additionally require a one-time **attestation** from a
  configurable set: email (operator SMTP; burner-domain blocklist),
  phone (opt-in, operator-paid SMS provider), OAuth (GitHub/GitLab account
  age — ideal for FOSS guilds), Nostr-native (NIP-05, key age/social graph),
  proof-of-work, vouching by an existing member. Privacy rules: store only
  `(method, salted_hash(credential), pubkey, verified_at)` — never raw
  emails/phones; attestations are per-instance and reusable across its guilds;
  guild owners see "verified", never the credential. **Bans bind to the
  credential hash**, not just the pubkey — that's what makes verification
  defeat ban-evasion (a fresh keypair can't re-verify with a banned phone).
  Installed bots bypass gates (owner installation is the vouching). Phone is
  offered but deliberately not the flagship — no phone-number honeypots.
- **Layered ban enforcement**: pubkey ban (exists) + credential-hash bans
  (above) + IP-level bans/rate limits at the gateway (documented limits: VPNs
  exist) + minimum account age on this instance + optional zap-gated entry
  (future, with Nostr zaps).
- **Anti-raid**: mass-join detection → auto-raise the gate (panic mode:
  lock joins, slowmode); per-user slowmode (per-channel rate limits beyond
  the global 30/10s).
- **Audit log**: append-only moderation log (kick/ban/role/channel/message-
  delete events) + owner/mod panel view. Required at any serious scale.
- **Onboarding wizard + community templates** (friend group / FOSS project /
  public community) seeding channels, roles, gates, visibility — two choices,
  not a hundred toggles.
- Checkpoint: a template-created public guild survives a scripted raid
  (mass keygen + join + spam) with gates + panic mode; audit log shows the
  timeline.

## Phase 5 — Protocol scale-readiness + topic delivery (Tier B)

Everything cluster mode would need, delivered as single-node wins first:

- **Targeted fan-out replacing broadcast-everything.** Fan-out becomes
  O(recipients), not O(connections); bot intent-filtering stays at egress.
  ✅ **shipped, but not as the `Bus` trait this bullet used to promise** — the
  delivered form is a per-connection routing table in `AppState` keyed by a
  pubkey→conn index. It buys the single-node win the phase was for. The `Bus`
  trait, topic subscriptions (`user:{pubkey}`, `guild:{id}`) and `LocalBus` are
  the *extraction*, and they are only worth doing when `NatsBus` needs them —
  so they now sit in Phase 7 rather than being counted as done here.
- **Event sequence numbers + delta sync**: per-topic monotonic seq in the
  protocol; clients resume with `since` cursors instead of full `Ready`
  snapshots. Jittered reconnect/backoff. Kills the reconnect-stampede and
  replaces the hub-lag full-resync.
- **System-guild (Lobby) redesign**: no more auto-join-everyone —
  Lobby becomes opt-in (or presence-silent: membership without per-connect
  MemberJoin storms); `GuildCatalog` fully on-demand + paginated. Removes the
  quadratic presence/catalog storms.
  - ✅ **catalog on-demand + paginated** (Phase 5b): `FetchCatalog { offset,
    limit }` → requester-only paginated `GuildCatalog { guilds, offset, total }`;
    removed 6 broadcast-to-everyone catalog pushes; client fetches on browse
    open, appends later pages. Still open: Lobby opt-in / presence-silent
    membership; client infinite-scroll UI for later pages.
- Checkpoint: 2k-connection synthetic load on one box: flat fan-out cost,
  node restart recovers via delta sync without a DB stampede.

## Phase 6 — Graduation: export / import / redirect

- ✅ `discordia export --guild <id> | --all` → versioned JSON archive;
  `discordia import <archive.json>` remaps guild/channel/role IDs to fresh
  UUIDs, **pubkeys unchanged** — nobody re-registers. Core in
  `server/src/archive.rs` (`Store::export_guild` / `import_guild`), CLI
  subcommands in `main.rs`, 3 round-trip tests green. **Still open:** media
  blob copy across instances (exported messages carry `media:<hash>`
  sentinels — same-instance import shares the blob dir; a cross-instance move
  must also copy `data/media/`), and Blossom-URL rewriting.
- **Reachability**: signed "guild moved" redirect frame (old instance, if
  alive, points clients at the new URL — verifiable against the owner's key) +
  **persistent rendezvous registrations** (registry survives restart, operator
  keys shortcodes) so the address story holds. Without this, graduation ends
  with DMing everyone a URL.
  - ✅ **persistent rendezvous + claimable server names** (this pass): hosts can
    claim a unique, memorable server name (URL-safe, case-insensitive) that
    becomes their `/join/{name}` code. Ownership is proven by a Schnorr
    signature over a rendezvous-issued challenge nonce (same scheme as the
    server's `Identify`), so a name can't be squatted — a different key is
    refused even with a valid signature of its own. Reservations persist to
    `<data>/reservations.json` and reload on restart, so a name stays owned
    while the host is briefly offline. Anonymous random shortcodes still work
    for quick, unauthenticated sharing. 8 tests (name validation, owner-scoped
    uniqueness, persistence-survives-reload, signature verify, + 3 e2e
    handshake). **Still open here:** the signed guild-moved *redirect* frame.
- Owner-side "Export guild" in Server settings.
- Checkpoint: round-trip SQLite → Postgres instance; a connected client
  follows the signed redirect automatically.

## Phase 7 — Cluster mode (demand-gated)

Built **only when a real community's growth demands it** — Phase 5 did the
protocol work, so the addressing is settled. It is **not** purely infrastructure
though, and this section used to imply it was: both seams are still concrete
types, so the extractions land here.

- **Extract the two seams first.** `Store` becomes a trait with a Postgres impl
  behind `DATABASE_URL` (the `$1`-vs-`?` placeholder split means per-backend
  queries or a query layer); the routing table in `AppState` becomes a `Bus`
  trait with topic subscriptions and a `LocalBus`, so `NatsBus` has something to
  implement. Neither is hard — the APIs were designed for it — but neither is
  done, and nothing downstream can start until they are.
- `NatsBus`; presence/voice/typing to Redis (heartbeat TTLs); stateless
  gateway nodes behind an LB; graceful drain, backpressure, metrics.
- Ops runbook + docker-compose→k8s manifests; load harness to ~20k sockets.
- Checkpoint: kill a node mid-chat → LB reconnect + delta sync, no loss;
  load targets met.

---

## Sequencing rationale

1 (durable) → 2 (deployable) → 3 (reachable from phones — hard requirement)
→ 4 (defensible in public — hard requirement) → 5 (scales on one box +
protocol future-proofing) → 6 (growth without lock-in) → 7 (giants, on
demand). Value ships at every checkpoint; stopping after any phase leaves a
coherent product. Phases 3 and 4 are swappable if a flagship community shows
up early and needs defenses first.

## Risk register (from the v1 adversarial review)

| Risk | Addressed |
|---|---|
| Ban evasion via free keypairs | Phase 4 (gates, PoW, vouching, IP limits, anti-raid) |
| Lobby auto-join / catalog broadcast storms | Phase 5 (redesign); interim mitigation Phase 2 |
| Reconnect stampede, full-snapshot-only sync | Phase 5 (seq numbers + delta sync) |
| Async port reopening TOCTOU races | Phase 1 (transactional Store methods + race tests) |
| Stale permission cache after kick | Phase 1 (synchronous invalidation + test) |
| 3MB images inside DB rows | Phase 1 (Blossom/blob offload) |
| PWA effort understated; no push story | Phase 3 (scoped as second platform; Web Push MVP) |
| Graduation breaks reachability | Phase 6 (signed redirect + persistent rendezvous) |
| Hosted instance = legal exposure (age-verification laws) | Deferred deliberately; product independent of it |
| No CI/ops foundation | Phase 2 onward (CI required before merge) |
| Trust optics (fast AI-built distrust) | Stance: small-scale reliability first; cluster demand-gated; public history + CI |

## Explicitly deferred (tracked in `docs/AUDIT-2026-08-17.md` §8)

Threads, cross-instance federation features beyond identity, native mobile
apps (post-PWA), official hosted instance. **E2EE DMs shipped** and are no
longer deferred: NIP-17 gift-wrapped messages over NIP-44, in
`client/src/nostr/`.

Two more, moved here from the register on 2026-08-17 because they are direction
rather than deferred work — neither describes something broken:

- **Role hierarchy and channel permission overwrites.** v1 uses the flat
  grant-subset rule instead, deliberately: a moderator can only grant what they
  hold. Revisit if guilds grow moderation teams with tiers, which is the
  trigger rather than a date — tiers are what make "who may act on whom"
  ambiguous, and the flat rule answers it for free until then.
- **Level curve tuning.** The XP system is real and message-count based; what
  the curve should *be* is a product judgement with no criterion written down,
  so there is nothing to close. It needs a stated goal (how long to reach level
  10, and what a level is worth) before it is work rather than a preference.

**Payments — decided: Nostr zaps, no in-app wallet.** The Solana wallet stays
removed and will not be rebuilt (custody burden, second keypair post-Nostr,
audience mismatch). Payments arrive via the Lightning/Nostr stack, custody-free
(bring-your-own-wallet via NIP-47 Nostr Wallet Connect + LNURL), in this
sequence when picked up:
1. `lud16` Lightning address on profiles + "Send a tip" on the profile card
   (the design comp's tip button, made real);
2. zap-gated guild entry (a Phase-4 attestation: deposit N sats to join —
   the scarcest anti-spam credential of all);
3. zap reactions on messages (⚡ + sats count in the reaction bar, NIP-57
   zap receipts).
If crypto-community demand appears later, chain wallets return only as
signature **attestations** for token-gated guilds (user signs with their own
Phantom/EVM wallet; we verify balance via RPC) — never as custody.

## Invariants to protect through all phases

Hop-in/out voice channels + screenshare + camera; the roles/moderation engine and its
security rules; the bot platform + intent filtering; Nostr identity everywhere
(no bearer tokens, no ID verification); one-click embedded self-host never
regresses.
