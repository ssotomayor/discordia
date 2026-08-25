# Map

Where things are, so a reader greps once instead of three times. `CLAUDE.md`
says what the parts *do* and which invariants bind them; this says where to
open and in what order. Line counts are the reason it exists: six files hold a
third of the tree, and reading one whole to find one arm is the usual waste.

## Read whole vs grep

| Size | Files | How |
|---|---|---|
| < 300 lines | most of `client/src/nostr/*`, `client/src/{identity,session,settings,version,quic,portmap}.rs`, `protocol/src/rendezvous.rs` | read whole |
| 300–800 | `client/src/{state,app,net}.rs`, `client/src/features/{home,connect,guilds,workspace,chat}.rs`, `server/src/{store,http,auth}.rs` | grep to a symbol, then read the block |
| > 900 | `client/src/features/{voice,channels,screenshare}.rs`, `server/src/state/mod.rs`, `server/src/gateway/connection.rs`, `protocol/src/lib.rs` | **never read whole** — grep the variant or `fn` name |

## Entry points

| To find | Open | At |
|---|---|---|
| What a `ServerMessage` does to the client | `client/src/net.rs` | `fn apply` (~503) — one arm per variant, exhaustive |
| What the server does with a `ClientMessage` | `server/src/gateway/connection.rs` | `handle_connection` (15); 53 `ClientMessage::` arms follow |
| Every wire type | `protocol/src/lib.rs` | 74 variants; grep the name, the file is 950 lines |
| Server state mutation + permissions | `server/src/state/mod.rs` | methods on `AppState`; all async, all write through `persist(…)` |
| Client state + advisory `can()` | `client/src/state.rs` | `AppState`, `use_app_state`, `use_gateway` |
| DMs end to end | `client/src/nostr/service.rs` | `spawn_nostr` (84); `conversation_id` (64) is the Uuid derivation |
| Voice, capture, mixing | `client/src/features/voice.rs` | 2.7k lines — grep `ScreenAudioRoom`, `ScreenVideoRoom`, `forward_mic` |
| The first screen | `client/src/features/home.rs` | `HomeView`; the connect form is `connect::ConnectForm` |

## Change recipes

Ordered file lists. Trap 1 in `CLAUDE.md` is the protocol one and is not
repeated here.

| Task | Touch, in order |
|---|---|
| New UI surface | `client/src/features/<new>.rs` → `client/src/features/mod.rs` → mount in `features/workspace.rs` (in a session) or `features/home.rs` (before one) |
| New Tailwind class | write it → `dx build --package dioxusfun` → commit `client/assets/tailwind.css` (trap 13) |
| New Nostr event kind | `client/src/nostr/<kind>.rs` → `client/src/nostr/mod.rs` → subscribe/handle in `nostr/service.rs` → new field in `client/src/state.rs` |
| New server permission | `protocol/src/lib.rs` (`Permission`) → `server/src/state/mod.rs` (`can`) → the handler arm in `gateway/connection.rs` → `client/src/state.rs` `can()` for hiding UI |
| A name shown anywhere | never store it — `AppState::display_name` (trap 8) |
| Deferred work | `docs/OPEN.md`, never only a commit message |

## Tests

| Kind | Where | Note |
|---|---|---|
| Wire, end to end | `server/tests/*.rs` | spawn a real gateway, drive it through the bot SDK; copy a helper block |
| Partial failure | `server/tests/voice.rs` | `ScriptedMinter` answers per request — the delegation seam doubles as a fault injector |
| Platform paths | `client/tests/live_sfu.rs`, `#[ignore]`d unit tests | `cargo test -p dioxusfun -- --ignored`; need an SFU, an audio device or a screen grant |
| Everything else | beside the code | `cargo test --workspace` stays headless and green |

## Not in this repo

Messages in memory (DB only, trap 2) · a browser client (P3) · cluster mode
(P7) · issue tracker (`docs/OPEN.md` is it).
