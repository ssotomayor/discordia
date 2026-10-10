# Rubato migration plan

Base: `origin/master` at `3ada06b`, inspected 2026-10-09.
Branch: `update-rubato-5`. App dependency migrated from 0.16.2 to 5.0.1.
PR base refreshed to `origin/master` at `65d2a18`; rebase completed without conflicts.

## Current integration

| Path / symbol | Responsibility |
|---|---|
| `client/src/features/voice.rs`: `AudioResampler` | Mono `f32`, `Fft` with fixed input, 512-frame hint, 2 FFT sub-chunks and `BlackmanHarris2`; accumulated input and reusable output scratch |
| `MicCapture::start_cpal`, `maybe_raw`, `forward_mic` | CPAL/raw device rate to 48 kHz after channel averaging; feeds 480-sample/10 ms DSP frames |
| `consume_remote_track` | 48 kHz to output-device rate; reconstructs resampler when playback rate changes; used for remote voice and shared audio |
| `soundboard_loop` | 48 kHz to output-device rate for local soundboard monitoring; published audio remains 48 kHz |
| `client/src/sound_decode.rs`: `decode` | Decoded file rate to 48 kHz; pads 1024 input samples to flush the tail, then caps at 5.5 seconds |
| `client/Cargo.toml`, `Cargo.lock` | App uses Rubato 5.0.1; 0.16.2 is a test-only reference; DeepFilterNet separately retains 0.14.1 |

Notification synthesis uses the selected output rate directly. Screen capture
has its own upstream conversion; this plan concerns consumers of `AudioResampler`.

## Implementation order

| Order | Change | Acceptance |
|---|---|---|
| 1 | Record 0.16.2 reference output, frame counts, impulse delay, CPU and buffer capacities | Same deterministic mono inputs, rate pairs, compiler/profile and hardware for both versions |
| 2 | Pin app Rubato to `=5.0.1`; update its dependency graph | Preserve DeepFilterNet/tract and Rubato 0.14.1; no SDK or CPAL changes; verify Rust >= 1.87 in CI |
| 3 | Replace `FftFixedIn<f32>` with `Fft<f32>` inside `AudioResampler` | Use `new_custom` with fixed input, 512-frame hint, 2 sub-chunks, one channel and `BlackmanHarris2`; compare actual FFT sizes and delay to baseline |
| 4 | Wrap reusable mono slices in borrowed audio adapters | Use Rubato's re-exported adapters; `process_into_buffer` with no indexing; no owned adapter allocations per block |
| 5 | Consume `input_frames_next()` and size scratch using reported maxima | Respect returned consumed/produced counts; do not assume the constructor hint is the required chunk size; retain partial input across calls |
| 6 | Verify every caller and rate-change lifecycle | Preserve equal-rate bypass, frame cutter, queue freshness/caps, gains, mute/deafen, echo reference and existing mixer drift policy |
| 7 | Verify file tail handling independently | Keep existing padding initially; measure tail loss/delay and duration before choosing a precise flush API; no automatic switch to `process_all` |
| 8 | Run regression, hardware and performance checks | No unexplained frame loss, pitch/amplitude change, allocations or latency increase; report performance measurements rather than assuming gains |

`AudioResampler::new` currently returns `None` both for equal rates and for a
construction error. A migration must not introduce additional silent bypasses:
validate the configured rates and exercise construction failures. Any redesign
to propagate these errors must update all callers together and be reviewed as
part of the migration.

Keep fixed-ratio FFT first. Async clock tracking, `Slip`, new DSP behavior and
chunk-size tuning are separate changes. Existing CPAL/CoreAudio pauses, saved
device labels, device fallback and raw WASAPI remain intact.

## Validation gates

