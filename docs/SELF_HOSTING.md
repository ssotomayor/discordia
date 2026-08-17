# Self-hosting Discordia

Three rungs, by community size. All three run the same code and the same
on-disk format — you can graduate by copying one directory.

For **who can reach you and who can read your traffic** — what a rendezvous
relay does and does not carry, and why a self-hosted machine is hard to reach
from the internet at all — see [`NETWORKING.md`](NETWORKING.md). It is the
companion to this page: this one is how to run a server, that one is what happens
on the wire once you do.

## Rung 1 — one click (friend groups)

Open the app → **Self-host** → Launch. The client spawns the gateway and the
bundled LiveKit voice server in-process. Nothing to install.

- Durable data (guilds, members, messages, media) lives in
  `<config dir>/dioxusfun/host-data/` — it **survives restarts**.
- Optional: publish a shortcode via a rendezvous server so friends join
  without your IP (checkbox on the Self-host tab).
- You are automatically the **operator** of your server's Lobby.

## Rung 2 — one box (small → mid communities)

A VPS with Docker:

```bash
git clone <this repo> && cd <repo>
cp .env.example .env          # set PUBLIC_HOST, and change the LiveKit secret
docker compose up -d discordia livekit
```

Clients connect with the **URL** tab → `ws://your.domain:9000`.

Name the services you want: `docker compose up -d` on its own also starts the
`rendezvous`, which is the *other* way to be reachable (rung 1 hosting from
your laptop) and pointless alongside an always-on gateway. `PUBLIC_HOST` is
required — compose refuses to start without it, since it's what goes into the
LiveKit URL handed to clients.

- Data volume: `discordia-data` (SQLite + media blobs). Backup = snapshot the
  volume.
- Voice: LiveKit runs as a sibling container. For production set real
  credentials (`LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET`, ≥32 chars) instead of
  the dev defaults, and open UDP 7882.
- Operators: `DIOXUSFUN_OPERATORS=<hex pubkey>[,<hex pubkey>…]` grants Lobby
  moderation.
- No Docker? `cargo build --release -p dioxusfun-server` gives a single binary;
  point `DIOXUSFUN_DATA_DIR` somewhere persistent and run it under systemd.

### Environment reference

| Var | Default | Meaning |
|---|---|---|
| `DIOXUSFUN_ADDR` | `0.0.0.0:9000` | gateway bind address |
| `DIOXUSFUN_DATA_DIR` | `./discordia-data` | SQLite DB + media blobs |
| `DIOXUSFUN_OPERATORS` | *(empty)* | comma-separated hex pubkeys who moderate system guilds |
| `LIVEKIT_URL` | *(derived per-connection)* | LiveKit URL handed to clients |
| `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` | dev defaults | must match the LiveKit server |
| `LIVEKIT_PORT` | `7880` | port used when deriving the LiveKit URL for clients (the bundled SFU always spawns on 7880) |
| `DIOXUSFUN_LIVEKIT_AUTOSPAWN` | `1` | set `0` when LiveKit runs separately |

Rendezvous relay (`dioxusfun-rendezvous`, a separate binary — see
`rendezvous/README.md`):

| Var | Default | Meaning |
|---|---|---|
| `DIOXUSFUN_RENDEZVOUS_ADDR` | `0.0.0.0:7700` | relay bind address |
| `DIOXUSFUN_RENDEZVOUS_DATA_DIR` | `./rendezvous-data` | persisted name reservations (`reservations.json`) |

Compose-only: `PUBLIC_HOST` (see `.env.example`) is read by
`docker-compose.yml`, not by the server — it builds the `LIVEKIT_URL` above.

Client-side, for reference: `DIOXUSFUN_CONFIG_DIR` relocates the identity, dev
log and settings, and `DIOXUSFUN_RENDEZVOUS_URL` presets the rendezvous the
Self-host tab offers. The three `DISCORDIA_E2EE*` variables are also client-side
and have their own section below. Build-time, `LIVEKIT_BUNDLE_SKIP=1` skips fetching the
LiveKit binary — useful for CI, but the resulting build cannot host voice — and
`LIVEKIT_BUNDLE_VERSION` (default `1.12.0`, `server/build.rs`) pins which
`livekit-server` release gets embedded. Also build-time, `DISCORDIA_VERSION`
(`client/build.rs`) is the version the client reports in its corner and, when
set, must be exactly the tag the artifact is published under — CI sets it on the
three publishing jobs. Left unset, the build calls itself `<crate>-dev+<sha>`,
which is what a local build should say.

