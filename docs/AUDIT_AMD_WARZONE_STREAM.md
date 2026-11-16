# Windows screen-share audit: codecs, transports and GPU load

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
| Encoder output progress | Stop after five seconds of fresh frames accepted by WebRTC without encoded-counter progress on active layers with assigned bitrate, while connected and unmuted. Adapter-rejected frames, paused layers, absent/unknown outbound stats and idle sources do not authorize a stop. Progress is summed across active layers; stream changes, counter resets and long sampling gaps restart the deadline |
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
system driver hang can be interrupted. The initial RX 5700 XT review did not
apply GPU stress, HAGS changes or a switch to software VP8.

Diagnostic signatures remain useful references, not prerequisites for this work:

| Log | What it establishes |
|---|---|
| `H264 slice header incompatible with LiveKit encryption` | Encoded-header compatibility failure, not proof of GPU saturation |
| `Hardware encoder stalled without output` | The output watchdog expired, not why output stopped |
| `ProcessInput` / native failure with device-removed/reset HRESULT | Device error returned by the driver; correlate with Windows events |
| `Refusing high-load Media Foundation software fallback` | The new native protection refused software initialization |

## Additional RX 9060 XT report (2026-11-15)

| Evidence | Limit |
|---|---|
| Another user sharing Call of Duty received the encoder-output stop message using H.264; switching to VP8 worked | GPU reported as RX 9060 XT. Resolution/FPS, encoder mode, driver, CPU and utilization are unknown. This does not prove resource saturation or the exact MFT failure |
| D3D11 device passed to the asynchronous MFT without multithread protection | Enable and verify protection before registering the DXGI manager; tested with a real WARP device. [Microsoft's threading guidance](https://learn.microsoft.com/en-us/windows/win32/direct3d11/overviews-direct3d-11-render-multi-thread-intro) distinguishes device and context safety; its [Media Foundation guidance](https://learn.microsoft.com/en-us/windows/win32/medfound/supporting-direct3d-11-video-decoding-in-media-foundation) recommends context protection against deadlocks |
| `MF_TRY(hr)` expanded to a new `HRESULT hr = hr` | The macro read its own uninitialized variable instead of the ProcessOutput result. Rename the macro temporary; a production-macro test checks successful and failed HRESULTs under `/W4 /WX` |
| Local AMD hardware reproduced the H.264 failure without Call of Duty | `AMDh264Encoder` emitted valid IDR prefix `0x88` with PPS 0, which the compatibility guard rejected. The first PPS bit is outside the existing clear prefix. Backport fresh-IV encryption retries so the encrypted bits still parse as the original PPS; preserve authentication, encryption and receiver format. The MFT now permits that specific header |
| Initial backport still lost about half the keyframes | The old cryptor object in the artifact archive took link priority. Link local objects first and reference an anchor in the corrected cryptor object so it cannot silently remain unused |
| Only the matching output timestamp was removed from `submitted` | Skipped inputs could retain raw-frame references until all eight pending slots were occupied, preventing more submissions. Baseline H.264 has no B-frame reordering, so later output now retires older input timestamps too. A regression exercises 1,000 skipped inputs, verifies release of their buffers and preservation of newer pending frames |
| Resource exposure | BGRA readback alone is `width × height × 4 × FPS`: 497.664 MB/s at 1080p60 or 884.736 MB/s at 1440p60, before I420/NV12 conversion. Downscaling still reads the source dimensions. These are data-flow estimates, not measured CPU/GPU use or retained RAM |

The capture callbacks are serialized, pacing precedes readback/conversion, the
encoder queue holds three fresh frames, and at most eight submitted frames wait
for output per MFT. The pending-frame fix prevents skipped frames from consuming
that fixed budget forever. Neither change switches codecs, disables encryption,
raises watchdog deadlines or permits high-load software fallback. Encryption
retries stop after 32 fresh-IV attempts or the first cipher error; exhaustion
drops the frame, and native regressions cover these boundaries. Reproduction
with Call of Duty on the RX 9060 XT remains unperformed.

Local verification uses AMD Radeon(TM) Graphics (driver `32.0.21042.62`), a
separate loopback SFU and encrypted moving synthetic I420 frames, not a game
capture. At 1080p30, the sustained decoder-delivery check passed: roughly 486
encoded frames reached the decoder, versus about half before the link fix.
This establishes a real AMD H.264 compatibility fix, not a measured resource
saturation diagnosis for the reported RX 9060 XT.

1440p30 and 1440p60 configuration probes also passed decoder-delivery checks.
The AMD integrated GPU did not sustain 60 encoded FPS; these validate encrypted
delivery at the selected dimensions, not RX 9060 XT gaming performance.

## First-publication pixelation and diagnostics

| Evidence / check | Result and limit |
|---|---|
| RTX 5080 sender: all viewers see pixelation and 1–5 FPS on first publication; republishing fixes it | Sender codec, resource use and logs unavailable; distinct from the AMD report |
| Offers could overlap sender setup, before backend/codec preferences were set | Serialize both initial and renegotiated offers with sender configuration. A native test forces the overlap and verifies H.264-only SDP. Related [LiveKit report](https://github.com/livekit/rust-sdks/issues/1572); exact cause of this user's pixelation remains unconfirmed |
| Local RTX 5080, encrypted H.264 1440p60; temporarily cap bitrate to 300 kbps and restore | Recovered about 31.7 Mbps and 59 FPS; no persistent low-bitrate state reproduced |
| Five seconds of startup black keepalives followed by moving monotonic-timestamp frames | Recovered normal output; does not establish a timestamp bug |
| Cold H.264 1440p60 with two receivers and real simulcast | Passed layer delivery/recovery checks: 1,796 encoded frames, 59.4 encoded FPS average, 60 FPS final output; 312 lower-layer and 763 recovered high-layer frames. Main native I420 delivery averaged 29.4 FPS, so this does not establish 60 FPS playback on both receivers. Allocations varied across runs (one peaked at 6.7 Mbps, another reached 34 Mbps); the maximum-budget assertion belongs to the independent single-layer rate probe, not adaptive layer switching |
| Normal application log (`logs/discordia.log` under the configuration directory) | `screen share diagnostic checkpoint` records requested settings, actual codec/encoder, capture/readback/conversion/encode timing, bitrate, keyframes, PLI/NACK and connection state; `screen share diagnostic layer` records each RID's activation, output and byte counters. First sample, then every 5 seconds through startup, every 30 seconds afterwards |
| No outbound stats / fatal output stall / stats timeout | Startup checkpoints also record absent output; terminal warnings capture accepted/dropped input and connection state before teardown. Track SID correlates checkpoints within a publication. No tokens, keys, frame payloads, SDP or network addresses are added |

For the next incident, preserve the sender's normal log before restarting the
app and note the approximate time of the first share and its successful retry.
Compare capture, adapter admission, encoder output, layer activation and bitrate
across those two track SIDs. Existing debug-per-second stats remain opt-in.

| On-screen report code | Evidence / scope |
|---|---|
| `TX-CAPTURE` | Capture is slow and measured processing exceeds its frame budget; no inference from an idle desktop alone |
| `TX-ENCODER` | WebRTC reports CPU limitation or measured encoding pressure while capture continues; does not identify a driver fault |
| `TX-OUTPUT` | Active allocated output produces 0–5 FPS while capture supplies at least 10 FPS; unknown cause is stated explicitly. Also included in the fatal encoder-output stop |
| `TX-NETWORK` | Publisher reports bandwidth limitation |
| `TX-BITRATE` | Assigned bitrate is below 40% of the selected cap while capture continues; does not by itself identify a network fault |
| `TX-VOICE` | Voice-protection budget was applied successfully and active video allocation matches it; a failed request or mere congestion intent is not enough |
| `RX-NETWORK` / `RX-PLAYBACK` | Recent receiver loss plus interrupted playback / measured decode or display pressure |
| `RX-OUTPUT` | At most 5 decoded FPS despite sustained incoming video packets and a fresh sender report of higher FPS at the same resolution; insufficient evidence to name a network/decoder cause |
| `RX-STARTUP` | The attached view still lacks its first playable frame after 12 seconds |
| `TX-DEVICE` / `STREAM-UNSTABLE` | Legacy sender evidence cannot separate capture/encoding / insufficient evidence for a playback cause |
| `TX-STATS` | Native encoder statistics failed to respond before the stop deadline |
| `TX-CONNECTION` | Native publisher could not join its video room; sanitized error text distinguishes signaling/ICE failures without exposing tokens |

Preview, inline and popout warnings include translated explanations and report
codes. Sustained health warnings remain visible when controls auto-hide, clear
after recovery, and log code transitions without waiting for Diagnostics mode.
Tests cover native Dioxus rendering, stale/reset/replacement reports, paused or
idle output, recovery, and responsive footer visibility in a real browser.

## Codec, network and saturation follow-up (2026-11-16)

| Check | Measured result |
|---|---|
| Encrypted H.264/NVENC, VP8/libvpx and H.264/OpenH264 | Decoded all three formats over each of IPv4/UDP, IPv6/UDP, IPv4/TCP and forced IPv4 TURN/UDP |
| Signaling versus media family | IPv6 signaling with IPv4 media and IPv4 signaling with IPv6 media; assert the selected ICE endpoints on both publisher and subscriber, not merely successful WebSocket connection |
| GPU contention, original NVENC P5 + quarter-resolution multipass + spatial AQ | RTX 5080 at 99–100% GPU (99.79% average). Output stopped after 379 encoded frames and remained at 0 FPS while the connection retained a positive bitrate allocation |
| Same contention with P3, single pass, spatial AQ disabled | RTX 5080 at 100% for all 58 telemetry samples. 1,781 frames encoded and 1,765 decoded in 30 seconds (59.36 / 58.83 FPS averages), 2560×1440 at 60 FPS final output, approximately 33 Mbps |
| Repeated saturation checks | Continued producing about 59 encoded/decoded FPS. Assigned bitrate varied (one run stayed around 5–7 Mbps; another reached 34 Mbps). Under contention, require ≥85% frame output and ≥80% decoder delivery, and verify actual bitrate follows the allocation. Maximum-cap ramp-up remains the separate unloaded rate-control check; a cap is not a guaranteed allocation |
| Real WGC capture at 100% GPU | Monitor 2560×1440: 145 frames/3 s; window 1280×1440: 109 frames/3 s. Capture stopped correctly and thumbnails remained available. Requested 60 FPS is a cap, not proof that a mostly static desktop presents 60 fresh frames each second |
| CUDA upload exceptions | Restore the previous thread context on failed Y/U/V uploads; fake-driver native tests cover failed entry, each copy and failed normal cleanup. CUDA context activation errors now return failure instead of a bare rethrow terminating the process |
| Native screen audio/video signaling stall | Use the voice room's 4 s signaling budget and disable SDK join retries. The selected successful voice endpoint is retained across token renewal. A native-video test holds a real TCP connection silent and requires a bounded, token-redacted failure |

NVIDIA documents CUDA use for AQ in its [SDK guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/ffmpeg-with-nvidia-gpu/index.html).
Screen sharing uses the lighter preset; camera tuning is preserved. This trades
additional compression optimization for compute headroom without changing codec,
requested dimensions/FPS or encryption. The failure was reproduced under synthetic
D3D11 compute load, not Call of Duty; VRAM exhaustion and TDR were not induced.
RX 9060 XT/5700 XT gaming, Android reception, WAN loss, NAT64 and TURN/TLS remain
outside the measured coverage. The original first-publication report is not yet
attributed conclusively to this saturation failure.

| Reproduce on Windows | Output / boundaries |
|---|---|
| `pwsh -File scripts/test-stream-network.ps1` | Four local SFUs, three codecs each; route assertions; `target/stream-network/`. TURN permits restricted peers only at this machine's interface addresses. No production credentials or host configuration changes |
| `pwsh -File scripts/test-stream-contention.ps1` | Exactly one NVIDIA GPU, synthetic H.264 1440p60, up to 45 s load. Requires ≥95% GPU in ≥90% of samples; `target/stream-contention/encoder/` |
| `pwsh -File scripts/test-stream-contention.ps1 -Stage Capture -Seconds 15` | Separate real monitor/window capture and stop under load; `target/stream-contention/capture/` |

The probe allocates a 4 MiB UAV, bounds each GPU submission wait, accepts a stop
file and limits runtime to 60 s. Wrappers track their own PIDs, restore environment
variables and clean up their isolated SFUs, including after test failures. They
do not open production accounts or send messages to members.

## Live voice-protection incident

Local 1440p60 telemetry showed a 34 Mbps video allocation followed by a 7% voice
loss report. The old policy immediately capped video at 300 kbps for 20 s, even
after the next fresh report fell below 2%. Recovery to about 19 Mbps took roughly
90 s after releasing that ceiling. GPU samples were 61–70%, encoder 5–6%.

`voice_video_protection.rs` debounces moderate loss, retains immediate protection
for severe loss, and reduces/reopens ceilings in steps from the current allocation.
It never forces a bitrate minimum. Invalid/stale feedback cannot hold the limit
forever; failed sender updates retain the last successfully applied policy.
Deterministic regressions cover isolated/sustained loss, recovery, expiry and caps.
The initial bandwidth drop preceded protection; its network cause remains unknown.

## Bitrate recovery follow-up (2026-11-16)

| Observation | Result / limit |
|---|---|
| Local encrypted NVENC 1440p60, moving synthetic frames, no imposed ceiling, 30 s | Final target 33.96 Mbps, output 34.3 Mbps, 1,791 encoded / 1,780 decoded frames |
| Same pipeline, 300 kbps ceiling from second 5 to 10, two 30 s runs | Final targets 3.28 and 5.70 Mbps; both failed the existing peak-allocation assertion. Encoder continued at 59–60 FPS and output followed allocation |
| Same ceiling, 90 s observation | Reached 34 Mbps around second 59 without republishing; final output 33.84 Mbps, 5,148 encoded / 5,045 decoded frames. Existing rate assertions passed |
| Live outgoing 1440p60 session, same publication from 17:43–17:49 UTC | Target reached 34 Mbps at 17:45:36, then varied down to 4.61 Mbps at 17:49:06. Hardware encoding stayed near 3 ms/frame and 57–60 FPS; voice protection remained unapplied. IPv4/UDP direct media route |
| Simultaneous incoming McClane stream | Samples around 55–58 FPS, 2.3–4.4 Mbps, decode 0.5–0.7 ms/frame, approximately 39–46 ms video buffer. Loss counters rose from 121 to 139 over 149,298 to 153,388 received packets (about 0.44% across that interval); no reported freezes |

These runs reproduce variable, sometimes slow allocation recovery, not a permanent
encoder stall. The direct 300 kbps test bypasses the graded voice policy and does
not identify the cause of a remote sender's incident. The live session recovered
and fell again without restarting, so stopping/restarting alone cannot distinguish
encoder state from congestion control or SFU allocation. Sparse samples do not
establish which network leg caused the allocation changes.

`DISCORDIA_TEST_NVENC_RECOVERY_SECONDS` extends the ignored native decoder probe
with congestion or idle recovery enabled, bounded to 30–120 seconds. Production
voice-protection thresholds and `TX-BITRATE` warnings are unchanged. Tests use an isolated local
SFU, not WAN impairment or Call of Duty. Diagnostic dialogs can temporarily report
zero displayed FPS while decode counters continue; compare successive samples.

Checkpoints now include `publisher_network`: estimated uplink capacity and RTT
from this video's selected transport, plus the matching SSRC/local-ID RTCP loss,
RTT, report timestamp and measurement count. In an SFU room this receiver is the
SFU, not an end viewer. These values describe publisher-to-SFU feedback, not the
SFU's downstream allocation. Missing/invalid metrics remain absent; report identity
and measurement counts allow repeated feedback to be recognized. No addresses,
tokens or media payloads are included, and no bitrate decision uses these fields.
Checkpoints also retain cumulative packet-send delay, packet/retransmission counts
and QP sum; compare deltas with the same track SID instead of treating totals as
current loss, latency or a codec-independent quality score.

| Idle recovery probe | Measured result / boundary |
|---|---|
| `DISCORDIA_TEST_NVENC_IDLE_RECOVERY=1` with NVENC rate/1440p60 probes | Five seconds of moving frames, 15 s with no new frames, then 40 s of motion; no duplicate frames supplied. Duration defaults to 60 s. Requires allocation ≥90% of the local cap within 5 s of resumption, ≥85% encoded active frames and ≥80% decoder delivery. `DISCORDIA_TEST_NVENC_LOW_MOTION=1` instead supplies one fresh cursor update/s |
| Unmodified transport, idle run | Target 10.42 Mbps before idle, 2.94 Mbps on resumption, 7.10 Mbps at the end; 2,690 encoded / 2,668 decoded. Allocation assertion failed while final output remained 60 FPS |
| Unmodified transport, continuous-motion control | Reached 33.90 Mbps and 60 FPS within 30 s; 1,789 encoded / 1,776 decoded; rate assertions passed |
| Experimental periodic ALR probing enabled | Post-idle target peaked at 6.68 Mbps and ended at 6.55 Mbps; 2,690 encoded / 2,660 decoded. Did not fix recovery; experiment removed from production code |

The idle case reproduces slow allocation recovery on an isolated local SFU with
encrypted H.264, not the exact cause of a user's WAN incident. Matching RTCP
samples after resumption reported zero loss and approximately 1 ms RTT; this
does not prove an uncongested path throughout the pause.

| Correction / review | Evidence and boundary |
|---|---|
| Connection probing budget | Configured track budgets, with 2x packet/probe headroom, replace the default 5 Mbps probe ceiling. No minimum or estimator restart is imposed; RTP caps remain unchanged. Temporary voice caps never overwrite configured budgets |
| RTX negotiation | Keep RTX with the chosen media codec. Native SDP tests require RTX/apt without enabling another video codec |
| No fresh frames for 15 s, 1440p60 | Recovered 34 Mbps at the first post-idle sample (about 1 s), 2,691 encoded / 2,683 decoded |
| One fresh cursor update/s, then fully changing frames | Recovered 34 Mbps within 5 s, final 60 FPS, 2,668 encoded / 2,563 decoded |
| 300 kbps ceiling, then release | Constrained allocation 250 kbps; recovered 34 Mbps about 2 s after release, 1,516 encoded / 1,510 decoded |
| Reused late-viewer probe from `test-stream-1440p-recovery` | Uses actual picker budgets. 1440p60: first frame 91 ms, 57.15 decoded FPS; 1440p30: 108 ms, 29.99 decoded FPS. Publisher was not restarted |
| First-frame High request | Preserved with a guard for receiver backoff/probing, Poor connections, self preview and replaced publications. 26 JS tests passed |
| Optional MF keyframe control | Only unsupported controls are tolerated. Unexpected support-query/write failures propagate; native tests inject device removal/reset, allocation, permission and parameter failures. An unsupported control is not evidence that every failure is harmless |

Media Foundation requests a GOP size of `configured_fps * 5` frames when supported.
That is five seconds only at the configured frame rate: [Microsoft defines GOP size
in pictures](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder).
It does not guarantee first-frame recovery on a static source without fresh input.
The real AMD Media Foundation path was checked at 720p30 with encrypted decoder
delivery; this does not establish the RX 9060 XT/Call of Duty incident's cause.

The bitrate/idle and late-viewer checks use an isolated local SFU and synthetic
frames. They do not measure game-footage visual quality, WAN impairment or GPU
saturation for this follow-up. No duplicated source frames or production app
replacement are introduced by these probes.

