# dioxusfun-server

WebSocket gateway, voice signaling, and LiveKit token minter for the
[dioxusfun](../dioxusfun) Discord clone.

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
cd ../dioxusfun && dx serve
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

- Text messaging, presence, voice signaling, voice token minting: in-memory.
- Voice media: libwebrtc end-to-end via LiveKit SFU.
- PostgreSQL persistence: not yet — `state/` is the swap point for a `sqlx`
  repository layer.
- Redis/NATS multi-node fanout: not yet — single-node `tokio::broadcast`.

## Protocol

`src/protocol/mod.rs` is duplicated at `dioxusfun/src/protocol/mod.rs`. Keep
both in sync until extracted into a shared crate.