### Direct messages do not touch your server

Worth knowing if you run a host: **DMs are not stored here.** They are NIP-17
gift-wrapped events on Nostr relays, addressed to a user's key. Your database
has no `dms` table and your gateway never sees one, which means you cannot read
them, cannot back them up, and cannot lose them for your users — and a user who
moves to another server keeps the whole conversation.

The relays are chosen per client (`dm_relays` in the client's settings, falling
back to several unaffiliated defaults), not by you. What a relay can see is that
somebody sent a given pubkey a message at a given time: gift wrapping signs the
outer event with a throwaway key, so not even the sender is visible to it.

### Media encryption (client-side)

Voice, screen video and camera all terminate at an SFU, which decrypts and
re-encrypts every frame — in *every* configuration, including a direct
connection to a server you run yourself. End-to-end encryption closes that, and
it is configured **on each participant's client**, not on the server. Nothing
here is read by `dioxusfun-server`.

| Var | Default | Meaning |
|---|---|---|
| `DISCORDIA_E2EE` | *(on)* | set `0`/`off`/`false`/`no` to disable media encryption entirely |
| `DISCORDIA_E2EE_KEY` | *(unset)* | a passphrase shared by hand; the developer path, superseded by a distributed channel key |
| `DISCORDIA_E2EE_OVERLAP` | *(off)* | set `1`/`on`/`true`/`yes` to overlap voice keys across a rekey — **unverified, see below** |

**`DISCORDIA_E2EE`** is the master switch, and it is on unless you turn it off.
The reason it exists is that a failed decryption is *silence*, not an error: if
audio or video misbehaves and you need to know whether encryption is the cause,
this is what removes it from the picture. With it off, the SFU carrying your
media can read it — which for a relayed session means the rendezvous operator,
not just you.

It also has a cost worth knowing before you leave it on. LiveKit disables Opus
**RED** (RFC 2198 packet-level redundancy) whenever encryption is configured, so
voice loses some resilience to packet loss. Opus in-band FEC still applies, so
this is a degradation rather than a cliff, but it is most noticeable on exactly
the lossy paths where encryption matters most. There is no setting that keeps
both — see `TODO.md` under Voice / audio.

**`DISCORDIA_E2EE_KEY`** is the manual path: every participant must be given the
same value out of band, and a mismatch produces **silence** — not noise, and not
an error. Measured rather than assumed: frames arrive and every sample is zero,
which is indistinguishable from someone who simply is not speaking. That is why
a wrong key is so expensive to diagnose, and why the switch above exists. It
exists for development and for verifying the mechanism. In normal
use it is unnecessary — the client distributes a per-channel key automatically,
sealed to each member against their Nostr identity, and a distributed key wins
over this variable. An empty value is treated as unset rather than as a
passphrase.

**`DISCORDIA_E2EE_OVERLAP`** changes how a rekey behaves. Keys roll when someone
is removed from a channel, and today every participant swaps keys at once, so
frames in flight across the changeover cannot be decrypted and the call
audibly stutters. With this on, voice publishes under a rotating key-ring slot
so the previous key stays loaded while the new one is adopted, and there is no
instant at which a frame has no key waiting for it.

**Leave it off unless you are testing it.** It depends on a libwebrtc behaviour
that cannot be verified from source in this repo, it has never been run between
two machines, and if the assumption is wrong the failure is a silent call rather
than a degraded one. It also only covers voice: screen and camera share a room
with the webview, which can hold only one key, so those still swap in place.

### Storage governance

- Per-guild **message retention** is set in-app (right-click guild → Server
  settings → Message retention). An hourly sweep deletes expired messages and
  reclaims disk (SQLite incremental vacuum).
- Message images are stored once, content-addressed, under
  `<data dir>/media/` — duplicate uploads cost nothing extra.

## Rung 3 — cluster (giants)

Deferred until a real community needs it — see `docs/ROADMAP.md` Phase 7
(Postgres + NATS + Redis behind a load balancer). The SQLite file from rungs
1–2 will be importable.
