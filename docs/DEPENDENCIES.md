# Client dependencies

Baseline: `master` at `e90d3ff`. Counts exclude the application; a package at
two versions counts twice. Normal + build dependencies, default client features.

| Stage | Windows x64 packages |
|---|---:|
| Baseline | 672 |
| Limit desktop image formats | 635 |
| Disable redundant logger; opt-in developer tools | 632 |
| Share desktop Tungstenite with LiveKit | 631 |

macOS ARM64: 688 → 646. Windows duplicate names: 52 → 51; extra versions:
68 → 66. `Cargo.lock` covers optional features, other platforms and tests;
its entry count is not the client build count.

| Change | Preserved behavior |
|---|---|
| `vendor/dioxus-desktop/PATCHES.md` | PNG/JPEG/GIF/WebP, Windows clipboard BMP, PNG/ICO icons, transparent windows, Tokio |
| Explicit Dioxus features | Launch, UI, signals/hooks, document, assets, warnings, CLI configuration |
| App-owned logging | Existing log file, panic reports and Diagnostics panel |
| `--features devtools` | Dioxus hot reload and developer tools |
| Workspace LiveKit API 0.5 | Server/coordinator token minting and SFU administration; client grant tests |
| WebRTC build-helper pin in build dependencies | Exact SDK-compatible version without compiling the helper as a runtime library |
| `winresource` build dependency | Embeds Windows executable identity and icon; no runtime dependency |

Remaining Reqwest, Tokio-Tungstenite, Rand and Windows versions are constrained
by upstream SDKs/frameworks or incompatible APIs. No forced version overrides.
Noise suppression, networking, media and the embedded server stay enabled.

Reproduce counts:

```sh
cargo tree -p discordia --locked --target x86_64-pc-windows-msvc -e normal,build --prefix none --format '{p}'
cargo tree -p discordia --locked --target aarch64-apple-darwin -e normal,build --prefix none --format '{p}'
cargo tree -p discordia --locked --target x86_64-pc-windows-msvc -e features -i image
```

Deduplicate package/version pairs and exclude `discordia`. Feature checks must
use default builds; `--all-features` deliberately enables developer tools.
Use `cargo build --timings` for timing comparisons with the same toolchain,
target, profile and cache state. Package reductions do not imply the same
percentage reduction in build time; no comparable cold-build timing is claimed.

| Validation on Windows | Result |
|---|---|
| Workspace tests, including name-registration regressions | 673 passed, 19 ignored, 0 failed |
| Workspace Clippy, all targets | `-D warnings` passed |
| Client/grid Clippy, debug assertions off | `-D warnings` passed |
| Client check, developer tools enabled | All targets passed |
| Three WebView bridge suites | Passed |
| Formatting and diff checks | Passed |
| Dioxus desktop build | Passed; 943 compilation units |

macOS dependency resolution was checked; native macOS compilation and runtime
verification require macOS.

## Iroh transport update

| Item | Result |
|---|---|
| Workspace Iroh / iroh-relay | 1.0.3 → 1.3.0; requires Rust 1.91+ |
| Production transport code | Unchanged: keys, ALPN, direct/relay selection, session recovery, SFU signaling tunnel |
| Isolated stream interoperability | 12 loopback combinations passed: hosts/clients 1.0.3 and 1.3.0, direct or relays 1.0.3 and 1.3.0; does not measure WAN/NAT behavior |
| Relay recovery regression | IP transports disabled; the same QUIC connection exchanges data before and after the local relay restarts |
| SFU proxy regression | LiveKit-style WebSocket signaling roundtrips over an authenticated QUIC stream |

Compatibility probes were built separately under ignored `target/iroh-compat`,
using the pre-update and current lockfiles. Audio, video and UI SDK versions
remain unchanged; QUIC, DNS and network-discovery dependencies move with Iroh.

## Browser media SDK

