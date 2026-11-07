"""Offline Chroma all-MiniLM-L6-v2 encoder shared by the worker and evaluation.

Provision model.onnx and tokenizer.json before starting the worker. No model
hub client or network access is needed. Keep tokenization and pooling identical
in production and threshold calibration.
"""
import json
from pathlib import Path
import sys

import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer


class MiniLM:
    """Encode bounded text on one CPU thread with normalized mean pooling."""

    def __init__(self, model_path):
        model_path = Path(model_path)
        self.tokenizer = Tokenizer.from_file(str(model_path / "tokenizer.json"))
        self.tokenizer.enable_truncation(max_length=256)
        self.tokenizer.enable_padding(pad_id=0, pad_token="[PAD]", length=256)
        options = ort.SessionOptions()
        options.intra_op_num_threads = 1
        options.inter_op_num_threads = 1
        self.session = ort.InferenceSession(
            str(model_path / "model.onnx"), options, providers=["CPUExecutionProvider"]
        )

    def encode(self, text):
        """Return 384 finite floats; padding does not contribute to the mean."""
        tokens = self.tokenizer.encode(text[:500])
        mask = np.array([tokens.attention_mask], dtype=np.int64)
        hidden = self.session.run(None, {
            "input_ids": np.array([tokens.ids], dtype=np.int64),
            "attention_mask": mask,
            "token_type_ids": np.array([tokens.type_ids], dtype=np.int64),
        })[0]
        expanded = mask[:, :, None]
        vector = (hidden * expanded).sum(1) / np.maximum(expanded.sum(1), 1)
        norm = np.linalg.norm(vector, axis=1, keepdims=True)
        if vector.shape != (1, 384) or not np.isfinite(vector).all() or not (norm > 0).all():
            raise ValueError("MiniLM returned an invalid 384-dimensional embedding")
        return (vector / norm).astype(np.float32)[0].tolist()


if __name__ == "__main__":
    encoder = MiniLM(sys.argv[1])
    for line in sys.stdin:
        print(json.dumps(encoder.encode(json.loads(line)), allow_nan=False), flush=True)
