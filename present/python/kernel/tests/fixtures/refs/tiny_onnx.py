"""A tiny ONNX embedding model and tokenizer for tests, written without the ``onnx`` package.

The model is one ``Gather`` over an embedding matrix: ``last_hidden_state[b, t] = E[input_ids[b, t]]``,
so the expected sentence vector can be computed by hand. The file is a hand-encoded ONNX protobuf.
"""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np


def _varint(n: int) -> bytes:
    out = bytearray()
    while True:
        byte = n & 0x7F
        n >>= 7
        out.append(byte | (0x80 if n else 0))
        if not n:
            return bytes(out)


def _field(number: int, value: bytes | int | str) -> bytes:
    if isinstance(value, int):
        return _varint(number << 3) + _varint(value)
    if isinstance(value, str):
        value = value.encode()
    return _varint((number << 3) | 2) + _varint(len(value)) + value


def _value_info(name: str, elem_type: int, dims: list[str | int]) -> bytes:
    shape = b"".join(_field(1, _field(2, d) if isinstance(d, str) else _field(1, d)) for d in dims)
    tensor = _field(1, elem_type) + _field(2, shape)
    return _field(1, name) + _field(2, _field(1, tensor))


def tiny_model_bytes(matrix: np.ndarray) -> bytes:
    vocab, dims = matrix.shape
    initializer = (_field(1, vocab) + _field(1, dims) + _field(2, 1) + _field(8, "E")
                   + _field(9, matrix.astype("<f4").tobytes()))  # fmt: skip
    gather = _field(1, "E") + _field(1, "input_ids") + _field(2, "last_hidden_state") + _field(3, "gather") + _field(4, "Gather")
    graph = (_field(1, gather) + _field(2, "tiny") + _field(5, initializer)
             + _field(11, _value_info("input_ids", 7, ["batch", "tokens"]))
             + _field(11, _value_info("attention_mask", 7, ["batch", "tokens"]))
             + _field(12, _value_info("last_hidden_state", 1, ["batch", "tokens", dims])))  # fmt: skip
    return _field(1, 8) + _field(2, "present-tests") + _field(7, graph) + _field(8, _field(1, "") + _field(2, 13))


def build_tiny_model(folder: Path, words: list[str], dims: int = 8, seed: int = 0, **manifest) -> np.ndarray:
    """Write model.onnx, tokenizer.json, manifest.json; returns the embedding matrix."""
    from tokenizers import Tokenizer
    from tokenizers.models import WordLevel
    from tokenizers.pre_tokenizers import Whitespace

    folder.mkdir(parents=True, exist_ok=True)
    vocab = {"<pad>": 0, "<unk>": 1, **{w: i + 2 for i, w in enumerate(words)}}
    matrix = np.random.default_rng(seed).normal(size=(len(vocab), dims)).astype(np.float32)
    (folder / "model.onnx").write_bytes(tiny_model_bytes(matrix))
    tokenizer = Tokenizer(WordLevel(vocab, unk_token="<unk>"))
    tokenizer.pre_tokenizer = Whitespace()
    tokenizer.save(str(folder / "tokenizer.json"))
    data = {"name": "tiny-test-model", "license": "test", "dims": dims, "max_tokens": 16, "onnx": "model.onnx",
            "tokenizer": "tokenizer.json", "pooling": "mean", "normalize": True, "passage_prefix": "passage: ",
            "query_prefix": "query: ", "pad_token": "<pad>", **manifest}  # fmt: skip
    (folder / "manifest.json").write_text(json.dumps(data), encoding="utf-8")
    return matrix