LiveKit JavaScript 2.19.2 → 2.22.4. Both bundled files come from the official
[`livekit-client` npm package](https://registry.npmjs.org/livekit-client/-/livekit-client-2.22.4.tgz).
The separate `e2ee-worker-shim.js` remains applied by `AppHead`.

| Artifact | SHA-256 of upstream bytes |
|---|---|
| `client/assets/livekit-client.umd.js` | `c7635b98785139085e0c30105404af894551d0870e19f06f64951f809812bd4a` |
| `client/assets/livekit-client.e2ee.worker.js` | `dc707d8e2b2174a92f3f8152e64a29d23cc9817a30f7169697f0a546999999cc` |

`node client/tests/e2ee_worker.cjs` runs the bundled worker with the shim,
real WebCrypto and synthetic Opus/H.264 frames. Checks encrypted roundtrips,
key-before-enable ordering and key isolation, including ratchet responses.
`LIVEKIT_REFERENCE_WORKER=<previous worker file>` adds old/new checks in both
directions. This checks encrypted bytes, not real decoding, browser playback
or stream FPS. Existing screen/camera/audio bridge suites remain required.

## Native media SDK

| Component | Previous | Current |
|---|---|---|
| `livekit` | 0.7.53 | 0.9.5 |
| `libwebrtc` | 0.3.42 | 0.3.52 |
| `webrtc-sys` | 0.3.39 | 0.3.49 |
| `webrtc-sys-build` | 0.3.18 | 0.3.19 |
| WebRTC artifact | `webrtc-51ef663` | `webrtc-89d790b` |

`livekit-common` 0.1.5, `livekit-data-stream` 0.1.8,
`livekit-datatrack` 0.2.3 and `livekit-protocol` 0.8.1 follow the upstream
SDK manifest. Sources come from crates.io at upstream revision
`49b3ebe9ab1c574114b164868ad226cc26a9b1be`; local changes are in each vendored
crate's `PATCHES.md`. Server token/admin API, SFU 1.13.9 and capture 2.0.1 remain
unchanged. ICE configuration construction follows the new non-exhaustive API.

Linux's C++ bridge must share the artifact's hermetic libc++ ABI. CI and the
devcontainer select Clang 21 from [LLVM's repository](https://apt.llvm.org/)
through `scripts/install-webrtc-toolchain.sh`, with `CC=clang-21` and
`CXX=clang++-21`. Other distributions need an equivalent compatible compiler.

| Windows validation | Result |
|---|---|
| Workspace tests | 1081 passed, 37 ignored |
| Workspace Clippy | `--all-targets -- -D warnings` passed |
| Real encrypted native video | 1080p60 NVIDIA H.264, VP8 software and CPU H.264 decoded successfully |
| Real SFU regressions | Camera/screen independent stop, encrypted audio RED and gateway signaling passed |
| Synthetic motion, local SFU, encrypted H.264, RTX 5080 | 1440p30/12 Mbps: previous 30 FPS, 884 decoded frames, 7.34 ms/encode; updated 30 FPS, 883 frames, 6.80 ms/encode |
| Synthetic motion, same host/codec/SFU, production `ultra` settings | 1440p60/34 Mbps: previous 60 FPS final, 59.09 decoded FPS average, 6.88 ms/encode; updated 60 FPS final, 59.36 decoded FPS average, 6.74 ms/encode |

Motion values are one debug-build run per version using the same existing
30-second changing-texture test, source, encoder settings and host. They exclude
screen capture, remote networks and perceptual quality measurements; no FPS gain
across qualities is established. Native macOS/Linux runtime validation needs
those platforms. Resolution/FPS budgets and encoder quality settings are unchanged.

The 60 FPS comparison supplies 1800 frames per run; each version encodes 1789.
The previous/current receiver decodes 1772/1780 frames before the final sample.
Detailed SDK logging is disabled for both measurements. Reproduce with
`DISCORDIA_TEST_REQUIRE_NVENC=1`, `DISCORDIA_TEST_NVENC_RATES=1`,
`DISCORDIA_TEST_NVENC_1440P60=1`, `DISCORDIA_TEST_MEDIA_LOG=warn`, then run
`cargo test -p discordia native_screen_codecs_reach_a_real_decoder -- --ignored --nocapture`.
The rate assertion checks that allocation reaches the budget during the run;
congestion control may lower the final allocation even on loopback.
