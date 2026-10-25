# livekit 0.9.5

Source: crates.io `livekit` 0.9.5, upstream commit `49b3ebe9ab1c574114b164868ad226cc26a9b1be` in [LiveKit Rust SDK](https://github.com/livekit/rust-sdks). Apache-2.0; original source notices retained.

| File | Local change |
|---|---|
| `Cargo.toml` | Allow existing upstream unused/deprecated items and lifetime-syntax warnings exposed by using a local dependency. |
| `src/room/options.rs` | Optional `video_start_bitrate` in bps, unset by default; assign the highest RID to the principal layer for one/two/three encodings. |
| `src/rtc_engine/rtc_session.rs` | Forward the publishing track's startup hint alongside its encoding budget; publish audio RED (offer opus+RED when `red` is on) so E2EE keeps redundancy. |
| `src/room/participant/local_participant.rs` | Stop forcing `disable_red` under encryption now that the publisher sends RED itself; `red: false` still opts out. |
| `src/room/track/local_audio_track.rs` | Live microphone bitrate ceilings through RTP sender parameters; audio gets high transport priority. |
| `src/room/track/local_video_track.rs`, `network_bitrate.rs` | Reversible video bitrate budgets that preserve simulcast proportions, dynacast activation and original maximum limits. The client includes the pure policy tests. |
| `src/rtc_engine/peer_transport.rs`, `start_bitrate.rs`, `mod.rs` | Preserve explicit startup hints clamped to the encoding budget; apply once per peer connection using upstream negotiation handling. Unconfigured tracks use upstream startup policy. No minimum bitrate is imposed. |
| `src/room/mod.rs`, `src/rtc_engine/mod.rs`, `src/rtc_engine/rtc_session.rs` | Preserve `signal_connect_timeout` across the new signaling crate, bounding initial validation/join while ICE retains its own deadline. |
| `src/rtc_engine/peer_transport.rs` | `log_description`: every local and remote description either transport sets, at `debug`, with the audio m-lines, opus/RED rtpmap, fmtp and rtcp-fb lines summarised first. `RUST_LOG=livekit::rtc_engine::peer_transport=debug`. |

The client applies a hint only to screen shares: fixed per-resolution/FPS startup values independent of adaptive caps (see `docs/MAP.md`). Hints never exceed the cap. The pure startup policy is included in the client's test build so CI exercises the vendored implementation. Upstream replaces our older connection recovery/statistics fixes; they are not reapplied over its new lifecycle handling.
