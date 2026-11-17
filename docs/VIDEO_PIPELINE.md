# Native video publication paths

| Platform | Capture → SDK | Encoder evidence | Result |
|---|---|---|---|
| Windows | `windows_gpu.rs::Processor` can scale/convert BGRA textures to NV12 on D3D11, then read output-sized NV12 and separate I420 planes. Selection compares CPU/GPU costs on fresh frames; explicit CPU mode, slower/unsupported/failed GPU processing retain `windows.rs::BgraConverter` | `nvidia/h264_encoder_impl.cpp::Encode` calls `ToI420`; `i420_buffer_cuda.h::CopyI420BufferToDeviceFrame` uploads host planes | GPU preprocessing reduces readback and CPU conversion where it wins; still includes CPU planes and encoder upload |
| macOS | `sysvideo/macos.rs::Tap::handle_frame` retains a CVPixelBuffer; `voice.rs::ScreenVideoRoom` hands it to `NativeBuffer::from_cv_pixel_buffer` | `objc_video_frame_buffer.mm::new_native_buffer_from_platform_image_buffer` wraps the retained image | The client already passes a native image; SDK adaptation/software fallback may still convert it |
| Linux | Native screen capture is not implemented | Browser publication | No native capture path to optimize here |

## Windows texture path requirements

| Boundary | Existing capability | Required change |
|---|---|---|
| Capture | windows-capture 2.0.1 exposes `Frame::as_raw_texture` and `as_raw_surface` | Keep owned texture leases until encoding completes; capture surfaces may be recycled |
| SDK | Rust exposes CPU buffers and Apple native-buffer construction | Add a D3D11-backed WebRTC frame buffer and a minimal owned FFI constructor |
| NVIDIA | CUDA input accepts uploaded I420 planes | Implement texture registration/interop or a D3D11 NVENC input path, preserving rate adaptation and keyframes |
| AMD/Intel | Current factory selects Windows Media Foundation | Validate the prebuilt encoder's DXGI-device/input-surface support; it is not established by the factory alone |
| Geometry/color | CPU conversion defines the existing output | GPU conversion/scaling must preserve dimensions, color range, rotation and stride behavior |
| Fallback/preview | CPU/Auto/GPU modes and thumbnails use the current SDK path | Keep an explicit CPU conversion fallback and map only when a consumer needs CPU pixels |
| Publication | WebRTC owns transport and E2EE | Keep native frames inside the same publication path; no parallel packet transport |

Windows preprocessing uses limited-range BT.601 to match libyuv, owns its output/staging surfaces and releases each input view after submission. Six fresh frames compare costs (two warmups, four samples, alternating order); only one prepared frame is published each time. GPU must be at least 10% faster and stay below 75% of the frame interval. Eight consecutive over-budget GPU frames, or any processing failure, restore CPU for that capture. Hardware regressions: `gpu_preprocessing_preserves_color_range_and_resizes_without_retaining_input` and `gpu_preprocessing_benchmark_1440p` (`--ignored --nocapture`).

macOS already has native-buffer handoff. End-to-end Windows texture handoff still requires the SDK buffer bridge and encoder input changes above; GPU preprocessing is not zero-copy encoding.
