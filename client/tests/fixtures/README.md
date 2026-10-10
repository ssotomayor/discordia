# Media fixtures

Synthetic tones and animation; no third-party recordings or images. FFmpeg
and Pillow are fixture generation tools, not application dependencies.

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

`optimized-profile.gif` is a synthetic 384×384 animation (163,320 bytes),
with 28 frames, a static patterned background and small moving rectangles.
It repeats twice; frame delays range from 50 to 320 ms. Exporting every
composited frame reproduces the size inflation of optimized GIF uploads.

Recreate with Python and Pillow (development tools only):

```python
from PIL import Image
import random

rng = random.Random(124)
palette = [v for i in range(64) for v in (i * 4, (i * 37) % 256, (i * 71) % 256)]
base = Image.new("P", (384, 384))
base.putpalette(palette + [0] * 576)
base.putdata([rng.randrange(64) for _ in range(384 * 384)])
frames = []
for i in range(28):
    frame = base.copy()
    frame.paste(i % 64, (8 + i * 4, 8, 24 + i * 4, 24))
    frames.append(frame)
frames[0].save("client/tests/fixtures/optimized-profile.gif", save_all=True,
               append_images=frames[1:], duration=[50 + i * 10 for i in range(28)],
               loop=2, optimize=True, disposal=1)
```
