"""Generate synthetic Opus regressions; requires development-only av and numpy."""
from pathlib import Path

import av
import numpy as np


def main():
    output = Path(__file__).resolve().parents[1] / "client/tests/fixtures"
    rate, length = 48_000, 19_200
    time = np.arange(length, dtype=np.float64) / rate
    random = np.random.default_rng(73041)
    for mode, name in enumerate(("opus-chirp-noise", "opus-stereo-phase", "opus-transients")):
        if mode == 0:
            left = .35 * np.sin(2 * np.pi * (150 * time + 5000 * time * time)) + .07 * random.normal(size=length)
            right = .32 * np.sin(2 * np.pi * (2700 * time - 2200 * time * time)) + .05 * random.normal(size=length)
        elif mode == 1:
            left = .32 * np.sin(2 * np.pi * 233 * time) + .12 * np.sin(2 * np.pi * 3400 * time)
            right = np.roll(left, 71) * np.sin(2 * np.pi * 7 * time)
        else:
            envelope = np.exp(-((np.arange(length) % 2400) / 300) ** 2)
            left = random.uniform(-1, 1, length) * .5 * envelope
            right = np.roll(left, 31) * .7 + .05 * np.sin(2 * np.pi * 1930 * time)
        samples = np.array([left, right], dtype=np.float32)
        path = output / f"{name}.opus"
        with av.open(str(path), "w", format="ogg") as container:
            stream = container.add_stream("libopus", rate=rate)
            stream.layout = "stereo"
            stream.bit_rate = 32_000
            stream.codec_context.options = {"application": "audio", "vbr": "on", "compression_level": "10"}
            for start in range(0, length, 960):
                frame = av.AudioFrame.from_ndarray(samples[:, start:start + 960].copy(), format="fltp", layout="stereo")
                frame.sample_rate, frame.pts = rate, start
                for packet in stream.encode(frame):
                    container.mux(packet)
            for packet in stream.encode(None):
                container.mux(packet)
        with av.open(str(path)) as container:
            stream = container.streams.audio[0]
            decoder = av.CodecContext.create("libopus", "r")
            decoder.extradata = stream.codec_context.extradata
            decoder.sample_rate, decoder.layout = rate, "stereo"
            decoder.options = {"request_sample_fmt": "flt"}
            decoder.open()
            parts = []
            for packet in container.demux(stream):
                for frame in decoder.decode(packet):
                    samples = frame.to_ndarray().reshape(-1, 2)
                    parts.append(samples.mean(axis=1))
            mono = np.concatenate(parts)
        # Sparse samples retain the waveform comparison without storing full decoded clips.
        path.with_suffix(".reference.f32").write_bytes(mono[::16].astype("<f4").tobytes())


if __name__ == "__main__":
    main()
