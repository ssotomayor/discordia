# Self-hosting Discordia

Three rungs, by community size. All three run the same code and the same
on-disk format — you can graduate by copying one directory.

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
| `LIVEKIT_PORT` | `7880` | port the bundled LiveKit is spawned on / derived URLs use |
| `DIOXUSFUN_LIVEKIT_AUTOSPAWN` | `1` | set `0` when LiveKit runs separately |

Compose-only: `PUBLIC_HOST` (see `.env.example`) is read by
`docker-compose.yml`, not by the server — it builds the `LIVEKIT_URL` above.

Client-side, for reference: `DIOXUSFUN_CONFIG_DIR` relocates the identity, dev
log and settings, and `DIOXUSFUN_RENDEZVOUS_URL` presets the rendezvous the
Self-host tab offers. Build-time, `LIVEKIT_BUNDLE_SKIP=1` skips fetching the
LiveKit binary — useful for CI, but the resulting build cannot host voice.

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
