# webrtc-sys 0.3.39

Source: [LiveKit Rust SDK](https://github.com/livekit/rust-sdks), crates.io release 0.3.39. Original notices remain in `NOTICE.md` and source headers.

| File | Local change |
|---|---|
| `build.rs` | Compile Windows x64 NVENC when `CUDA_PATH` contains driver headers/import library; link CUDA and delayimp. |
| `src/nvidia/nvidia_encoder_factory.cpp` | Load and probe the Windows NVENC driver DLL. |
| `src/nvidia/NvCodec/include/Utils/Logger.h` | Use Winsock 2 to match WebRTC's Windows headers. |
| `src/video_decoder_factory.cpp` | Keep Windows decoding on the existing software path; Linux NVDEC is unchanged. |
| `src/nvidia/h264_encoder_impl.{cpp,h}` | Initialize NVENC configuration/profile; allow higher H.264 send levels under SDP level asymmetry; apply WebRTC bitrate/FPS changes through NVENC reconfiguration before encoding. |
| `src/windows/mf_encoder_factory.{cpp,h}` | Enumerate hardware H.264 MFTs, bind the matching Direct3D adapter, encode asynchronously with bounded queues and expose the driver encoder name. |
| `src/video_encoder_factory.cpp` | Prefer NVENC, then Windows Media Foundation hardware encoders; expose runtime hardware availability. |

`client/build.rs` delay-loads CUDA and Media Foundation so unavailable optional runtimes do not prevent startup. Missing CUDA build dependencies omit NVENC; Windows hardware MFTs remain available without CUDA. Media Foundation falls back through WebRTC's software wrapper on initialization/encoding failure. Explicit GPU mode stops on a detected software fallback.

| Diagnostic environment variable | Purpose |
|---|---|
| `LIVEKIT_PREFERRED_HW_ENCODER=mediafoundation` | Prefer Windows hardware MFTs over NVENC. |
| `DISCORDIA_MF_ENCODER_FILTER=AMD` | Restrict MFT enumeration to names containing this case-sensitive string; normal operation enumerates all vendors. |
