# Recorded microphone replay

Private WAVs and generated comparisons stay local under ignored `target/`.
No production DSP behavior changes in this experiment.

```powershell
$env:DISCORDIA_AUDIO_REPLAY_WAV = 'C:/path/to/mic-raw.wav'
$env:DISCORDIA_AUDIO_REPLAY_OUTPUT = "$PWD/target/mic-replay"
cargo test -p discordia --bin Discordia replay_tests -- --include-ignored --nocapture
python scripts/analyze-mic-replay.py --raw $env:DISCORDIA_AUDIO_REPLAY_WAV --recorded 'C:/path/to/mic-sent.wav' --replays $env:DISCORDIA_AUDIO_REPLAY_OUTPUT --interval 7:8 --interval 15:15.5 --interval 20:40 --interval 46:50
python -B scripts/test-mic-replay.py
```

| Control | Value |
|---|---|
| Input | Mono PCM16, 48 kHz, whole 10 ms hops |
| Manual gain / AGC | 125% / off |
| Echo / high-pass | Off / on |
| Gate | Threshold 2; separate no-gate comparison |
| Engines | DeepFilterNet and RNNoise, each at 6/12 dB |
| Warmup | Cold start; separate DeepFilterNet 12 dB run repeats the first 5 s |
| Runtime fallback | Disabled only in replay so machine load cannot change the engine |
| Output | WAV, per-hop stages, configuration/timing JSON, aligned 500 ms levels |

## Local observation, 2026-10-19

57.26 s input; the user identifies speech at 7–8 s and 15–15.5 s.
Values below are output/input RMS differences after alignment, including manual
gain and high-pass. They are not scores against a clean speech reference.

| Output | 7–8 s | 15–15.5 s | 20–40 s | 46–50 s, low input |
|---|---:|---:|---:|---:|
| Original sent recording | -7.39 dB | -11.49 dB | +1.48 dB | -4.94 dB |
| Replay, no denoiser/gate | +1.47 | +0.50 | +1.53 | -2.84 |
| DeepFilterNet 12 dB | -4.29 | -11.49 | +1.49 | -4.90 |
| DeepFilterNet 6 dB | -2.10 | -5.50 | +1.50 | -4.29 |
| RNNoise 12 dB | +1.42 | -7.34 | +1.51 | -4.22 |
| RNNoise 6 dB | +1.43 | -4.03 | +1.51 | -3.84 |
| Experimental protection, DeepFilterNet 12 dB | -0.48 | -6.71 | +1.50 | -4.85 |

The no-gate run retains the 15 s dip; prefix warmup also retains it. The replay
matches that dip but not the exact 7 s attenuation: pre-recording model state is
unavailable. Delay: DeepFilterNet 31 ms, RNNoise 11 ms, baseline 1 ms.

The independent VAD's median speech probability at 15–15.5 s is 0.10 despite
confirmed speech. A VAD-only bypass would miss it. High VAD values also occur
at low input levels; probability alone is insufficient.

## Experimental protection

Test-only sustained-energy/VAD controller, 120 ms qualification, 350 ms hold,
fast reduction of the attenuation cap toward 3 dB and gradual return to the
requested 12 dB. It changes the filter cap, never amplifies filtered audio.
Quiet false positives and short impulses have deterministic coverage.

It improves the measured speech intervals and retains similar low-input
suppression in this recording, but leaves a substantial onset dip at 15 s.
Sustained loud non-speech could also trigger it. This is not a production fix:
broader speech-plus-noise and onset listening validation are still required.

Energy masks (`raw > -40 dBFS`, `raw < -52 dBFS`) in the analyzer are not semantic
speech/noise labels. Offline processing times do not establish real-time safety.
