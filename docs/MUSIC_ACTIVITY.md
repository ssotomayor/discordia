# Music activity

Current phase: installable shared-queue prototype, named **Music Lab**. Playback
and collection expansion are not connected to the activity. An optional native
audio engine and encrypted transport have been tested independently. Nothing
starts with Discordia or the gateway by default.

## Optional installation

```powershell
cargo run -p discordia-music --features activity --bin discordia-music-activity -- --server 'ws://127.0.0.1:9000'
```

Off loopback, use the host's `quic://…` share string (or its TLS-proxied URL).
The operator runs this process independently, like the Wizards example. It
prints its public app key; a guild manager installs that key in
**Integrations → Install an app**. No commands, guild intents or guild
permissions are requested: only the activity's consented `Instance` capability.
The service supplies its signed, hash-pinned HTML bundle through `BundleWanted` /
`OfferActivityBundle`; no public download URL or automatic install is needed.

`music.key` (or `--key-file` / `MUSIC_KEY_FILE`) is the service's secret identity:
keep it private and preserve it across restarts. Unix creation uses mode 0600;
on Windows it inherits the directory's permissions. An invalid existing file
fails startup; it is never overwritten or replaced. Ctrl+C stops the service.

| Prototype behavior | Limit |
|---|---|
| Independent sessions | One per channel, 16 per service; follows existing activity channel visibility |
| Queue | 8 single-track links; no pretend album/playlist imports |
| Controls | Players add/remove their own tracks; first current player hosts, reorders/removes any/clears |
| Metadata workers | 4 per service, at most 2 pending tracks per person per session |
| Repeated commands | Last 64 successful mutations deduplicated by participant + request UUID |
| Session data | Memory only; last participant leaving ends the session |
| Service reconnect | Re-registers the same app, receives own instances; queue resets with a new epoch and UI notice |
| Media | No voice tokens, microphone access, audio publication or streaming credentials |
| Client | Existing isolated activity window, with Wizards' current platform support; Android activity windows are not supported yet |

Interface labels and notices identify this as a metadata/queue prototype.
Spectators receive state but cannot issue commands. No playback controls are
shown. URLs and returned titles render as text, never HTML, embeds or thumbnails.
Request acknowledgements are correlated, source jobs cancel when removed, and
late results cannot resurrect deleted entries. Bundle changes require a newer
signed manifest version.

## Source validation

| Source | Recognized links | Public metadata probe | Audio / playlist entries |
|---|---|---|---|
| YouTube | Videos, shorts, live links, playlists | Tracks only, oEmbed | Not validated |
| SoundCloud | Public-form track and set links | oEmbed | Not validated |
| Spotify | Tracks, albums, playlists, localized URLs | oEmbed | Not validated |
| TIDAL | Tracks, albums, UUID playlists | No adapter; `unsupported_probe` | Not validated |

Recognition checks the URL's structure, not the existence, public visibility,
licensing or availability of its content. Shortened redirect links are rejected.
A YouTube video linked within a playlist selects that video, not the collection.
Canonical links drop query tracking, timestamps, fragments and private-link tokens.

```powershell
# Offline URL validation; no provider requests.
cargo run -p discordia-music -- 'https://youtu.be/dQw4w9WgXcQ'

# Explicit public metadata lookup, JSON report; no audio or embed execution.
cargo run -p discordia-music -- --probe 'https://soundcloud.com/forss/flickermood'

cargo test -p discordia-music
cargo clippy -p discordia-music --all-targets -- -D warnings

# Optional service, queue, key and local gateway installation regression tests.
cargo test -p discordia-music --features activity
node --test music/tests/activity_ui.cjs
```

At most 32 links per invocation, processed sequentially. Invalid input rejects
the invocation before any lookup. Probe exit status is nonzero if any source
does not return valid metadata. Offline success means only that links parse;
probe success means only that metadata was returned.

Probes use fixed provider endpoints, HTTPS, no redirects, no environment proxy,
a 5 s connection timeout, 10 s total request budget and 64 KiB response cap,
including chunked bodies. Provider HTML, thumbnails and supplied content URLs
are never fetched or executed. Reports omit raw network errors and response
bodies. Titles are untrusted text. No request authentication or cookie store.

## Architecture decisions

| Boundary | Direction |
|---|---|
| Runtime | Rust service explicitly enabled by the operator; guild installs never execute code in the gateway (trap 39) |
| Authority | Installable activity with an external Rust backend; current queue inherits channel access, voice participation will be part of the media boundary |
| Client | Installed isolated bundle, as with Wizards; media support must not open third-party activity networking or Dioxus IPC |
| Audio | One stereo publication per session; reuse Symphonia, resampling and LiveKit where compatible |
| Privacy | Dedicated music credentials/key scope; no microphone subscriptions or voice-room keys for the music service |
| Resources | Bounded queues and decode buffers, streaming reads, cancellation and explicit teardown |
| Sources | Distinguish link recognition, metadata, collection expansion and actual playback |

