# Microphone profiles and guided calibration

Design and implementation, 2026-10-17. Branch: `feature/voice-profiles-calibration`.
Base: `master` at `fb53365`. Profiles, local calibration and before/after playback are
implemented. Preset listening checks on target hardware remain a release validation requirement.

## Product decision

Offer **Ajustar mi micrófono** beside the existing microphone test, plus four input profiles:
**Natural**, **Equilibrado**, **Entorno ruidoso**, **Personalizado**.
Keep advanced controls collapsed. The guided adjustment measures this microphone in this room,
proposes a configuration, and lets the person compare and apply it.

A profile is a processing preset. Calibration estimates useful settings.
Speaker-specific isolation requires a model capable of separating speakers; the current
DeepFilterNet integration supplies noise reduction, so these presets must not promise
to identify the owner or remove every background voice.

## Comparison

Observed UI and public documentation establish features, not proprietary DSP parameters.
The Discord profile names below come from the screenshot supplied with this request.

| App | Documented/observed approach | Lesson for this feature |
|---|---|---|
| Discord | Screenshot: isolation, studio and custom profiles, automatic sensitivity, suppression and advanced controls. [Krisp FAQ](https://support.discord.com/hc/en-us/articles/360040843952-Krisp-FAQ) describes local noise removal and cautions about quality in quiet rooms; [input modes](https://support.discord.com/hc/en-us/articles/211376518-Voice-Input-Modes-101-Push-to-Talk-Voice-Activated) documents automatic/manual sensitivity | Simple choices, manual escape hatch, modest processing when the room is already quiet |
| TeamSpeak | Official [product/SDK overview](https://teamspeak.com/en/solutions/business-sdk/) lists automatic microphone adjustment, noise reduction and echo cancellation | Coordinate the controls; the overview does not disclose an exact tuning algorithm |
| Mumble | [Official site](https://www.mumble.info/) advertises microphone setup wizards; [AudioWizard.cpp](https://github.com/mumble-voip/mumble/blob/master/src/mumble/AudioWizard.cpp) exposes device, level, transmission and local loopback stages | A guided local test is directly relevant to a gaming voice client |
| Zoom | [Microphone profiles](https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0059985): automatic/graded noise removal, personalized isolation and original sound. [Personalized isolation](https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0074698) can use voiceprints | Separate noise reduction, original sound and actual speaker isolation |
| Teams | [Audio settings](https://support.microsoft.com/en-au/teams/meetings/manage-audio-settings-in-microsoft-teams-meetings): automatic sensitivity, test call, background suppression and isolation. [Isolation documentation](https://learn.microsoft.com/en-us/microsoftteams/voice-isolation) describes personalized models/profiles | Let people verify what others will hear; do not label generic suppression as personalized isolation |
| Google Meet | [Noise cancellation](https://support.google.com/meet/answer/9919960?hl=en) targets non-speech noise; [Studio sound](https://support.google.com/meet/answer/14441737) can reconstruct higher frequencies from degraded audio | A quality-restoration model is a separate capability from choosing a gate or gain |
| Slack | [Huddle preferences](https://slack.com/help/articles/1500002037922-Adjust-your-huddles-preferences): gain control, suppression and a speaking-level test | Keep common controls understandable and provide immediate feedback |
| Webex | [Smart audio](https://help.webex.com/en-us/article/yrsw3l/Webex-App-Remove-Background-Noise-and-Speech-in-Calls-and-Meetings/1000): noise removal, own/all voices and music modes | Describe each mode by the intended environment |
| NVIDIA Broadcast | [Setup guide](https://www.nvidia.com/en-us/geforce/guides/broadcast-app-setup-guide/): adjustable effects, recording/playback comparison and guidance against stacking suppression in another app | Use before/after playback and ask about existing virtual-mic processing |

These sources do not provide a universal optimal sensitivity, suppression strength or
private implementation for each product. Candidate settings below are our proposed
starting points, to be validated with listening tests.

## Baseline before implementation

| Existing code | Consequence |
|---|---|
| `features/voice_mic_test.rs` opens local capture/playback and uses the production DSP; it is unavailable during guild/DM calls | Reuse the same exclusive microphone session and teardown rules |
| `features/voice.rs::denoise_gate_loop`: echo/high-pass processing → DeepFilterNet → gate measurement → AGC → manual gain → limiter → gate output | Capture measurements before processing and at the actual gate stage; avoid deriving thresholds from the AGC-amplified output |
| `echo.rs::Echo::new` enables echo cancellation and high-pass filtering unconditionally | Add explicit processing controls before offering a minimal-processing profile |
| `agc.rs`: target RMS 0.126, gain up to 10x, rise 6 dB/s | Calibration should first try fixed gain, addressing the previously observed audible volume ramp without retuning AGC blindly |
| `MicMeter` exposes integer peaks/open state, not distributions, RMS or clipping counts | Add bounded worker-side analysis; UI meter values are insufficient for calibration |
| Saved sensitivity uses peak × 1000; suppression range is 6–60 dB; current defaults include AGC on and suppression 30 dB | Quantize recommendations to the actual gate representation and distinguish suppression disabled from its minimum |
| `voice_direct.rs::update_controls` shares guild/DM/test preferences | One effective configuration must feed all three paths |
| System processing bypass is requested through the raw-input path and can fail/fall back | Record actual capture capability; do not claim the microphone is unprocessed merely because the preference is enabled |

## Guided flow

```mermaid
flowchart LR
    A[Elegir micrófono y salida] --> B[1.5 s de silencio]
    B --> C[6 s leyendo normal]
    C --> D[6 s leyendo suave]
    D --> E[1.5 s de silencio]
    E --> F[Comparar actual y recomendado]
    F --> G[Aplicar o conservar ajustes]
```

Before capture, ask whether output is headphones or speakers and whether the selected
mic already has processing, for example a virtual Broadcast/Krisp device. Start only
after the person presses the button; no microphone capture is needed for this design task.

Both speech stages display the same localized reading passage; soft speech means
lower volume without whispering or changing microphone distance.

During capture, mute local monitoring and network publication. Keep the optional short
keyboard/fan sample separate from speech. The basic sample is 15 seconds; keyboard
sampling and comparison add time. A passive quiet-room sample cannot measure acoustic
echo cancellation, so echo policy follows the declared output setup, not an inferred score.

Hold at most 20 seconds of raw mono 48 kHz float PCM in memory (about 3.84 MB), with no
automatic disk upload/save. Bound all PCM, including preview/scratch buffers, to 16 MiB.
Analyze on a worker. Stop and discard on cancellation,
tab close, device loss/change or entering a call. Retain only applied settings/summary.

Measure 10 ms RMS/peak distributions, input clipping, sample gaps and speech/noise
separation. Reject missing/zero input, inadequate speech, clipping or excessive gaps.
Use median quiet-window RMS to identify normal speech; brief room sounds must not
set that floor. Keep high-percentile noise for gate/gain and preview safety checks.
RNNoise speech probability can support weaker normal/soft frames above the measured
background. Low probability alone never rejects an otherwise usable level measurement.
Silence/speech instructions alone are not proof that all frames contain the expected
signal; require sufficient valid frames and separation.

Evaluate suppression candidates off/12/18/24/30 dB sequentially using fresh, equally warmed
production DSP state over the same buffered input; release rejected outputs/models.
Do not change persisted or
active call settings while evaluating. Measure each candidate's gate-stage residual
noise and quiet-speech peaks; replay candidates with their corresponding gate/gain.

Propose a gate between high-percentile noise and low-percentile quiet speech, with
room for the existing hysteresis. Do not force a threshold when that interval
collapses after integer quantization.
Estimate fixed gain toward roughly -20 to -18 dBFS speech RMS, keeping peaks below
-3 dBFS and within the usable 1–200% microphone volume range (0% is mute).
Quiet inputs that need more than 200% receive a limited-gain warning.

Choose the least suppression that reduces measured room noise while preserving
quiet speech. Energy metrics are diagnostic, not an intelligibility score:
human before/after listening is required to judge muffling and lost consonants.
Automatic suggestions in noisy rooms retain suppression and a safe speech gate.
Gain is capped by the current audible noise floor, with a -70 dBFS measurement floor
and a -44 dBFS ceiling. Final PCM is checked after gain and gating; a louder room
is rejected even if speech/noise ratio improves. Unknown soft speech or unsafe gates
keep existing settings instead of offering unfiltered amplification. No-improvement
results offer current playback without an Apply action.
Silence, clipping, gaps and insufficient normal speech still fail.
Offer **Escuchar actual**, **Escuchar recomendado**, **Aplicar**, **Repetir**,
**Conservar mis ajustes**. Compare the same phrase; any comparison loudness matching
uses one constant gain per clip, never dynamic normalization that could hide pumping.

## Profiles

Candidate suppression values are experimental, not equivalents of Discord/Krisp controls.

| Profile | Suppression | Level and transmission | Echo |
|---|---|---|---|
| Natural | Off | Fixed calibrated gain; bypass the activity gate to preserve soft speech/music | Off with declared headphones; on with speakers |
| Equilibrado | Lowest validated candidate, initially testing 12–18 dB | Calibrated gate and fixed gain; continuous AGC off | According to output setup |
| Entorno ruidoso | Test 24–30 dB, reducing it if speech is damaged | Calibrated gate/fixed gain; no automatic move to 60 dB | According to output setup |
| Personalizado | Preserve every manual choice | Expose existing controls plus explicit echo/gate policy | Manual |

## Optional voice activation

Advanced settings offers **Use voice detection (experimental)**. Enabling it also
enables the transmission gate; existing saved configurations default to level-only
activation. It reuses the post-echo RNNoise probability, independent of suppression.
Two consecutive 10 ms frames at probability ≥0.6 and the selected sensitivity open
the gate; probability ≥0.35 and half that level sustain it. Release holds 300 ms.
This detects speech, not the identity of its speaker.

`voice_activity.rs` retains at most three processed hops (30 ms) before opening,
including their original capture stamps/positions. The first retained hop fades in.
Mute, zero gain, configuration changes, stale input and capture reopens discard it.
Channel calls, DMs, local monitoring and calibration previews share this behavior;
preview metrics include the restored leading samples. VAD-gated recommendations
also need measurable normal and soft output, so silence cannot win on noise alone.
Sensitivity remains a minimum level; local listening and real-call validation are
required before assuming quiet words are preserved on a particular microphone.

Natural retains resampling, mono voice transport and an overload limiter; the UI
must describe those limits rather than promise lossless studio capture. Existing
external suppression favors Natural or a low-processing recommendation.
Manual changes switch to Personalizado. Leaving it snapshots the manual DSP/volume
configuration separately from the active preset; returning restores that snapshot,
including after a restart. Legacy Custom values are captured on the first switch. A profile click must not overwrite mute,
deafen, input/output selection, network bitrate, or camera settings.

## Implementation sequence and acceptance

| Step | Change | Acceptance |
|---|---|---|
| 1 | Add serializable profile/config model and one apply operation in `settings.rs`, `state.rs`, `voice.rs`, `voice_direct.rs` | Old settings load as Personalizado with identical values; no silent reset to new defaults |
| 2 | Make echo/high-pass and activity-gate behavior explicit; apply one coherent config at a DSP frame boundary | Guild, DM and local playback use identical settings; no partially applied preset or unnecessary capture restart |
| 3 | Add bounded analysis and pure recommendation rules in a new `voice_calibration.rs` module | Deterministic silence/clipping/noise/quiet-speech/quantization tests; audio callback never runs analysis/model loading |
| 4 | Extend the existing test session with capture, candidate playback and draft settings | Preview does not persist; Apply writes one config; cancel/error preserves settings and releases the device |
| 5 | Add profile selector, guided button and collapsed advanced controls in `settings_dialog.rs` | Keyboard-accessible progress/status, explicit recommendations, English/Spanish/Portuguese labels |
| 6 | Bind calibration to the actual microphone/capture backend and processing context | Resolving a new default device or changing bypass/backend invalidates the old result; duplicate names do not silently select another mic |
| 7 | Validate real speech and device/call lifecycle | No lost quiet syllables, no new pumping/clipping, quiet/keyboard/fan scenarios, headphones/speakers, Windows/macOS/Linux; report untested hardware separately |

Persist profile, effective parameters, calibration schema/version and device/context
identity locally. Use a stable device ID where available, with a conservative
backend/name fallback; ambiguous identity requires recalibration. Output changes
invalidate echo assumptions. Existing saved preferences remain intact until Apply.
Do not create voiceprints or add cloud processing for this feature.

Reuse `voice_audio_tests.rs`, `voice_speech_tests.rs`, `dm_call_tests.rs` and
the existing microphone-test teardown checks. Include a fixed-input level-envelope
regression for the earlier pumping symptom. Compare DSP output against the current
pipeline, then test the applied configuration through a real local guild/DM call.
Run formatting, translation coverage, both Clippy groups and the relevant client
tests. Initial preset strengths are 15 dB (balanced) and 30 dB (noisy); the guided
adjustment evaluates the candidate set above. These are starting values, with
manual override and before/after listening, rather than universal optimal values.

## Settings layout

The dialog grows from 780 × 560 to 920 × 660 (about 18% per dimension), constrained
by the viewport. Its sidebar stays fixed and each tab scrolls independently.
Microphone order is input → output setup/profile → test/calibration → volume →
voice quality → collapsed advanced processing. Audio keeps output, app sounds and
soundboard. Small helper text is at least 12 px; controls have consistent
spacing and keyboard focus. New labels include Spanish and Portuguese.

## Validation notes

Windows visual review used the real settings components in an isolated window and
preferences directory, without an account or microphone recording. Audio, microphone
profiles and expanded advanced controls were inspected.

The opt-in `recorded_speech_produces_a_bounded_stable_recommendation` test takes
`DISCORDIA_MIC_AUDIT_WAV`: mono PCM WAV with 3 s quiet, 7 s normal speech, 3 s soft
speech and 2 s quiet at 48 kHz. A controlled fixture built from the public
[whisper.cpp JFK sample](https://github.com/ggml-org/whisper.cpp/blob/master/samples/jfk.wav)
passed with fixed gain and retained soft speech. Its 16 kHz source validates speech
handling, not full-band microphone fidelity. Real-device listening remains required.
