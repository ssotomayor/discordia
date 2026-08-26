# Map

Where things are, so a reader greps once instead of three times. `CLAUDE.md`
carries what every session needs — the rules and the invariants; this carries
what only some do: where to open, what moves together, how the parts connect. It exists because a handful of files hold most of the tree,
and reading one whole to find one arm is the usual waste.

Symbols, never line numbers, and no hand-kept list that a commit can falsify:
a symbol can be grepped and a number cannot, and this file shipped three stale
numbers and seven miscategorised files before that rule was learned.

## Do not read these whole

10 files hold most of the tree. Opening one to find a single arm costs more
than every other document here put together, so grep the variant or the `fn`
name instead.

| File | Lines |
|---|---|
| `client/src/features/voice.rs` | 2726 |
| `server/src/state/mod.rs` | 2331 |
| `server/src/gateway/connection.rs` | 1938 |
| `client/src/features/channels.rs` | 1921 |
| `client/src/features/screenshare.rs` | 1582 |
| `client/src/net.rs` | 1145 |
| `client/src/features/chat.rs` | 971 |
| `protocol/src/lib.rs` | 952 |
| `server/src/store.rs` | 942 |
| `client/src/update.rs` | 905 |

Everything else is small enough that `wc -l` answers faster than a list here
could stay true. There used to be rows for "under 300" and "300 to 800": they
were wrong about seven files across three review rounds, and being wrong in
that direction says a file is safe to open when it is not.

## Entry points

| To find | Open | At |
|---|---|---|
| What a `ServerMessage` does to the client | `client/src/net.rs` | `fn apply` — one arm per variant, exhaustive |
| What the server does with a `ClientMessage` | `server/src/gateway/connection.rs` | `handle_connection`, then ~50 `ClientMessage::` arms |
| Every wire type | `protocol/src/lib.rs` | grep the variant name; ~70 of them |
| Server state mutation + permissions | `server/src/state/mod.rs` | methods on `AppState`; all async, all write through `persist(…)` |
| Client state + advisory `can()` | `client/src/state.rs` | `AppState`, `use_app_state`, `use_gateway` |
| DMs end to end | `client/src/nostr/service.rs` | `spawn_nostr`; `conversation_id` is the Uuid derivation |
| Voice, capture, mixing | `client/src/features/voice.rs` | the largest file in the tree — grep `ScreenAudioRoom`, `ScreenVideoRoom`, `forward_mic` |
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
| Platform paths | `client/tests/live_sfu.rs`, `#[ignore]`d unit tests | need an SFU, an audio device or a screen grant — hence ignored, not optional |
| Everything else | beside the code | the suite stays headless and green |

## Architecture

```mermaid
flowchart LR
  subgraph C["client/ — dioxusfun"]
    UI["features/*.rs"]
    NET["net.rs — WS loop<br/>apply / send"]
    ST["state.rs — AppState<br/>+ advisory can()"]
    NOSTR["nostr/ — DMs<br/>NIP-17/44/59 on relays"]
    V["features/voice.rs<br/>native LiveKit + cpal mixer"]
    CAP["sysaudio/ · sysvideo/ · rawmic/<br/>native capture"]
    JS["screenshare.rs · camera.rs<br/>webview LiveKit JS"]
    HOST["host.rs · portmap.rs · quic.rs"]
    UI-->NET-->ST
    UI-->NOSTR
    CAP-->V
    V-.->JS
  end
  subgraph S["server/ — dioxusfun-server"]
    GW["gateway/connection.rs<br/>one task per socket"]
    AS["state/mod.rs — AppState<br/>DashMaps, authoritative"]
    DB[("store.rs — SQLite<br/>write-through")]
    MED["media.rs — blobs"]
    LK["livekit.rs — tokens"]
    GW-->AS-->DB
    GW-->MED
    GW-->LK
  end
  SFU["LiveKit SFU<br/>voice-{ch} · screen-{ch}"]
  RZ["rendezvous/<br/>/control · /join · /proxy · /discover"]
  RELAYS[("Nostr relays")]
  BOT["bot-sdk"]
  NET<-->|"WS /gateway or QUIC<br/>Schnorr Identify"|GW
  BOT<-->|filtered stream|GW
  V<-->SFU
  JS<-->SFU
  LK-.mint.->SFU
  HOST<-->RZ
  NOSTR<-->RELAYS
```

## Not in this repo

Messages in memory (DB only, trap 2) · a browser client (P3) · cluster mode
(P7) · issue tracker (`docs/OPEN.md` is it).
