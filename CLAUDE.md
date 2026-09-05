# Discordia

Self-hostable Discord clone with Nostr identity. Rust workspace, Dioxus desktop
client, axum WebSocket server. Text/voice/DMs, guilds, roles, bots, optional
decentralisation.

## Rules

**Comments — only *why*, never *what*.**

- The code already says what it does. A comment restating it is a second copy to
  keep true. Say nothing.
- Write one only when the reason is unrecoverable from the code: a decision that
  looks wrong until you know the constraint, a load-bearing ordering, a trap.
- **Max 2 lines.** Needs more? It is a `docs/OPEN.md` entry, not a comment.
- Delete rather than update a comment you cannot justify in one sentence.

**Navigating — open `docs/MAP.md` before the second grep.** Where a thing
lives, what changes with it, and which files must not be read whole. Cheaper
than the search it replaces, and it says which of them cost 20k tokens to open.

**Before merging, re-read the docs against what changed.** A path, a symbol, a
file size band or a trap number in `docs/MAP.md` is a claim about the tree, and
moving code is what makes one false.

**Docs — terse.** Prefer a table or a mermaid chart to prose. No file restates
another. Deferred work goes in `docs/OPEN.md`, never only in a commit message.

**No Claude/AI attribution** in commits, PRs, or generated content.

## Crates

| Crate | Package | Role |
|---|---|---|
| `protocol/` | `dioxusfun-protocol` | Wire types. Single source of truth for `ClientMessage`/`ServerMessage`. |
| `server/` | `dioxusfun-server` | axum gateway: WS protocol, state, SQLite, media blobs, LiveKit, export/import. |
| `client/` | `dioxusfun` | Dioxus 0.7 desktop app (wry). Also the self-host path — embeds the server in-process. |
| `rendezvous/` | `dioxusfun-rendezvous` | Discovery + NAT relay: hosts register, friends join by code, frames proxied. |
| `bot-sdk/` | `dioxusfun-bot` | Bot client library; integration tests drive the server through it. |
| `grid-layout/` | `dioxus-grid-layout` | Draggable/resizable grid widget. Self-contained. |

## Traps

| # | Invariant |
|---|---|
| 1 | A protocol change ripples: `protocol/src/lib.rs` → server arm in `gateway/connection.rs` → client sender in `features/*.rs` → client arm in `net::apply` → `server/tests/*`. `#[serde(default)]` on new optional fields. |
| 2 | Server memory is authoritative; SQLite is write-through and a failed write is logged and ignored. **Messages live only in the DB**, fetched on demand. |
| 3 | Every uploaded picture — message, avatar, banner, guild icon, emoji — becomes a `media:<sha256>.<ext>` sentinel on disk; rows, history, snapshots and broadcasts carry the sentinel, never bytes. A client resolves one with `FetchEmoji`, the blob fetch for every kind of image. |
| 4 | Fan-out is `deliver(pubkeys, msg)` against a routing table, not `broadcast`. A full outbound queue drops the connection (typing is dropped instead); the client reconnects and re-snapshots. `identify_conn` runs *before* the Ready snapshot. Sockets are capped in total, per address and per key (`state::MAX_*`). |
| 5 | The server re-checks every permission. Client `can()` only hides dead-end UI. |
| 6 | `key` on a lone component does nothing — Dioxus reads it only in a list. `App` renders `WorkspaceView` inside a one-element `for` so a session change remounts it. |
| 7 | DMs never touch the gateway. `ConnectionStatus::Offline` is a state the app *runs in*; home works with no server. |
| 8 | A person's name is resolved at render, never stored: roster → petname → kind 0 → truncated key. `DmInfo` holds a pubkey for that reason. |
| 9 | A relay subscription is replaced by its id, so `RelayPool::subscribe` takes one. The contact list is replaceable — read-modify-write it whole. |
| 10 | One user joins `screen-{ch}` under up to 3 identities (LiveKit allows one connection each): bare = webview (renders all, captures screen on Windows, publishes camera everywhere), `#audio` = native subscriber, `#video` = native macOS screen publisher. Watchers resolve the suffix in `attach`/`reattach` only. |
| 11 | Native share publications are owned by `ScreenShareBridge`'s effect, keyed on voice epoch + target — not by the button. Anything ending a share must clear `screen_share_target`. |
| 12 | Identity is a BIP-340 keypair; your key is your account. The login signature covers the address the client dialed (`protocol::dial_origin`), and a server accepts only addresses in its identity set: loopback and interface IPs, `DIOXUSFUN_PUBLIC_HOSTS`, and what a self-host registered. `bot` and `client_version` in `Identify` are self-declared and unauthenticated by design. |
| 13 | Tailwind is dx's, not npm's: it finds `client/tailwind.css` at the crate root, installs a standalone CLI, and writes `client/assets/tailwind.css` — which is committed because a plain `cargo build` has no dx to generate it. |
| 14 | CI lints with `-D warnings`: one warning is a red build. It lints in two groups — `dioxusfun`+`dioxus-grid-layout`, and `protocol`+`server`+`bot`+`rendezvous` — so `-p dioxusfun` alone misses half the tree. |
| 15 | Off loopback the gateway speaks QUIC only, keyed to the host (`quic.rs` both sides, `quic://key@addrs` share strings). `ws://` is loopback or behind a TLS proxy and the client refuses it elsewhere; the rendezvous never proxies frames, its iroh relay carries ciphertext. A friend's peer address arrives as `ConnectInfo` on direct QUIC and is absent when relayed. |

## Elsewhere

| For | Read |
|---|---|
| Running, hosting, env vars, reachability, the devcontainer | `docs/OPS.md` |
| Deferred work — the only tracker | `docs/OPEN.md` |
| Build and test commands, contributing | `README.md` |
