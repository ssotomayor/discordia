# webrtc-sys 0.3.39

Source: [LiveKit Rust SDK](https://github.com/livekit/rust-sdks), crates.io release 0.3.39. Original notices remain in `NOTICE.md` and source headers.

| File | Local change |
|---|---|
| `build.rs` | Compile Windows x64 NVENC when `CUDA_PATH` contains driver headers/import library; link CUDA and delayimp. |
| `src/nvidia/nvidia_encoder_factory.cpp` | Load and probe the Windows NVENC driver DLL. |
| `src/nvidia/NvCodec/include/Utils/Logger.h` | Use Winsock 2 to match WebRTC's Windows headers. |
| `src/video_decoder_factory.cpp` | Keep Windows decoding on the software path; use the pinned Windows library's dav1d decoder despite its missing bridge define. Advertise AV1 only where a decoder is built; Linux NVDEC is unchanged. |
| `src/nvidia/h264_encoder_impl.{cpp,h}` | Initialize NVENC configuration/profile; use P5 low-latency tuning, quarter-resolution multipass and spatial AQ without B-frames/lookahead; allow higher H.264 send levels under SDP level asymmetry; apply WebRTC bitrate/FPS changes through NVENC reconfiguration before encoding. |
| `src/windows/mf_encoder_factory.{cpp,h}`, `src/windows/realtime_encoder.h` | Enumerate hardware H.264 MFTs and bind the matching Direct3D adapter. Retain fresh queued frames and keyframe requests under overload; preserve capture timing, apply bitrate/FPS updates, and fail stalled MFTs to the software wrapper. Driver names remain visible in diagnostics. `scripts/test-mf-encoder.ps1` tests queue/timing/recovery without a GPU. |
| `src/video_encoder_factory.cpp`, `src/webrtc.rs`, `include/livekit/webrtc.h` | Prefer NVENC, then Windows Media Foundation hardware encoders; expose runtime hardware availability and codec names per backend, excluding pass-through encoders. |

`client/build.rs` delay-loads CUDA and Media Foundation so unavailable optional runtimes do not prevent startup. Missing CUDA build dependencies omit NVENC; Windows hardware MFTs remain available without CUDA. Media Foundation falls back through WebRTC's software wrapper on initialization/encoding failure. Explicit GPU mode stops on a detected software fallback.

MFT slice headers must fit LiveKit's two clear H.264 bytes (NAL header plus one slice-header byte). Longer headers fall back before publication: encrypting the PPS identifier makes WebRTC discard frames before decryption. This compatibility guard also applies to unencrypted MFT tracks; NVENC is unchanged.

| Diagnostic environment variable | Purpose |
|---|---|
| `LIVEKIT_PREFERRED_HW_ENCODER=mediafoundation` | Prefer Windows hardware MFTs over NVENC. |
| `DISCORDIA_MF_ENCODER_FILTER=AMD` | Restrict MFT enumeration to names containing this case-sensitive string; normal operation enumerates all vendors. |
