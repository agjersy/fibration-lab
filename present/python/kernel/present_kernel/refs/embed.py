"""Sentence embeddings with a bundled ONNX model (onnxruntime + tokenizers).

A model directory holds ``manifest.json``, the ONNX weights, and ``tokenizer.json``::

    { "name": "intfloat/multilingual-e5-small", "license": "MIT", "dims": 384, "max_tokens": 512,
      "onnx": "model.onnx", "tokenizer": "tokenizer.json", "pooling": "mean", "normalize": true,
      "passage_prefix": "passage: ", "query_prefix": "query: ", "sha256": { "model.onnx": "..." } }

The prefixes are part of the manifest because E5-style models need them and others do not.
Resolution order: ``PRESENT_EMBEDDING_MODEL``, ``<home>/models/embedding/``, then
``<repo>/vendor/models/embedding/`` in a development checkout. Nothing is downloaded at run time.
"""

from __future__ import annotations

import json
import os
import threading
from pathlib import Path
from typing import Any

from ..errors import NOT_FOUND, RpcError, invalid_params

MANIFEST = "manifest.json"
DEFAULT_BATCH = 32
MAX_TEXTS = 4096
NO_MODEL = (
    "no embedding model is installed: set PRESENT_EMBEDDING_MODEL to a model directory, or place one at "
    "<home>/models/embedding/ (development: run tools/fetch_embedding_model.py). Lexical search works without it."
)


def candidate_dirs(home: Path | None) -> list[Path]:
    out: list[Path] = []
    env = os.environ.get("PRESENT_EMBEDDING_MODEL")
    if env:
        out.append(Path(env))
    if home is not None:
        out.append(Path(home) / "models" / "embedding")
    repo = Path(__file__).resolve().parents[4]
    if (repo / "python" / "kernel").is_dir():
        out.append(repo / "vendor" / "models" / "embedding")
    return out


def find_model_dir(home: Path | None) -> Path | None:
    for folder in candidate_dirs(home):
        if (folder / MANIFEST).is_file():
            return folder
    return None