No external engine selected or vendored. Ibuki is an alpha reference for a Rust
node; Songbird's Discord transport does not match Discordia. RustyPipe's
GPL-3.0 license needs compatibility review before adoption. Native Rust does
not establish a provider's authorization for retransmission. A successful
oEmbed response grants neither audio access nor shared-listening rights.

If source matching is introduced, show the actual provider/version; do not
silently replace tracks. The current probe performs no cross-provider matching.

## Evidence — 2026-11-26

Live metadata requests returned HTTP 200 and nonempty titles for YouTube
`dQw4w9WgXcQ`, SoundCloud `forss/flickermood`, and Spotify
`4uLU6hMCjMI75M1A2tKUQC`, without music account credentials. These are
single-item observations, not platform-wide availability guarantees. All
reports retain `audio_verified: false` and `playlist_entries_verified: false`.
No live TIDAL request or music playback test was performed.

Local gateway tests cover explicit installation (absent before install), bundle
delivery, two participants sharing a queue, command deduplication, host handoff,
and service reconnect. UI bridge tests cover sender validation, untrusted text,
command acknowledgement, timeout, spectator controls and restart epochs.

Primary references: [SoundCloud oEmbed](https://developers.soundcloud.com/docs/oembed),
[Spotify oEmbed](https://developer.spotify.com/documentation/embeds/reference/oembed),
[YouTube developer policies](https://developers.google.com/youtube/terms/developer-policies/),
[Ibuki](https://github.com/Deivu/Ibuki),
[RustyPipe](https://codeberg.org/ThetaDev/rustypipe),
[Songbird](https://github.com/serenity-rs/songbird).

## Native audio prototype

| Feature | Scope |
|---|---|
| `audio` | Symphonia decoder, stereo resampling, bounded producer queue, playback state and publication clock |
| `transport` | Adds the existing native LiveKit SDK, a dedicated-room publisher and an independent E2EE key provider |
| Default / `activity` alone | Neither audio feature is enabled; the installed bundle still says audio is not connected |

`DecodedStream::file` reads a regular local file on a blocking worker. It does
not resolve a provider URL or accept file paths from the installed activity.
Enabled decoders: PCM/WAV, MP3, Ogg/Vorbis; unsupported codecs/layouts fail.
Mono duplicates to both channels; stereo stays separate. Output is interleaved
48 kHz signed 16-bit PCM in 10 ms frames; only the last frame is zero-padded.
Resampling removes its own startup delay and preserves the converted duration.
Floating-point values must be finite; out-of-range peaks clamp to PCM full scale.

| Resource/control | Behavior |
|---|---|
| File | At most 512 MiB; read incrementally, never loaded whole |
| Packet | At most 1 MiB encoded / 32,768 decoded frames; not a process-wide decoder memory guarantee |
| Metadata | Tags capped at 16 KiB; embedded pictures disabled |
| PCM queue | 50 frames / 500 ms, about 96 KiB of sample payload; full queue blocks the decoder |
| Input | 8–192 kHz, one or two channels; format/rate changes fail |
| Pause | Returns silence for the publication loop without advancing song position; decoder stops at its queue limit |
| Replacement/stop | Closes the old receiver and waits for the worker; stale PCM cannot enter the replacement |
| Publication clock | Advances through silence; short scheduling slips catch up, >250 ms reports `ClockStalled` rather than silently falling behind |
| Publisher | 192 kbit/s Opus ceiling, no microphone AEC/noise suppression/AGC, no automatic subscriptions |

The publisher takes an explicit music-room token and its own 32-byte key. It
does not mint activity grants, borrow voice tokens/keys, or expose networking
to the activity window. Current activity commands do not invoke it: permission
and grant integration has not been implemented. Owners must close the publisher
and stop the decoder when their media session ends. Probe/queue capabilities
are unchanged.

```powershell
# Local decode validation only; no playback, upload or publication.
cargo run -p discordia-music --features audio --bin discordia-music-audio-check -- 'C:\Music\example.wav'
cargo test -p discordia-music --features activity,audio

# Needs the same native C++ toolchain as the client.
cargo clippy -p discordia-music --all-features --all-targets -- -D warnings
cargo test -p discordia-music --all-features
```

Synthetic PCM tests cover 8 / 44.1 / 48 / 96 / 192 kHz conversion, channel
separation, exact duration/partial tails, an 11-second input, bounded buffering,
cancellation, pause, replacement and stalled clocks. MP3/Vorbis are enabled
but have not been validated with provider audio.

An opt-in native network test (`music/tests/media.rs`) was run on Windows against
an isolated local LiveKit 1.13.9 server: two receivers decoded separate 440/880 Hz
stereo tones, including a silence interval; a third receiver with a different
key did not decode the signal. No microphones, speakers or user streams were
used. The temporary server was stopped and its exit verified.

The network test is ignored by default and requires `MUSIC_TEST_SFU_URL` pointing
to loopback plus `MUSIC_TEST_API_KEY` / `MUSIC_TEST_API_SECRET` for that dedicated
test server. It creates its own room and restrictive publisher/subscriber
grants; it never starts or attaches to an ordinary user's voice session.

```powershell
cargo test -p discordia-music --all-features --test media -- --ignored --test-threads=1
```