| Check | Cases / criterion |
|---|---|
| Same-rate path | 48 -> 48 kHz bypass preserves samples exactly |
| Rate conversion | 8/16/22.05/32/44.1/96 -> 48 kHz and 48 -> 16/44.1/96 kHz; finite output, expected duration and steady-state amplitude |
| Streaming boundaries | Inputs of 1, 127, 480, 512, 1024 and irregular lengths; equivalent output regardless of how the same input is partitioned |
| Frequency response | Impulse and sine fixtures; align output delay before comparisons; verify passband and alias rejection against 0.16.2 |
| Soundboard | Existing stereo-file conversion, short clips, tail impulse, duration cap and local/remote gain independence |
| Device changes | 48 -> 44.1 -> 48 kHz output resets resampler history; CPAL/raw mic switches retain the DM track |
| Real-time cost | CPU per processed audio second and allocations/capacity growth after warm-up; same release profile and inputs |
| Regression | Client/workspace tests; Clippy in both CI crate groups; format and whitespace checks |
| Hardware | Windows CPAL/raw, selected/default devices; macOS Bluetooth teardown; Linux ALSA saved selection and capture/playback |

## Implementation results

| Check | Evidence |
|---|---|
| Legacy equivalence | `features/resampler_tests.rs` compares 1 kHz sine and impulse fixtures against 0.16.2 across 18 rate pairs (8 to 192 kHz); equal output counts and delay, finite samples, maximum sample error below 0.00002 |
| Streaming boundaries | Exact equivalent output for 1, 127, 480, 512, 1024 and mixed input partitions |
| Buffer reuse | All 18 rate pairs retain wrapper/output buffer capacities through 200 calls after warm-up with 10 ms input blocks; borrowed adapters do not own audio buffers |
| Edge cases | Zero input/output rates return `None`; empty calls clear output and retain partial input. Short clips of 1/127/480/511/512/513/1023 samples with existing tail padding match legacy samples/counts |
| Caller behavior | Capture, playback, soundboard and file callers unchanged; existing file padding, queue policy, DSP and device lifecycle retained |
| Dependency scope | Only Rubato 5.0.1 and six required new packages added; DeepFilterNet, tract, CPAL and patched SDK versions retained |
| Regression | Workspace tests passed before review expansion; client suite after rebasing onto `65d2a18`: 528 passed, 17 ignored. Formatting and whitespace checks passed |
| Lint | Both CI crate groups passed Clippy for all targets with `-D warnings` on Windows |
| Windows hardware | Realtek CPAL/raw/CPAL capture switching retains the track and delivers DSP frames; native notification playback starts/stops at 48 kHz. These devices exercise equal-rate bypass, not hardware rate conversion |

Capacity checks do not count every allocator call. Release CPU comparisons and
macOS/Linux hardware checks have not been measured here; no performance or
sound-quality improvement is claimed from the version change alone.

## Upstream references

- [Rubato 5.0.1 and migration notes](https://docs.rs/crate/rubato/5.0.1)
- [FFT configuration and processing API](https://docs.rs/rubato/latest/rubato/struct.Fft.html)

The new API uses audio adapters and exposes configurable FFT construction.
Neither capability alone establishes better sound or lower CPU in this app.

## Pre-PR review, 2026-10-09

| Area | Result |
|---|---|
| Adapter bounds | Mono slices sized from `input_frames_next` / `output_frames_next`; output max reserved; zero-output blocks supported; consumed frames drained only after successful processing |
| Caller lifecycle | Both playback loops reconstruct on device-rate changes; capture conversion precedes the 480-sample frame cutter; file padding and duration cap retained |
| Compatibility | New crates require Rust 1.87; repository already uses `as_chunks` (1.88) and stable CI toolchains. New processing code has no platform-specific branches or native dependencies |
| Production graph | `cargo tree -e normal` confirms 5.0.1 is the app dependency and 0.16.2 is absent; 0.14.1 remains under DeepFilterNet |
| Scope | No CPAL, DSP, codec, bitrate, transport or vendor patch changes; additional tests and documentation only since the migration |

No new runtime regression identified. macOS/Linux compilation and physical
device checks remain unverified on this Windows host.
