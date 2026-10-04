# livekit 0.7.53

Source: crates.io `livekit` 0.7.53, upstream commit `da3ee007c044e31422ce3412346c3d6c93fea0b4` in [LiveKit Rust SDK](https://github.com/livekit/rust-sdks). Apache-2.0; original source notices retained.

| File | Local change |
|---|---|
| `Cargo.toml` | Allow existing upstream unused/deprecated items and lifetime-syntax warnings exposed by using a local dependency. |
| `src/room/options.rs` | Optional `video_start_bitrate` in bps, unset by default. |
| `src/rtc_engine/rtc_session.rs` | Forward the publishing track's startup hint alongside its encoding budget. |
| `src/rtc_engine/peer_transport.rs`, `start_bitrate.rs`, `mod.rs` | Apply the hint to video SDP offers; clamp to the encoding budget. Unconfigured tracks retain the 1 Mbps ceiling. No minimum bitrate is imposed. |

The client applies a hint only to screen shares: fixed per-resolution/FPS startup values independent of adaptive caps (see `docs/MAP.md`). Hints never exceed the cap. The pure startup policy is included in the client's test build so CI exercises the vendored implementation.
