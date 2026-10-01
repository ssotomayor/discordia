# webrtc-sys 0.3.39

Source: [LiveKit Rust SDK](https://github.com/livekit/rust-sdks), crates.io release 0.3.39. Original notices remain in `NOTICE.md` and source headers.

| File | Local change |
|---|---|
| `build.rs` | Compile Windows x64 NVENC when `CUDA_PATH` contains driver headers/import library; link CUDA and delayimp. |
| `src/nvidia/nvidia_encoder_factory.cpp` | Load and probe the Windows NVENC driver DLL. |
| `src/nvidia/NvCodec/include/Utils/Logger.h` | Use Winsock 2 to match WebRTC's Windows headers. |
| `src/video_decoder_factory.cpp` | Keep Windows decoding on the existing software path; Linux NVDEC is unchanged. |
| `src/nvidia/h264_encoder_impl.{cpp,h}` | Initialize NVENC configuration/profile; allow the encoder to select a higher H.264 send level when SDP negotiates level asymmetry. |

`client/build.rs` delay-loads `nvcuda.dll` so binaries also start without an NVIDIA driver. Missing CUDA build dependencies omit NVENC; unavailable runtime hardware falls back in Automatic mode. Explicit GPU mode reports unavailable hardware or a detected software fallback.
