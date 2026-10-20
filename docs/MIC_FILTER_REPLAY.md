# Recorded microphone replay

Private WAVs and generated comparisons stay local under ignored `target/`.
The replay uses production microphone DSP; only timing-triggered fallback is disabled.

```powershell
$env:DISCORDIA_AUDIO_REPLAY_WAV = 'C:/path/to/mic-raw.wav'
$env:DISCORDIA_AUDIO_REPLAY_OUTPUT = "$PWD/target/mic-replay"
cargo test -p discordia --bin Discordia recorded_microphone_filter_comparison -- --ignored --nocapture
python scripts/analyze-mic-replay.py --raw $env:DISCORDIA_AUDIO_REPLAY_WAV --replays $env:DISCORDIA_AUDIO_REPLAY_OUTPUT --interval 7:8 --interval 15:15.5 --interval 20:40 --interval 46:50
```

| Control | Value |
|---|---|
| Input | Mono PCM16, 48 kHz, whole 10 ms hops |
| Manual gain / AGC | 125%; AGC off except `deep-12-agc` |
| Echo / high-pass | Off / on |
| Gate | Threshold 2; separate no-gate comparison |
| Engines | DeepFilterNet and RNNoise at 6/12 dB |
| Warmup | Cold start; separate DeepFilterNet 12 dB run repeats the first 5 s |
| Output | WAV, per-hop stages/effective suppression, configuration/timing JSON, aligned 500 ms levels |

## Speech protection

Production DeepFilterNet protection combines raw input energy and the previous
post-echo VAD score before AGC. Four qualifying 10 ms hops trigger a 350 ms hold.
The attenuation cap drops toward 3 dB at 4.5 dB/hop and returns to the requested
value at 0.1 dB/hop. RNNoise fallback retains the requested suppression.
Mute, silence, retry, disabling suppression and engine changes reset protection;
changing requested suppression resets its detector. AGC cannot affect its decisions.

It retains more speech energy in the user's 7-8 s and 15-15.5 s intervals, but
may also retain sustained loud background sounds. It does not reconstruct lost
speech. No clean speech reference is available. Energy masks in the analyzer
are not semantic speech/noise labels; offline timing does not establish realtime safety.
