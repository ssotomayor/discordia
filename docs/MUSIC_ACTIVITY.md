# Music activity

Current phase: source feasibility. `discordia-music` is an isolated CLI/library,
not an installed activity or an audio player. Nothing executes in the gateway,
and no UI, protocol frames, account credentials or voice keys have changed.

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
| Authority | One ephemeral music session per voice channel, server-checked access and controls |
| Client | Trusted native Dioxus UI for desktop/mobile; preserve third-party activity isolation |
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

Primary references: [SoundCloud oEmbed](https://developers.soundcloud.com/docs/oembed),
[Spotify oEmbed](https://developer.spotify.com/documentation/embeds/reference/oembed),
[YouTube developer policies](https://developers.google.com/youtube/terms/developer-policies/),
[Ibuki](https://github.com/Deivu/Ibuki),
[RustyPipe](https://codeberg.org/ThetaDev/rustypipe),
[Songbird](https://github.com/serenity-rs/songbird).
