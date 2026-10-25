# Microphone diagnostics

Enable Diagnostics → Record calls. Files join the active recording mid-call;
capture reopens create suffixed files. All microphone PCM is mono 48 kHz.

| File | Meaning |
|---|---|
| `mic-raw.wav` / `mic-capture.csv` | Capture after channel conversion/resampling, before the DSP queue; includes blocks rejected by that queue |
| `mic-processed.wav` / `mic-processing.csv` | Gated DSP output; closed/stale input blocks are silence. Model-load discards have metadata without waveform samples |
| `mic-accepted.wav` / `mic-publication.csv` | Only blocks accepted by `NativeAudioSource::capture_frame`; this is not proof of network transmission or remote playback |
| `mic-dsp.csv` | Stage levels, gate, gain, model, speech probability and processing settings, including `voice_detection`; `processed_dbfs` describes DSP output before lead-in routing |
| `recv-<who>-<kind>.wav` / `.csv` | Each remote track as decoded, before resampling and mixing. `mixer_ms` is the queue depth after the push, `rebuffered_samples` what recovery held, `ratio` the drift controller's resample ratio (1 ± 0.01) |
| `net-*.csv` | Cumulative transport statistics in guild voice; not available for direct calls |

`mic-sent.wav` belongs to older recordings and described pre-publication DSP
output. Do not interpret it as transmitted audio.

## Correlation

| Column | Meaning |
|---|---|
| `t_ms` | Recorder session clock when the row is submitted |
| `capture_id` | One capture opening; changes on reopen |
| `first_sample` | Position in that capture's resampled stream, including queue drops; increases by 480 per block |
| `samples` | Block length, currently 480 |
| `age_ms` | Time since capture stamping when metadata is formatted, including diagnostic-worker delay for capture rows |
| `event` / `event_count` | Outcome and its cumulative count within this file/session |
| `wav_first_sample` | Position in the matching WAV; empty when the event has no recorded PCM |
| `record_dropped_samples` | Cumulative missing diagnostic blocks in the capture recorder and/or writer queues; separate from live audio loss |

Join stages on `(capture_id, first_sample)`, not WAV length or row order. Capture
positions cover samples delivered through conversion; device/driver losses before
delivery cannot be reconstructed from these indices.

| Stage | Outcomes |
|---|---|
| Capture | `queued`, `capture_full`, `capture_closed` |
| DSP | `queued`, `lead_in_queued`, `gate_drop`, `stale_input`, `stale_processed`, `model_load_discard`, `publisher_full`, `publisher_closed` |
| Publication | `accepted`, `stale_publish`, `source_error` |

The device callback sends fixed blocks to a bounded diagnostic queue. A slow
recorder drops diagnostic blocks without waiting. Each waveform block and its
position row share one writer operation; rejected operations do not advance WAV
offsets. Queue losses become visible in the next successful row; a final loss
without a later row has no trailing counter. Disk errors remain possible and
are not evidence of live-audio failure.

With voice detection enabled, a previously closed block can later be queued as
lead-in. Its capture position stays unchanged; the processing WAV may therefore
contain both its earlier silent `gate_drop` and its later `lead_in_queued` audio.
Publication rows distinguish which blocks were actually accepted by WebRTC.

## Startup and device changes

The DSP worker prepares enabled suppression before capture opens. Channel calls
and local monitoring wait asynchronously, bounded at 20 seconds; direct calls
open capture from their maintenance loop after the worker reports readiness.
Explicit noise-suppression retries during a call can still discard queued audio;
the processing trace identifies those blocks.

Windows RAW capture observes the default capture endpoint by stable endpoint ID,
including a change while the previous endpoint stays connected. A selected
named endpoint remains explicit; a missing selection that falls back to the
default follows that default. Notification registration is checked against the
current default to close the opening race. The session reopens capture without
replacing its published track; physical device transitions still need testing.
