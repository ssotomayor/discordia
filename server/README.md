# dioxusfun-server

WebSocket gateway, voice signaling, and LiveKit token minter for the
[dioxusfun](../client) Discord clone.

## Components

| Process | What it does | Port |
|---|---|---|
| `dioxusfun-server` | Gateway WS (text + voice signaling), mints LiveKit access tokens | 9000 |
| `livekit-server` (separate, in Docker) | libwebrtc SFU. Handles all voice media (audio frames, AEC, NS, AGC, congestion control). Clients connect directly here once they have a token. | 7880 (WS), 7881 (TCP), 7882/udp |

## Run

### 1. Start LiveKit (the SFU)

```bash
docker run --rm \
  -p 7880:7880 -p 7881:7881 -p 7882:7882/udp \
  -e LIVEKIT_KEYS="devkey: secret-must-be-at-least-32-chars-long" \
  livekit/livekit-server --dev --bind 0.0.0.0
```

`--dev` accepts the `devkey`/`secret-must-be-at-least-32-chars-long` credentials
matching `LiveKitConfig::from_env()` defaults below.

### 2. Start dioxusfun-server

```bash
cargo run
```

Listens on `ws://0.0.0.0:9000/gateway`. Override the bind with
`DIOXUSFUN_ADDR=host:port`.

### 3. Start the Dioxus client

```bash
cd ../client && dx serve
```

Connect to `ws://localhost:9000` from the connect screen. Joining a voice
channel will fetch a LiveKit access token from this server and the client will
open a peer connection directly to LiveKit at `ws://localhost:7880`.

## LiveKit configuration

Read at startup from env, with `--dev` defaults:

| Var | Default | Notes |
|---|---|---|
| `LIVEKIT_URL` | `ws://localhost:7880` | Sent to clients; must be reachable from them too |
| `LIVEKIT_API_KEY` | `devkey` | Must match a key configured on the LiveKit server |
| `LIVEKIT_API_SECRET` | `secret-must-be-at-least-32-chars-long` | Used to sign JWTs |

For production: generate a strong secret (≥32 chars), put it in env, do NOT
commit, and run LiveKit with proper TURN configuration for NAT traversal.

## Bundled / standalone mode

The client (`dioxusfun`) depends on this crate as a library. When a user picks
**Self-host** on the connect screen, the client spawns this server in-process
on the next free port starting at `9000` and launches the bundled
`livekit-server` as a subprocess on `7880`. No separate binaries needed.

This binary (`cargo run`) is for shared/remote deployments where the gateway
runs centrally.

## Status

- Text messaging, presence, voice signaling, voice token minting: live state in
  `state/` (DashMaps), written through to SQLite by `store.rs` and rehydrated on
  boot. Messages are the exception — they live only in the DB.
- Voice media: libwebrtc end-to-end via LiveKit SFU.
- PostgreSQL: not yet. The `sqlx` repository layer exists (`store.rs`); only the
  SQLite implementation is written, and `Store` is where a Postgres one would go.
- Redis/NATS multi-node fanout: not yet — single-node, and fan-out is a
  per-connection routing table (`AppState::deliver`), not a broadcast channel.

## Protocol

The wire protocol lives in the standalone **`dioxusfun-protocol`** crate. The
server (and the desktop client, and the bot SDK) all re-export it, so there is
a single source of truth — no duplication to keep in sync.

## Bots (Tier 1)

A bot is an external WS client identified by a secp256k1 Schnorr (BIP-340 /
Nostr) pubkey — the same identity primitive users have, so there's no bearer
token to leak. The guild
**owner** installs a bot by its pubkey (`InstallBot`), granting:

- **Permissions** — what it may *do*: `send_messages`, `read_message_history`,
  `add_reactions`.
- **Intents** — what events it *receives*. `guild_messages` delivers message
  events; `message_content` (privileged) is required to receive the actual
  text — otherwise it's blanked. `members` (privileged) delivers the roster /
  join-leave events.

The gateway enforces both: a bot connection only receives events for its
installed guilds, filtered by intent, and its actions are permission-checked.
All connections are rate-limited. Write bots with the `dioxusfun-bot` crate
(`cargo run -p dioxusfun-bot --example ping`). See `tests/bots.rs` for the
end-to-end behavior.
