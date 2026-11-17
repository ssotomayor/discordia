# Native video publication paths

| Platform | Capture → SDK | Encoder evidence | Result |
|---|---|---|---|
| Windows | `windows_gpu.rs::Processor` can scale/convert BGRA textures to NV12 on D3D11. CPU conversion uses a reusable read-only BGRA staging texture (`Readback`), not a new read/write allocation per frame. Both paths finish as I420 | `nvidia/h264_encoder_impl.cpp::Encode` calls `ToI420`; `i420_buffer_cuda.h::CopyI420BufferToDeviceFrame` uploads host planes | GPU preprocessing reduces readback and CPU conversion where it wins; CPU also avoids allocation churn. Encoder upload remains |
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

Windows preprocessing checks the BGRA/sRGB → NV12/limited-range BT.601 conversion capability before use. The capture thread owns and unmaps all staging textures before publishing; size/device mismatches are rejected before copying.

| Selection | Policy |
|---|---|
| Probe | 16 fresh frames, CPU/GPU/GPU/CPU order; first four warm up, remaining six samples per path use p75. Each frame uses one preparation path and publishes once; failed GPU preparation may fall back to CPU |
| Admit GPU | p75 must beat CPU by 10% and stay below 75% of the configured frame interval. Any over-budget GPU probe declines GPU immediately |
| Reevaluate | Geometry changes or more than 500 ms without a callback after the previous preparation finished. Slow processing does not count as idle |
| Runtime fallback | Three over-budget GPU frames in the last eight restore CPU. A GPU API failure disables preprocessing for the capture; Disabled preparation mode never probes GPU |
| Memory | CPU staging texture is reused by device/size. After selecting GPU, CPU staging/scratch is released; selecting CPU releases GPU preprocessing surfaces |

Hardware regressions: `gpu_preprocessing_preserves_color_range_and_resizes_without_retaining_input` and `gpu_preprocessing_benchmark_1440p` (`--ignored --nocapture`). The benchmark rotates legacy per-frame CPU allocation, cached CPU and GPU, including readback; its elapsed times are not CPU utilization or encoder throughput. The cached BGRA mapping also has a WARP regression for changed pixels, odd geometry, padding and rejected mismatches.

Windows Settings → Video and screen → Screen capture hardware acceleration persists `screenshare_gpu_preprocessing`: Automatic defaults on, including older files missing the field; Disabled uses CPU preparation. It is independent of `screenshare_encoder` and applies to the next capture without restarting a live share. Other platforms do not expose the D3D11-specific control.

macOS already has native-buffer handoff. End-to-end Windows texture handoff still requires the SDK buffer bridge and encoder input changes above; GPU preprocessing is not zero-copy encoding.

## Design references

| Source | Constraint |
|---|---|
| [Microsoft Map](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-map), [DO_NOT_WAIT](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_map_flag) | Immediate readback can wait for GPU completion. Nonblocking mapping needs a policy for unfinished work; the current pipeline still maps synchronously |
| [Microsoft resource usage](https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_cpu_access_flag) | Staging access is read-only here; conversion never mutates captured pixels |
| [OBS video output](https://github.com/obsproject/obs-studio/blob/7e3aa99811798255bd1a13a6da7260ada76a4d95/libobs/obs-video.c#L874) | Reads the previous staging surface, overlapping work. A similar queue here would also need to deliver the last static frame without another capture callback |
| [Sunshine D3D11 encoder integration](https://github.com/LizardByte/Sunshine/blob/63d6d05259ad738243584e0cdd044966e0ffdd1e/src/platform/windows/display_vram.cpp) | Conversion targets encoder-owned GPU textures. Discordia's SDK currently still consumes CPU-backed I420 |
| [NVIDIA NVENC guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/nvenc-video-encoder-api-prog-guide/index.html) | DirectX resources can be registered as encoder inputs; ownership and synchronization must span encoding completion |
