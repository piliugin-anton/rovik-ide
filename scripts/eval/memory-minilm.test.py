"""Offline encoder contracts, using synthetic token outputs instead of weights."""
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest

import numpy as np

spec = importlib.util.spec_from_file_location(
    "minilm", Path(__file__).resolve().parents[2]
    / "src-tauri/crates/verified-code-memory/src/minilm.py",
)
minilm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(minilm)


class Encoder(unittest.TestCase):
    def backend(self, hidden):
        encoder = minilm.MiniLM.__new__(minilm.MiniLM)
        self.texts = []
        self.feeds = []

        def tokenize(text):
            self.texts.append(text)
            return SimpleNamespace(ids=[101, 102, 0], attention_mask=[1, 1, 0], type_ids=[0, 0, 0])

        def infer(_, feed):
            self.feeds.append(feed)
            return [hidden]

        encoder.tokenizer = SimpleNamespace(encode=tokenize)
        encoder.session = SimpleNamespace(run=infer)
        return encoder

    def test_padding_is_excluded_and_mean_is_normalized(self):
        hidden = np.zeros((1, 3, 384), dtype=np.float32)
        hidden[0, 0, 0] = 6
        hidden[0, 1, 1] = 8
        hidden[0, 2, :] = 1000
        vector = self.backend(hidden).encode("claim")
        np.testing.assert_allclose(vector[:2], [0.6, 0.8], atol=1e-7)
        self.assertTrue(all(value == 0 for value in vector[2:]))
        self.assertEqual(self.feeds[0]["input_ids"].dtype, np.int64)

    def test_character_bound_preserves_unicode_and_bounds_inference(self):
        encoder = self.backend(np.ones((1, 3, 384), dtype=np.float32))
        encoder.encode("🦀" * 501)
        self.assertEqual(self.texts, ["🦀" * 500])

    def test_invalid_outputs_fail_instead_of_entering_the_index(self):
        for hidden in [np.zeros((1, 3, 384)), np.ones((1, 3, 300)),
                       np.full((1, 3, 384), np.nan)]:
            with self.subTest(shape=hidden.shape), self.assertRaises(ValueError):
                self.backend(hidden).encode("claim")


if __name__ == "__main__":
    unittest.main()
