# Ops

## Three ways to run it, same binary

| Rung | For | How |
|---|---|---|
| One click | friend groups | "Host my own" in the client — spawns a gateway and (unless the rendezvous has its own) an SFU in-process |
| One box | communities | `cargo run -p dioxusfun-server`, or Docker (`Dockerfile`, `docker-compose.yml` — what the test deployment runs) |
| Cluster | giants | not built (entry: demand-gated) |

## Environment

**Server**

| Var | Default | Meaning |
|---|---|---|
| `DIOXUSFUN_ADDR` | `0.0.0.0:9000` | plaintext gateway bind — for loopback and a TLS proxy; clients refuse `ws://` to anything else |
| `DIOXUSFUN_RELAY_URL` | — | an iroh relay (a rendezvous's `/config` names one) that introduces friends behind NAT and carries ciphertext when a punch fails |
| `DIOXUSFUN_DATA_DIR` | `./discordia-data` | SQLite, media blobs, `livekit-keys`, `quic-secret` (the key in the share string; back it up or friends re-add you) |
| `DIOXUSFUN_MEDIA_MAX_BYTES` | `2 GiB` | cap on `<data dir>/media`; uploads are refused past it |
| `DIOXUSFUN_OPERATORS` | — | comma-separated hex pubkeys who moderate system guilds |
| `DIOXUSFUN_PUBLIC_HOSTS` | — | comma-separated `host`, `host:port` or URLs clients dial (a DNS name, a reverse proxy). Loopback and every interface IP are always accepted; a login signed for any other address is refused |
| `LIVEKIT_URL` | derived per-connection | SFU URL handed to clients |
| `LIVEKIT_API_KEY` / `_SECRET` | generated into `<data dir>/livekit-keys` on first run | set only for an external SFU (`LIVEKIT_URL`); must match it |
| `LIVEKIT_PORT` | `7880` | port used when deriving the URL (the bundled SFU always binds 7880) |
| `DIOXUSFUN_LIVEKIT_AUTOSPAWN` | `1` | `0` when LiveKit runs separately |

**Rendezvous**

| Var | Default | Meaning |
|---|---|---|
| `DIOXUSFUN_RENDEZVOUS_ADDR` | `0.0.0.0:7700` | HTTP bind: `/control`, `/discover`, `/resolve`, `/config`, `/voice-token` |
| `DIOXUSFUN_RENDEZVOUS_DATA_DIR` | `./rendezvous-data` | persisted name reservations |
| `DIOXUSFUN_RENDEZVOUS_RELAY_ADDR` | `0.0.0.0:7701` | iroh relay bind, the one that carries ciphertext for hosts behind NAT |
| `DIOXUSFUN_RENDEZVOUS_RELAY_URL` | — | how clients reach that relay; handed out in `/config` and every entry |
| `LIVEKIT_URL` / `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` | — | a shared SFU for hosts without one; the rendezvous mints their tokens so no host holds the secret |

Serve it on the address it binds, not behind a reverse proxy: the per-address
limits and the check that a host's advertised address is its own both read the
peer's IP, and a proxy makes every host the same peer.

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

Every connection that leaves the machine is QUIC, authenticated by the key in
the `quic://key@addrs` share string the host banner copies, or in the
rendezvous entry. The plaintext gateway binds loopback only.

| Setup | You | LAN friend | Friend over the internet |
|---|---|---|---|
| Self-host, nothing else | loopback | share string with the LAN address (needs "accept direct connections") | **unreachable** |
| + rendezvous | loopback | punched, or carried by the relay | punched by the relay, or carried by it |
| + port mapping (UPnP/NAT-PMP) | loopback | direct | **direct** on the forwarded UDP port |
| Community server | loopback or `wss://` proxy | QUIC by share string | QUIC by share string, or `wss://` through a TLS proxy (`DIOXUSFUN_PUBLIC_HOSTS`) |

Port mapping failure is the normal case and never stops hosting. It also
measures hairpin NAT, because LiveKit *replaces* its LAN candidate with the
advertised address rather than adding to it.

## Deploying a box

CI builds the two images on every push to `master` (`deploy-images` in
`ci.yml`, gated on `test`) and pushes them to
`ghcr.io/<owner>/discordia-{server,rendezvous}`, tagged `latest` and the short
sha. **Never build on the deployment box**: the server crate needs more RAM than
a small VPS has, and the OOM killer picks among the containers already running.

Updating is then a pull, from a directory holding only `docker-compose.yml`,
`deploy/livekit.yaml` and `.env`:

```bash
cd /opt/discordia
docker compose pull && docker compose -p discordia up -d
```

Rolling back is the same command against an older sha in the image tag. Keep
the deploy directory off any checkout an agent or a timer writes to — a compose
file read out of a working tree is whatever revision that tree last held.

| Trap | Why |
|---|---|
| `-p discordia` | volumes are project-scoped; a different project name orphans `discordia_*` and the box comes up empty |
| `.env` is required | compose uses `${LIVEKIT_API_KEY:?}`, so a missing key fails at start rather than falling back to LiveKit's public `devkey` |
| All three restart together | the gateway, the rendezvous and the SFU must agree on the LiveKit pair |
| `deploy/livekit.yaml` carries no `keys:` | `LIVEKIT_KEYS` supplies them, so a copied file never carries a secret |
| Off loopback the gateway needs QUIC reachable | host networking today, `DIOXUSFUN_RELAY_URL` so a blocked UDP port still connects (issue #151) |

## Devcontainer

A Linux box holding nothing but this repo, for agent sessions run with
`--dangerously-skip-permissions`. `.devcontainer/`, one comment per decision.

```bash
devcontainer up --workspace-folder .
devcontainer exec --workspace-folder . bash     # then: claude --dangerously-skip-permissions
devcontainer up --workspace-folder . --remove-existing-container   # after editing .devcontainer/
```

| Thing | How | Note |
|---|---|---|
| Egress | `init-firewall.sh`, re-applied every start | default deny; a new upstream host needs a line **and an image rebuild** |
| Commit + push | the host's SSH agent forwarded to `/ssh-agent` | no key crosses; commits are SSH-signed, not OpenPGP |
| `.git/config`, `.git/hooks`, `.cargo` | read-only binds | so a hostile `build.rs` cannot make *host* git run code. Costs `git push -u` — push with `git push origin HEAD` |
| Claude skills, hooks, memory, transcripts | bound from `~/.claude` | credentials are **not** — `claude /login` in the container |
| `/effort` | fails with `EBUSY` | `~/.claude/settings.json` is a read-only *single-file* bind. `rename()` over a mountpoint is EBUSY whatever the mode, so `:rw` would not help — the `:ro` is what stops the agent disarming its own deny list and guard hook. Use `claude --effort <level>`, or `effortLevel` in `.claude/settings.local.json` |
| `target/` | named volume | the host's is 50GB of Mach-O |
| `dx` | pinned to `DIOXUS_CLI_VERSION` in `ci.yml` | bump both, plus the literal in `windows-release.yml` |
| Client | compiles, never runs | no display for wry, no audio device, screen capture is Windows/macOS-only |
| SFU | `LIVEKIT_BUNDLE_SKIP=1`, autospawn off | unset to build a server that embeds one |

## Who can read what

| Party | Control | Voice / video |
|---|---|---|
| Network path | encrypted (QUIC, keyed to the host) | encrypted (DTLS-SRTP) |
| Rendezvous relay, when used | ciphertext only — it sees the two keys and the timing | **readable** by its SFU, unless E2EE is on |
| TLS proxy in front of a community server | **readable** by the proxy's operator | encrypted |
| Host | readable | readable |
| LAN friend | encrypted | encrypted |
| Bots | loopback or the TLS proxy only; the SDK has no QUIC | — |

DMs are in none of these rows: they are Nostr gift wraps on relays, and a relay
learns only that somebody messaged you.