def read_manifest(folder: Path) -> dict[str, Any]:
    try:
        manifest = json.loads((folder / MANIFEST).read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise RpcError(NOT_FOUND, f"embedding model manifest {folder / MANIFEST} is unreadable: {exc}") from exc
    if not isinstance(manifest, dict):
        raise RpcError(NOT_FOUND, f"embedding model manifest {folder / MANIFEST} is not an object")
    return manifest


def embedding_info(home: Path | None) -> dict[str, Any]:
    """``EmbeddingInfo`` for kernel.hello, from the manifest alone (the model is not loaded)."""
    folder = find_model_dir(home)
    if folder is None:
        return {"kind": "none", "model": "", "dims": 0}
    try:
        manifest = read_manifest(folder)
    except RpcError:
        return {"kind": "none", "model": "", "dims": 0}
    if not (folder / str(manifest.get("onnx", "model.onnx"))).is_file():
        return {"kind": "none", "model": "", "dims": 0}
    return {"kind": "bundled", "model": str(manifest.get("name", folder.name)), "dims": int(manifest.get("dims", 0))}


def pool(hidden, mask, pooling: str = "mean", normalize: bool = True):
    """Sentence vectors from token states ``[batch, tokens, dims]`` and the attention mask ``[batch, tokens]``."""
    import numpy as np

    hidden = np.asarray(hidden, dtype=np.float32)
    if hidden.ndim == 2:  # the model already pools
        vectors = hidden
    elif pooling == "cls":
        vectors = hidden[:, 0, :]
    else:
        weights = np.asarray(mask, dtype=np.float32)[:, :, None]
        vectors = (hidden * weights).sum(axis=1) / np.maximum(weights.sum(axis=1), 1e-9)
    if normalize:
        vectors = vectors / np.maximum(np.linalg.norm(vectors, axis=1, keepdims=True), 1e-12)
    return vectors.astype(np.float32)


class Embedder:
    """Runs one model. ``session`` and ``tokenizer`` can be injected (tests)."""

    def __init__(self, folder: Path | None = None, manifest: dict[str, Any] | None = None, session: Any = None,
                 tokenizer: Any = None) -> None:  # fmt: skip
        self.manifest = manifest if manifest is not None else read_manifest(folder)
        self.name = str(self.manifest.get("name", folder.name if folder else "model"))
        self.max_tokens = int(self.manifest.get("max_tokens", 512))
        self.pooling = str(self.manifest.get("pooling", "mean"))
        self.normalize = bool(self.manifest.get("normalize", True))
        self.batch_size = int(self.manifest.get("batch_size", DEFAULT_BATCH))
        self._lock = threading.Lock()
        if tokenizer is None:
            from tokenizers import Tokenizer

            tokenizer = Tokenizer.from_file(str(folder / str(self.manifest.get("tokenizer", "tokenizer.json"))))
        if session is None:
            import onnxruntime as ort

            options = ort.SessionOptions()
            threads = int(os.environ.get("PRESENT_EMBEDDING_THREADS", "0") or 0)
            if threads > 0:
                options.intra_op_num_threads = threads
            session = ort.InferenceSession(str(folder / str(self.manifest.get("onnx", "model.onnx"))), sess_options=options,
                                           providers=["CPUExecutionProvider"])  # fmt: skip
        self.tokenizer, self.session = tokenizer, session
        pad_token = str(self.manifest.get("pad_token", ""))
        pad_id = self.manifest.get("pad_id")
        if pad_id is None:
            for token in ([pad_token] if pad_token else []) + ["<pad>", "[PAD]"]:
                found = tokenizer.token_to_id(token)
                if found is not None:
                    pad_id, pad_token = found, token
                    break
        tokenizer.enable_truncation(max_length=self.max_tokens)
        tokenizer.enable_padding(pad_id=int(pad_id or 0), pad_token=pad_token or "<pad>")
        self.inputs = [i.name for i in session.get_inputs()]
        self.dims = int(self.manifest.get("dims", 0))

    def prefix(self, kind: str) -> str:
        if kind == "none":
            return ""
        return str(self.manifest.get("query_prefix" if kind == "query" else "passage_prefix", ""))

    def embed(self, texts: list[str], kind: str = "passage"):
        """An array ``[len(texts), dims]`` of float32 vectors, in input order."""
        import numpy as np

        if not texts:
            return np.zeros((0, self.dims), dtype=np.float32)
        prefix = self.prefix(kind)
        prepared = [prefix + (t or "") for t in texts]
        order = sorted(range(len(prepared)), key=lambda i: len(prepared[i]))  # similar lengths share a batch
        out: Any = None
        with self._lock:
            for start in range(0, len(order), self.batch_size):
                batch = order[start:start + self.batch_size]
                encodings = self.tokenizer.encode_batch([prepared[i] for i in batch])
                ids = np.array([e.ids for e in encodings], dtype=np.int64)
                mask = np.array([e.attention_mask for e in encodings], dtype=np.int64)
                feeds = {"input_ids": ids, "attention_mask": mask, "token_type_ids": np.zeros_like(ids)}
                result = self.session.run(None, {name: feeds[name] for name in self.inputs if name in feeds})
                vectors = pool(result[0], mask, self.pooling, self.normalize)
                if out is None:
                    out = np.zeros((len(prepared), vectors.shape[1]), dtype=np.float32)
                out[batch] = vectors
        self.dims = int(out.shape[1])
        return out


_EMBEDDERS: dict[str, Embedder] = {}
_LOAD_LOCK = threading.Lock()


def get_embedder(home: Path | None) -> Embedder:
    folder = find_model_dir(home)
    if folder is None:
        raise RpcError(NOT_FOUND, NO_MODEL)
    key = str(folder.resolve())
    with _LOAD_LOCK:
        if key not in _EMBEDDERS:
            try:
                _EMBEDDERS[key] = Embedder(folder)
            except RpcError:
                raise
            except Exception as exc:
                raise RpcError(NOT_FOUND, f"the embedding model in {folder} could not be loaded: {type(exc).__name__}: {exc}") from exc
        return _EMBEDDERS[key]


def embed_texts(home: Path | None, texts: Any, kind: str = "passage") -> dict[str, Any]:
    """``embed.texts``."""
    if not isinstance(texts, list) or not all(isinstance(t, str) for t in texts):
        raise invalid_params("texts must be a list of strings")
    if len(texts) > MAX_TEXTS:
        raise invalid_params(f"at most {MAX_TEXTS} texts per call")
    if kind not in ("passage", "query", "none"):
        raise invalid_params("kind must be passage, query, or none")
    embedder = get_embedder(home)
    vectors = embedder.embed(texts, kind)
    return {"vectors": [[round(float(x), 6) for x in row] for row in vectors], "model": embedder.name, "dims": embedder.dims}
