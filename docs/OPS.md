# Ops

## Three ways to run it, same binary

| Rung | For | How |
|---|---|---|
| One click | friend groups | "Host my own" in the client — spawns a gateway and (unless the rendezvous has its own) an SFU in-process |
| One box | communities | `cargo run -p dioxusfun-server`, or Docker (`Dockerfile`, `docker-compose.yml` — untested, entry 15) |
| Cluster | giants | not built (entry: demand-gated) |

## Environment

**Server**

| Var | Default | Meaning |
|---|---|---|
| `DIOXUSFUN_ADDR` | `0.0.0.0:9000` | gateway bind |
| `DIOXUSFUN_DATA_DIR` | `./discordia-data` | SQLite + media blobs |
| `DIOXUSFUN_OPERATORS` | — | comma-separated hex pubkeys who moderate system guilds |
| `LIVEKIT_URL` | derived per-connection | SFU URL handed to clients |
| `LIVEKIT_API_KEY` / `_SECRET` | dev defaults | must match the SFU |
| `LIVEKIT_PORT` | `7880` | port used when deriving the URL (the bundled SFU always binds 7880) |
| `DIOXUSFUN_LIVEKIT_AUTOSPAWN` | `1` | `0` when LiveKit runs separately |

**Rendezvous**

| Var | Default | Meaning |
|---|---|---|
| `DIOXUSFUN_RENDEZVOUS_ADDR` | `0.0.0.0:7700` | relay bind |
| `DIOXUSFUN_RENDEZVOUS_DATA_DIR` | `./rendezvous-data` | persisted name reservations |

**Client**

| Var | Default | Meaning |
|---|---|---|
| `DIOXUSFUN_CONFIG_DIR` | OS config dir | identity, settings, release log |
| `DIOXUSFUN_RENDEZVOUS_URL` | — | presets the rendezvous |
| `DISCORDIA_E2EE` | on | `0`/`off` disables media encryption |
| `DISCORDIA_E2EE_KEY` | — | passphrase shared by hand; developer path |
| `DISCORDIA_E2EE_OVERLAP` | off | overlap voice keys across a rekey — **unverified** |

Guild migration: `cargo run -p dioxusfun-server -- export --guild <uuid> f.json`
then `import f.json` on the target. Fresh ids, pubkeys preserved.

## Reachability

| Setup | You | LAN friend | Friend over the internet |
|---|---|---|---|
| Self-host, nothing else | loopback | direct to LAN IP (needs `allow_lan`) | **unreachable** |
| + rendezvous | loopback | direct | control relayed; media via the relay's SFU |
| + port mapping (UPnP/NAT-PMP) | loopback | direct | **direct** |
| + QUIC coordinator | loopback | direct | **direct**, punched, no public address needed |

Port mapping failure is the normal case and never stops hosting. It also
measures hairpin NAT, because LiveKit *replaces* its LAN candidate with the
advertised address rather than adding to it.

## Who can read what

| Party | Control | Voice / video |
|---|---|---|
| Network path | readable on `ws://`, encrypted on QUIC | encrypted (DTLS-SRTP) |
| Rendezvous relay, when used | **readable** | **readable** — its SFU decrypts and re-encrypts |
| Host | readable | readable |
| LAN friend | **readable** — `allow_lan` is plaintext | encrypted |

DMs are in none of these rows: they are Nostr gift wraps on relays, and a relay
learns only that somebody messaged you.
