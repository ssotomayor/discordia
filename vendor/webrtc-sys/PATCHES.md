# webrtc-sys 0.3.49

Source: [LiveKit Rust SDK](https://github.com/livekit/rust-sdks), crates.io release 0.3.49, upstream commit `49b3ebe9ab1c574114b164868ad226cc26a9b1be`. Original notices remain in `NOTICE.md` and source headers.

| File | Local change |
|---|---|
| `build.rs` | Compile Windows x64 NVENC when `CUDA_PATH` contains driver headers/import library; link CUDA and delayimp. |
| `src/nvidia/nvidia_encoder_factory.cpp` | Load and probe the Windows NVENC driver DLL. |
| `src/nvidia/NvCodec/include/Utils/Logger.h` | Use Winsock 2 to match WebRTC's Windows headers. |
| `src/video_decoder_factory.cpp` | Keep Windows decoding on the software path; use the pinned Windows library's dav1d decoder despite its missing bridge define. Advertise AV1 only where a decoder is built; Linux NVDEC is unchanged. |
| `src/nvidia/h264_encoder_impl.{cpp,h}` | Screen sharing uses P3, single pass and no spatial AQ to avoid CUDA contention under saturated graphics load. Cameras retain P5/quarter-resolution multipass/AQ. No B-frames/lookahead; higher H.264 send levels under SDP asymmetry. Upstream `NvEncoder::SetRates` applies bitrate/FPS and five-frame VBV changes. |
| `src/nvidia/i420_buffer_cuda.h`, `context_scope.h` | Restore the thread CUDA context after failed Y/U/V uploads. Native fake-driver tests cover failed entry, individual copies and failed normal release. |
| `src/nvidia/cuda_context.cpp`, `nvidia_encoder_factory.cpp` | Failed context queries/activation return failure rather than a bare rethrow outside an exception handler; factories refuse an invalid context. |
| `src/windows/mf_encoder_factory.{cpp,h}`, `src/windows/{realtime_encoder,mf_input_buffer,mf_device,mf_result}.h` | Bind hardware H.264 MFTs to their Direct3D adapter with multithread protection, configure Intel's single temporal layer and a 5-second GOP ceiling when supported, honor input buffer size/alignment, and treat CODECAPI_AVEncVideoForceKeyFrame as optional on drivers that lack it. The HRESULT macro cannot shadow a result named `hr`. Retire skipped baseline inputs when later output arrives, so dropped frames cannot exhaust the pending limit. Retry temporary input rejection with the same sample/timestamp/keyframe; bound stalls, retain fresh queued frames, and preserve bitrate/FPS updates. `scripts/test-mf-encoder.ps1` covers real MF memory allocation, WARP context protection and retry/queue/timing/recovery without a hardware GPU. |
| `src/frame_crypto_transformer.cpp`, `src/h264_pps_retry.h`, `src/frame_cryptor.cpp`, `build.rs` | Windows compiles the matching `webrtc-89d790b` cryptor source locally. Link local objects before the artifact and use a symbol anchor to require the corrected implementation. Backport PPS-preserving encryption retries from upstream; retain the existing clear prefix, IV trailer and receiver format. Every retry uses a fresh IV, at most 32 attempts; errors/exhaustion drop the frame. Key material is never logged by the local copy. |
| `src/webrtc.rs` | Forward MFT failures with operation and hexadecimal HRESULT to the normal warning log; upstream `libwebrtc` otherwise demotes native errors to debug. |
| `src/video_encoder_factory.cpp`, `src/webrtc.rs`, `include/livekit/webrtc.h` | Prefer NVENC, then Windows Media Foundation hardware encoders; expose runtime hardware availability and codec names per backend, excluding pass-through encoders and Apple's software VPx hardware aliases. |
| `src/camera_capture.{cpp,rs}`, `include/livekit/camera_capture.h`, `src/lib.rs` | Preserve native camera format enumeration and selection used by setup and publication. |
| `src/peer_connection.{cpp,rs}`, `include/livekit/peer_connection.h` | Preserve structured removal errors across CXX for reliable teardown. |
| `src/apm.{cpp,rs}`, `include/livekit/apm.h` | `create_gain_controller`: an APM running only AGC2 adaptive digital, with its headroom, gain limits, rate and noise ceiling set by the caller. |
| `src/voice_audio_encoder.{cpp,rs}`, `include/livekit/voice_audio_encoder.h`, `src/peer_connection_factory.cpp`, `build.rs`, `src/lib.rs` | The audio encoder factory makes mono Opus a `VoiceAudioEncoder` built on `WebRtcOpus_*`, with expected loss fixed at 30%; other formats use the builtin factory. `probe_voice_encoder` encodes through the factory for tests. |
| `src/soundboard_decoder.{cpp,rs}`, `include/livekit/soundboard_decoder.h`, `build.rs`, `src/lib.rs` | Soundboard Opus uses the bundled libopus float decoder at 48 kHz, avoiding a second native library. Owned state and bounded slices validate channels, packet duration and output capacity. |

`webrtc-sys-build` 0.3.19 selects the matching `webrtc-89d790b` artifact. Headers,
bindings and libraries must move together; `LK_CUSTOM_WEBRTC` overrides require
the same API/ABI. Windows MFT NAL parsing now uses the current `ArrayView` API.

`client/build.rs` delay-loads CUDA and Media Foundation so unavailable optional runtimes do not prevent startup. Missing CUDA build dependencies omit NVENC; Windows hardware MFTs remain available without CUDA. Media Foundation falls back through WebRTC's software wrapper on initialization/encoding failure. Screen-share fallback above the 720p30 pixel-rate budget per encoder is refused before OpenH264 initialization; cameras and explicitly selected CPU use their existing paths. The Windows client stops unexpected high-budget software, stalled active output or a stats timeout; paused layers do not establish encoder demand. See `docs/AUDIT_AMD_WARZONE_STREAM.md`.

MFT slice headers normally fit LiveKit's two clear H.264 bytes (NAL header plus one slice-header byte). AMD's `0x88` IDR slice with PPS 0 leaves one PPS bit encrypted: the cryptor now chooses fresh IVs until the sent bits parse as the original PPS. That specific header is allowed; other incompatible headers still fail. Parameter-set identifiers are set to zero when the MFT supports it. Unencrypted MFT tracks also permit that valid header.

Cryptor source: [webrtc-sdk/webrtc at 89d790b](https://github.com/webrtc-sdk/webrtc/blob/89d790b/api/crypto/frame_crypto_transformer.cc), with the PPS retry approach from [m150_release](https://github.com/webrtc-sdk/webrtc/blob/m150_release/api/crypto/frame_crypto_transformer.cc). Keep source and artifact headers at the same revision on upgrades. Receiver decryption and cipher/key-derivation algorithms are unchanged.

`probe_mf_fallback` runs the production guarded constructor and real WebRTC fallback wrapper with simulated encoders. Client tests force initialization and mid-stream failures without a GPU, checking software is never initialized above budget and low-budget/camera fallback still works. An unguarded control runs only simulated encoders and demonstrates that the same failure would initialize software.

| Diagnostic environment variable | Purpose |
|---|---|
| `LIVEKIT_PREFERRED_HW_ENCODER=mediafoundation` | Prefer Windows hardware MFTs over NVENC. |
| `DISCORDIA_MF_ENCODER_FILTER=AMD` | Restrict MFT enumeration to names containing this case-sensitive string; normal operation enumerates all vendors. |
