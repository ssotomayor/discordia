# Windows screen-share audit: AMD / Warzone

## Incident evidence

| Reported | Status |
|---|---|
| RX 5700 XT, Ryzen 5 5500, 16 GB RAM | User-provided profile; not measured locally |
| Automatic H.264, 1920×1080 at 60 FPS | User-confirmed settings |
| Video stalls, degraded voice, Warzone fails and Discordia stops responding after a while | Reported symptoms; not reproduced on the sender's hardware |
| Audio target buffer around 1 second | Screenshot; lifetime target average, not instantaneous video latency |
| Recent non-silent audio concealment near 20% with zero recent loss | Local receiving logs; evidence of degraded audio, not proof of its cause |

## Code findings

| Path | Confirmed behavior |
|---|---|
| `vendor/webrtc-sys/src/windows/mf_encoder_factory.cpp` | Media Foundation hardware failures can activate OpenH264 through the software fallback wrapper |
| H.264 / encryption compatibility | Slice headers that do not fit LiveKit's clear prefix are rejected, independently of GPU load; this can also initiate software fallback |
| `src/windows/realtime_encoder.h` | The watchdog detects two seconds without output between MFT calls; it cannot interrupt a driver call that never returns |
| `client/src/features/voice.rs` | Before this change only explicit GPU mode stopped when software appeared in publisher stats |
| `client/src/sysvideo/windows.rs` | Capture errors propagate to the fatal channel; capture start/stop already run outside the UI event loop |
| Capture / MFT conversion | BGRA readback → I420 → NV12; capture scratch is reused, pending queues are bounded |

Sender logs will not be available. The mitigation therefore covers the confirmed
fallback path without assigning a unique cause to this incident. TDR, RAM/VRAM
exhaustion, driver version and anti-cheat interference
remain unconfirmed. A 497.664 MB/s BGRA pixel flow at 1080p60 does not mean that
497 MB of RAM accumulates each second. CPU thread counts and utilization cannot be
deduced from the processor model alone.

## Mitigations

| Layer | Result |
|---|---|
| MF software fallback initialization | Reject screen-share fallback above 1280×720×30 pixels/second per encoder before initializing OpenH264. Camera encoding and explicit CPU selection bypass this restriction |
| Windows publisher statistics | Stop Automatic sharing when hardware was advertised for the selected codec but known software appears above that budget. Unknown implementation names are not classified as hardware |
| Encoder output progress | Stop after five seconds of fresh captured frames without encoded-counter progress on active layers with assigned bitrate. Paused layers, absent/unknown outbound stats and idle sources do not authorize a stop. Progress is summed across active layers; stream changes, counter resets and long sampling gaps restart the deadline |
| Statistics request | Stop sharing if a Windows video stats request does not complete within five seconds |
| Cleanup | Use the existing share-failure path: clear the share target, release capture and unpublish the screen track; do not issue LeaveVoice |
| Messages / logs | Translate the stop reason and record the actual software implementation and requested dimensions/FPS; do not assert GPU overload without evidence |

The 720p30 threshold is a conservative product policy, not a benchmark guarantee.
CPU explicitly selected by the user and Automatic without advertised hardware
remain available. Windows MF low-budget fallback remains available. Explicit GPU
mode continues rejecting software. The native two-second watchdog is unchanged.

This does not recover a Windows GPU reset or cancel a blocked native driver call.
The async deadlines can act only while the application executor is being scheduled.

## Validation

Windows validation completed on 2026-11-14, based on `origin/master` at `c4b0fc77`:
1257 client library tests passed (16 ignored); all-target client/grid Clippy with
`-D warnings`, formatting and five translation checks passed.

| Check | Coverage |
|---|---|
| C++ encoder regression suite | Budget boundary, overflow-sized inputs, refused initializer never invoked, permitted low-budget/camera initialization; existing queue/retry/timestamp/watchdog cases |
| Real WebRTC fallback wrapper, simulated encoders | Fail hardware initialization or fail while encoding. The unguarded control initializes/uses software at 1080p60; the production guarded constructor does neither. 720p30 and camera fallback remain permitted; no GPU or sender telemetry required |
| Rust policy tests | Hardware→software reports, unknown names, CPU explicit, no advertised hardware, portrait/ultrawide, pixel/FPS limits, stalled output, idle input, progress and stream/counter replacement |
| Application failure transition | The actual share-failure handler turns off the share without changing the connected voice session; it ignores duplicate failures, wrong targets and obsolete epochs |
| Existing real-SFU MF fallback probe | Uses 720p30 so it continues testing the permitted fallback path; requires suitable hardware |
| RX 5700 XT / Warzone reproduction | Not performed; no claim of a confirmed driver fix |

Acceptance does not require sender logs or reproducing a GPU reset. The criteria
are bounded fallback, a scoped share-failure transition and no false stop for
paused/idle encoding. The controlled tests establish those properties; they do
not establish why the RX 5700 XT stopped encoding or guarantee that an operating
system driver hang can be interrupted. GPU stress, HAGS changes and switching to
software VP8 were not applied.

Diagnostic signatures remain useful references, not prerequisites for this work:

| Log | What it establishes |
|---|---|
| `H264 slice header incompatible with LiveKit encryption` | Encoded-header compatibility failure, not proof of GPU saturation |
| `Hardware encoder stalled without output` | The output watchdog expired, not why output stopped |
| `ProcessInput` / native failure with device-removed/reset HRESULT | Device error returned by the driver; correlate with Windows events |
| `Refusing high-load Media Foundation software fallback` | The new native protection refused software initialization |
