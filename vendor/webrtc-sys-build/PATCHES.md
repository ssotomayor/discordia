# webrtc-sys-build 0.3.19

Source: [LiveKit Rust SDK](https://github.com/livekit/rust-sdks), crates.io release 0.3.19, Apache-2.0. `src/lib.rs` is unchanged.

| File | Local change |
|---|---|
| `Cargo.toml` | `zip` 0.6 → 9. 0.6 is reached by RUSTSEC-2026-0367/0368/0369 through the `extract()` this crate calls, and upstream still requires `^0.6`. The `webrtc-89d790b` archives hold no symlinks or setuid bits, so 9's stricter `extract()` unpacks them unchanged. |

Drop this crate and its `[patch.crates-io]` line once an upstream release moves to `zip` ≥ 9.
