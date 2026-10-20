import importlib.util
import unittest
from pathlib import Path

import numpy as np

spec = importlib.util.spec_from_file_location("mic_replay", Path(__file__).with_name("analyze-mic-replay.py"))
replay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(replay)


class ReplayMetrics(unittest.TestCase):
    def test_delay_alignment_preserves_the_measured_gain(self):
        raw = np.random.default_rng(7).normal(0, 0.1, 24000)
        for delay in [48, 480, 1488]:
            sent = np.concatenate([np.zeros(delay), raw[:-delay] * 0.5])
            before, after, offset, measured = replay.align(raw, sent)
            self.assertEqual(measured, delay)
            self.assertEqual(offset, 0)
            self.assertAlmostEqual(replay.db(after) - replay.db(before), -6.020599913, places=6)

    def test_earlier_output_retains_the_correct_source_time(self):
        raw = np.random.default_rng(8).normal(0, 0.1, 24000)
        before, after, offset, delay = replay.align(raw, raw[480:])
        self.assertEqual(delay, -480)
        self.assertEqual(offset, 480)
        np.testing.assert_allclose(before, after)

    def test_silence_has_no_invented_latency_or_infinite_level(self):
        silence = np.zeros(4800)
        self.assertEqual(replay.align(silence, silence)[3], 0)
        self.assertEqual(replay.db(silence), -120)
        self.assertEqual(replay.span("7:8"), (7, 8))
        with self.assertRaises(ValueError):
            replay.span("8:7")


if __name__ == "__main__":
    unittest.main()
