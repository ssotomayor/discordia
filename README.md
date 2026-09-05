# Discordia

[![CI](https://github.com/ssotomayor/discordia/actions/workflows/ci.yml/badge.svg)](https://github.com/ssotomayor/discordia/actions/workflows/ci.yml)

A self-hostable Discord alternative where **your keypair is your account**. No
signup, no password, no e-mail: identity is a Nostr (BIP-340 secp256k1 Schnorr)
keypair you hold, so the same identity works on every instance and no operator
can take it from you.

Text and voice channels, guilds with roles and moderation, screen sharing with
system audio, cameras, bots, and DMs that no server can read. Rust workspace,
native desktop client.

> **Pre-release.** Persistence, moderation, voice, screen sharing, bots and
> guild export/import work and are covered by the suite. No web client, no
> hosted instance. `docs/OPEN.md` is the honest list.

## What is actually yours

| | Who holds it |
|---|---|
| Identity | you — a keypair, importable as `nsec`, revocable by nobody |
| DMs | your key. NIP-17 gift wraps on Nostr relays, NIP-44 encrypted, never through the gateway. They follow you to another instance or any Nostr client. |
| Guild data | whoever runs the server. `export`/`import` moves it with pubkeys intact. |

Self-hosted is not decentralised, and the project is deliberate about that: the
decentralisation is portable identity plus independent instances. A community's
data is centralised with its operator by design.

## Quick start

```bash
cargo install dioxus-cli
dx serve --package dioxusfun     # then: Create a server → Launch
```

That spawns a gateway and a bundled LiveKit in-process — you are the operator of
your own Lobby, and friends join with a code that does not show your IP. Data
lands in `<config dir>/dioxusfun/host-data/`.

Server, relay, and guild migration: `docs/OPS.md`.

## Build and test

```bash
cargo build --workspace
cargo test --workspace                  # must stay green and headless
cargo test -p dioxusfun -- --ignored    # platform paths (SFU, audio device, screen grant)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

First run is slow: `server/` fetches or builds `livekit-server` once (macOS
builds from source, needs `go`). `LIVEKIT_BUNDLE_SKIP=1` opts out.

`devcontainer up --workspace-folder .` builds a Linux container that can do all
of the above except run the client — `docs/OPS.md`.

On macOS `dx serve` cannot relax App Transport Security for the webview, so
screen share and camera reach no self-hosted SFU under it; `cargo run` and the
bundle both carry the `Info.plist` that does.

Packaging the macOS app needs a signing identity, or macOS treats every build
as a new app and re-asks for the screen, mic and camera grants. It is
per-developer, so it is passed in rather than committed to `Dioxus.toml`:

```bash
DISCORDIA_SIGNING_IDENTITY="Apple Development: You (TEAMID)" ./bundle-macos.sh
security find-identity -v -p codesigning   # lists candidates
```

## Verifying a download

Every artifact ships a `.minisig` signed by CI, against
`release-signing.pub` in this repo.

```bash
minisign -Vm Discordia-windows-setup.exe -p release-signing.pub
```

The trusted comment names the tag and filename and is covered by the signature,
so a signature cannot be lifted onto another release's artifact. This says the
file is the one CI built, unmodified — it is **not** an OS signature, so
SmartScreen and Gatekeeper still warn on first launch (entries 9, 10).

Maintainers: `minisign -G -W -p release-signing.pub -s minisign.key`, commit the
`.pub`, put the key's contents in the `MINISIGN_SECRET_KEY` secret, delete the
local copy. It is recoverable only by rotating, and rotation has a bootstrap
problem — a new public key travels in an update the old key signs.

## Contributing

Fork, branch, PR. Keep `cargo test --workspace`, clippy `-D warnings` and
`cargo fmt --all --check` green. Read `CLAUDE.md` first — it is the orientation
and its rules are binding, including the one on comments. Deferred work goes in
`docs/OPEN.md`.
