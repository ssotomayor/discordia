# CPAL migration

Base: `master` at `d311c01`. Target: CPAL 0.18.2 from 0.15.3.

| Order | Work | Acceptance |
|---|---|---|
| 1 | Record client test baseline; update CPAL and lockfile | Only required dependency changes |
| 2 | Adapt voice, notification playback and `bt_probe` APIs | All client targets compile |
| 3 | Select supported PCM formats explicitly; preserve saved device names | Unsupported defaults fall back to supported configurations |
| 4 | Classify callback errors in existing guild/DM recovery | Transient underruns do not reopen streams; device loss does |
| 5 | Preserve explicit teardown and Windows raw microphone path | Device switches retain mute and the published DM track |
| 6 | Run regression tests, lint and platform checks | Report hardware checks separately from headless results |

The existing 2 s reopen retry and 5 s guild device scan remain. Processing stays
at 48 kHz with existing resampling. New Linux backends, persistent device IDs,
LiveKit upgrades and DSP changes are outside this migration.

| Platform | Hardware checks |
|---|---|
| Windows | CPAL/raw input, USB unplug/replug, default/selected devices, repeated calls |
| macOS | Selected/default devices, repeated teardown, Bluetooth release using `bt_probe` |
| Linux | Existing backend input/output, unplug/replug |

Integration checks cover guild voice over direct/TURN routes, private calls,
notifications, soundboard and shared audio. Keep the CoreAudio workarounds until
device checks establish they are unnecessary.

## Implementation

CPAL 0.18.2 and its platform dependencies are locked. ALSA retains its previous
PCM names so saved selections still resolve; capability scans avoid opening ALSA
plugins. `audio_device` preserves
compatible default configurations and selects supported PCM when a default format
has no callback. Device labels remain name-based. `Xrun`, automatic route changes
and scheduling refusal do not trigger guild, DM or notification stream recovery.
Explicit pauses and the default-handle preference remain; Windows raw capture is
unchanged. `bt_probe` accepts the microphone's actual sample format.

## Validation

| Check | Result |
|---|---|
| Client headless tests | 508 passed, 17 ignored; `RUST_LOG` unset |
| Workspace Clippy, all targets | Passed with `-D warnings` |
| Formatting and diff whitespace | Passed |
| Workspace tests | Passed, including integration tests; hardware/SFU-only cases remain ignored |
| Windows DM microphone/bypass switch | Passed on Realtek input/output; CPAL/raw switches retain the published track |
| Windows notification playback and teardown | Passed three runs at 48 kHz; callbacks stop after drop |
| Windows USB reconnect and repeated calls | Pending hardware checks |
| macOS/Bluetooth and Linux devices | Requires those platforms |

## Compatibility review

| Area | Result |
|---|---|
| CoreAudio workaround | Default-handle preference and explicit mic/mixer/notification pauses retained |
| Device recovery | Guild scan, guild/DM retry delays and fallback retained; WASAPI default changes report `StreamInvalidated`, which reopens |
| Saved selections | ALSA PCM identifiers retained instead of the new friendly descriptions |
| Device enumeration | Uses `supports_input`/`supports_output`; ALSA capability scans avoid opening plugins |
| Audio processing | Resampling, channel conversion, queues, mute/deafen, gains, DSP and echo reference unchanged |
| Local patches | LiveKit, WebRTC and Dioxus patches unchanged; no vendored CPAL patch existed |

## Revalidation on current master

2026-10-09, `master` at `4227605`; CPAL 0.18.2 unchanged.

| Check | Result |
|---|---|
| Client tests, locked/offline, `RUST_LOG` unset | 517 passed, 17 ignored, 0 failed |
| Client/grid Clippy, all targets | Passed with `-D warnings` |
| Native notification output | Passed at 48 kHz; playback consumed the tone and stopped after drop |
| DM CPAL/raw/CPAL switches | Passed on Realtek microphone and digital output; DSP frames delivered and track retained |
| Capture xruns during switching | Observed without stream teardown; expected transient classification |
| macOS/Linux, Bluetooth and USB reconnect | Not executed on this Windows host; retain existing platform workarounds |

Rubato migration is planned in `RUBATO_UPGRADE.md`; no audio dependency or
runtime implementation changed during this revalidation.
