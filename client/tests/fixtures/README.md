# Sound decoding fixtures

Synthetic 440 Hz tones; no third-party recordings. FFmpeg is only a fixture
generation tool, not an application dependency.

| File | Signal |
|---|---|
| `tone-stereo.opus` | Existing one-second stereo Opus tone, approximately half scale |
| `tone-silence-stereo.mp3` | One second, 44.1 kHz; left 0.5/right 0.25; silence from 0.3 to 0.7 s; ID3v2 title |
| `tone-stereo.ogg` | One second, 44.1 kHz Vorbis; left 0.5/right 0.25 |

Generate the MP3 and Vorbis fixtures from the repository root:

```sh
ffmpeg -f lavfi -i 'aevalsrc=if(between(t\,0.3\,0.7)\,0\,0.5*sin(2*PI*440*t))|if(between(t\,0.3\,0.7)\,0\,0.25*sin(2*PI*440*t)):s=44100:d=1' -c:a libmp3lame -b:a 128k -metadata title='Soundboard regression tone' -id3v2_version 3 -y client/tests/fixtures/tone-silence-stereo.mp3
ffmpeg -f lavfi -i 'aevalsrc=0.5*sin(2*PI*440*t)|0.25*sin(2*PI*440*t):s=44100:d=1' -c:a libvorbis -q:a 4 -y client/tests/fixtures/tone-stereo.ogg
```

Header variants used by the Opus tests recompute the Ogg page checksum.
