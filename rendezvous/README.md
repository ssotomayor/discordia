# dioxusfun-rendezvous

A small relay that lets self-hosted [dioxusfun](../dioxusfun) servers be
reached by shortcode (e.g. `purple-fox-42`) instead of IP address.

## How it fits

```
┌─────────────────┐   outbound ws       ┌──────────────────────────┐
│ Host's laptop   │  /control ──────────►                          │
│ self-hosting    │                     │  dioxusfun-rendezvous    │
│ dioxusfun       │  /proxy/{sid} ─────►│  (this binary)           │
│ + bundled       │                     │                          │
│ livekit-server  │                     │  (optional)              │
└─────────────────┘                     │   livekit-server (SFU)   │
                                        └────────▲─────────────────┘
                                                 │  /join/{shortcode}
                                                 │
                                        ┌────────┴─────────┐
                                        │ Friend's client  │
                                        └──────────────────┘
```

Frames sent by the friend are forwarded over the host's outbound proxy
WebSocket, so the host needs no port forwarding and shares no IP. The host
opens the proxy connection per-friend on demand.

## Run

```bash
cargo run
```

Listens on `0.0.0.0:7700`. Override via `DIOXUSFUN_RENDEZVOUS_ADDR=host:port`.

## Centralized voice (optional)

Set `LIVEKIT_URL` to tell hosts which LiveKit URL to hand back to clients
when they join voice channels. Run a LiveKit instance alongside the
rendezvous, with the same key/secret as the dioxusfun-server defaults
(`devkey` / `secret-must-be-at-least-32-chars-long`).

```bash
LIVEKIT_URL=wss://chat.example.com:7880 cargo run
```

Without this, the rendezvous tells hosts `livekit_url=None` and the gateway
falls back to per-connection host derivation (works for same-machine and LAN
testing).

## Endpoints

| Path | Direction | Use |
|------|-----------|-----|
| `GET /` | text | Sanity check |
| `WS /control` | host → rendezvous | Long-lived: register, receive NewFriend |
| `WS /join/:code` | friend → rendezvous | Join by shortcode; proxied to host |
| `WS /proxy/:session` | host → rendezvous | Per-friend bridge to local gateway |

## Protocol

JSON tagged enums on `/control`:

**Host → Rendezvous:**
```json
{ "op": "register", "d": { "name": null, "preferred": null } }
```

**Rendezvous → Host:**
```json
{ "op": "registered", "d": { "shortcode": "purple-fox-42", "livekit_url": null } }
{ "op": "new_friend", "d": { "session_id": "uuid-..." } }
{ "op": "error", "d": { "message": "..." } }
```

After the proxy WS pairing is established, frames flow through verbatim
(the rendezvous doesn't inspect them — it's pure transport).

## Status

- In-memory registry (no persistence — restarts wipe shortcodes)
- No auth: anyone with a shortcode can connect to that host
- No rate limiting
- Single-node (no horizontal scaling)

For a production deployment of any meaningful size, you'd want a Redis-backed
registry, per-shortcode auth tokens, and connection limits.
