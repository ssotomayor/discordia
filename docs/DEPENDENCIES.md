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
cargo tree -p dioxusfun --locked --target x86_64-pc-windows-msvc -e normal,build --prefix none --format '{p}'
cargo tree -p dioxusfun --locked --target aarch64-apple-darwin -e normal,build --prefix none --format '{p}'
cargo tree -p dioxusfun --locked --target x86_64-pc-windows-msvc -e features -i image
```

Deduplicate package/version pairs and exclude `dioxusfun`. Feature checks must
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
