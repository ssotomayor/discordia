"""Compare local mono PCM16 replay exports. Requires NumPy; never uploads audio."""

import argparse
import csv
import json
import wave
from pathlib import Path

import numpy as np


def read(path):
    with wave.open(str(path), "rb") as source:
        if (source.getnchannels(), source.getsampwidth(), source.getframerate()) != (1, 2, 48000):
            raise ValueError(f"Expected mono PCM16 48 kHz: {path}")
        return np.frombuffer(source.readframes(source.getnframes()), dtype="<i2").astype(float) / 32768


def align(raw, sent):
    if not np.any(raw) or not np.any(sent):
        length = min(len(raw), len(sent))
        return raw[:length], sent[:length], 0, 0
    a, b = raw[::6], sent[::6]
    size = 1 << (len(a) + len(b) - 1).bit_length()
    corr = np.fft.irfft(np.fft.rfft(b, size) * np.conj(np.fft.rfft(a, size)), size)
    lags = np.arange(-960, 961)
    delay = int(lags[np.argmax(corr[lags % size])]) * 6
    start = max(0, -delay)
    end = min(len(raw), len(sent) - delay)
    return raw[start:end], sent[start + delay:end + delay], start, delay


def db(samples):
    return float(10 * np.log10(max(float(np.mean(samples * samples)), 1e-12)))


def span(value):
    start, end = map(float, value.split(":"))
    if not 0 <= start < end:
        raise ValueError("Interval must be start:end in seconds")
    return start, end


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--raw", type=Path, required=True)
    parser.add_argument("--recorded", type=Path)
    parser.add_argument("--replays", type=Path, required=True)
    parser.add_argument("--interval", type=span, action="append", default=[])
    args = parser.parse_args()
    raw = read(args.raw)
    files = {path.stem: path for path in sorted(args.replays.glob("*.wav"))}
    if args.recorded:
        files["recorded"] = args.recorded
    report = {"window_seconds": 0.5, "notes": "Input above -40 dBFS and below -52 dBFS are energy masks, not voice/noise labels. Output is delay-aligned before comparison.", "candidates": {}}
    window_rows = []
    for name, path in files.items():
        original, processed, offset, delay = align(raw, read(path))
        count = len(original) // 24000
        input_frames = original[:count * 24000].reshape(count, 24000)
        output_frames = processed[:count * 24000].reshape(count, 24000)
        before = 10 * np.log10(np.maximum(np.mean(input_frames ** 2, axis=1), 1e-12))
        after = 10 * np.log10(np.maximum(np.mean(output_frames ** 2, axis=1), 1e-12))
        gain = after - before
        high, low = before > -40, before < -52
        selected = gain[high]
        intervals = []
        for start, end in args.interval:
            left, right = max(0, int(start * 48000) - offset), min(len(original), int(end * 48000) - offset)
            if right <= left:
                continue
            input_db, output_db = db(original[left:right]), db(processed[left:right])
            intervals.append({"start": start, "end": end, "input_dbfs": round(input_db, 2), "output_dbfs": round(output_db, 2), "gain_db": round(output_db - input_db, 2)})
        result = {"delay_ms": delay / 48, "high_input_windows": int(high.sum()),
                  "high_input_gain_p05_p50_p95_db": np.percentile(selected, [5, 50, 95]).round(2).tolist() if len(selected) else [],
                  "high_input_windows_below_minus3db": int(np.sum(selected < -3)),
                  "low_input_gain_median_db": round(float(np.median(gain[low])), 2) if np.any(low) else None,
                  "peak_dbfs": round(20 * np.log10(max(float(np.max(np.abs(processed))), 1e-6)), 2), "intervals": intervals}
        report["candidates"][name] = result
        for i in range(count):
            window_rows.append([name, round(offset / 48000 + i * 0.5, 3), round(before[i], 3), round(after[i], 3), round(gain[i], 3)])
        print(name, json.dumps(result))
    (args.replays / "analysis.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    with (args.replays / "levels.csv").open("w", newline="", encoding="utf-8") as output:
        writer = csv.writer(output)
        writer.writerow(["candidate", "raw_time_seconds", "raw_dbfs", "sent_dbfs", "gain_db"])
        writer.writerows(window_rows)


if __name__ == "__main__":
    main()
