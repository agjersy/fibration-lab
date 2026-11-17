"""W9: embeddings (embed.texts, kernel.hello) and Personal Analyst imports.

No real embedding model is available to this test suite. The onnxruntime + tokenizers path runs
against a tiny ONNX model that the test helpers write by hand (one Gather over a random embedding
matrix), so tokenization, batching, padding, truncation, pooling, and normalization are exercised
end to end; the quality and speed of the real model are not.
"""

from __future__ import annotations

import json
import sys
import time
from pathlib import Path

import numpy as np
import pytest

FIXTURES = Path(__file__).resolve().parent / "fixtures" / "refs"
sys.path.insert(0, str(FIXTURES))

import make_fixtures  # noqa: E402
from tiny_onnx import build_tiny_model  # noqa: E402

from present_kernel.errors import RpcError  # noqa: E402
from present_kernel.kernel import Kernel  # noqa: E402
from present_kernel.refs import embed  # noqa: E402

WORDS = ["alpha", "beta", "gamma", "delta", "passage", "query", ":"]
ID = {"<pad>": 0, "<unk>": 1, **{w: i + 2 for i, w in enumerate(WORDS)}}


def expected(matrix: np.ndarray, tokens: list[str]) -> np.ndarray:
    vector = matrix[[ID.get(t, 1) for t in tokens]].mean(axis=0)
    return vector / np.linalg.norm(vector)


@pytest.fixture()
def kernel(tmp_path):
    made = Kernel(project=tmp_path / "project", home=tmp_path / "home")
    yield made
    made.close()


def hello(kernel) -> dict:
    return kernel.dispatcher.call("kernel.hello", {"core_version": "0", "protocol_version": 1})


def test_embed_texts_with_a_model_directory(kernel, tmp_path, monkeypatch):
    matrix = build_tiny_model(tmp_path / "model", WORDS)
    monkeypatch.setenv("PRESENT_EMBEDDING_MODEL", str(tmp_path / "model"))
    assert hello(kernel)["embedding"] == {"kind": "bundled", "model": "tiny-test-model", "dims": 8}
    texts = ["alpha beta", "gamma", "alpha unknownword gamma beta beta", ""]
    out = kernel.dispatcher.call("embed.texts", {"texts": texts})
    vectors = np.array(out["vectors"])
    assert vectors.shape == (4, 8) and out["dims"] == 8 and out["model"] == "tiny-test-model"
    assert np.allclose(np.linalg.norm(vectors, axis=1), 1.0, atol=1e-5)
    prefix = ["passage", ":"]  # the manifest's "passage: " prefix is tokenized with the text
    assert np.allclose(vectors[0], expected(matrix, [*prefix, "alpha", "beta"]), atol=1e-5)  # padding does not leak in
    assert np.allclose(vectors[1], expected(matrix, [*prefix, "gamma"]), atol=1e-5)
    assert np.allclose(vectors[2], expected(matrix, [*prefix, "alpha", "?", "gamma", "beta", "beta"]), atol=1e-5)
    assert np.allclose(vectors[3], expected(matrix, prefix), atol=1e-5)
    query = np.array(kernel.dispatcher.call("embed.texts", {"texts": ["alpha beta"], "kind": "query"})["vectors"])
    assert np.allclose(query[0], expected(matrix, ["query", ":", "alpha", "beta"]), atol=1e-5)
    bare = np.array(kernel.dispatcher.call("embed.texts", {"texts": ["alpha beta"], "kind": "none"})["vectors"])
    assert np.allclose(bare[0], expected(matrix, ["alpha", "beta"]), atol=1e-5)
    # Truncation at the manifest's max_tokens (16): the 2 prefix tokens plus the first 14 words.
    long = np.array(kernel.dispatcher.call("embed.texts", {"texts": ["alpha " * 14 + "delta " * 50]})["vectors"])
    assert np.allclose(long[0], expected(matrix, [*prefix, *["alpha"] * 14]), atol=1e-5)
    assert kernel.dispatcher.call("embed.texts", {"texts": []})["vectors"] == []
    for bad in ({}, {"texts": "alpha"}, {"texts": [1]}, {"texts": ["a"], "kind": "document"}):
        with pytest.raises(RpcError) as info:
            kernel.dispatcher.call("embed.texts", bad)
        assert info.value.code == -32602


def test_model_directory_resolution_order(tmp_path, monkeypatch):
    home = tmp_path / "home"
    monkeypatch.delenv("PRESENT_EMBEDDING_MODEL", raising=False)
    dirs = embed.candidate_dirs(home)
    assert dirs[0] == home / "models" / "embedding" and dirs[-1].as_posix().endswith("vendor/models/embedding")
    monkeypatch.setenv("PRESENT_EMBEDDING_MODEL", str(tmp_path / "env"))
    assert embed.candidate_dirs(home)[:2] == [tmp_path / "env", home / "models" / "embedding"]
    build_tiny_model(home / "models" / "embedding", WORDS, name="home-model")
    assert embed.find_model_dir(home) == home / "models" / "embedding"  # the env directory has no manifest
    assert embed.embedding_info(home) == {"kind": "bundled", "model": "home-model", "dims": 8}
    build_tiny_model(tmp_path / "env", WORDS, name="env-model")
    assert embed.embedding_info(home)["model"] == "env-model"


def test_no_model_is_reported_clearly(kernel, tmp_path, monkeypatch):
    monkeypatch.setattr(embed, "candidate_dirs", lambda home: [tmp_path / "nowhere"])
    assert hello(kernel)["embedding"] == {"kind": "none", "model": "", "dims": 0}
    with pytest.raises(RpcError) as info:
        kernel.dispatcher.call("embed.texts", {"texts": ["alpha"]})
    assert info.value.code == 2000 and "no embedding model is installed" in info.value.detail
    # A manifest without weights does not count as a model.
    (tmp_path / "nowhere").mkdir()
    (tmp_path / "nowhere" / "manifest.json").write_text(json.dumps({"name": "x", "dims": 4}))
    assert hello(kernel)["embedding"]["kind"] == "none"


class StubSession:
    """Stands in for onnxruntime.InferenceSession: token states are one-hot rows of the token id."""

    def __init__(self, pooled: bool = False) -> None:
        self.batches: list[tuple[int, int]] = []
        self.pooled = pooled

    def get_inputs(self):
        return [type("Input", (), {"name": n})() for n in ("input_ids", "attention_mask", "token_type_ids")]

    def run(self, _outputs, feeds):
        assert set(feeds) == {"input_ids", "attention_mask", "token_type_ids"}
        ids = feeds["input_ids"]
        assert ids.dtype == np.int64 and feeds["attention_mask"].shape == ids.shape and not feeds["token_type_ids"].any()
        self.batches.append(ids.shape)
        hidden = np.eye(16, dtype=np.float32)[ids]
        return [hidden.sum(axis=1) if self.pooled else hidden]


def test_batching_and_pooling_against_a_stub_session(tmp_path):
    from tokenizers import Tokenizer

    build_tiny_model(tmp_path / "model", WORDS)
    tokenizer = Tokenizer.from_file(str(tmp_path / "model" / "tokenizer.json"))
    manifest = {"name": "stub", "max_tokens": 16, "batch_size": 4, "passage_prefix": "", "pad_token": "<pad>"}
    session = StubSession()
    embedder = embed.Embedder(manifest=manifest, session=session, tokenizer=tokenizer)
    texts = ["alpha " * n for n in (9, 1, 5, 3, 7, 2, 8, 4, 6, 10)]
    vectors = embedder.embed(texts)
    # Ten texts in batches of four, sorted by length so each batch pads only to its own longest text.
    assert session.batches == [(4, 4), (4, 8), (2, 10)]
    assert vectors.shape == (10, 16) and embedder.dims == 16
    for row in vectors:  # mean of identical one-hot tokens, normalized: order was restored, padding ignored
        assert np.allclose(row, np.eye(16)[ID["alpha"]], atol=1e-6)
    mixed = embedder.embed(["alpha beta beta", "gamma"])
    want = np.zeros(16)
    want[ID["alpha"]], want[ID["beta"]] = 1 / 3, 2 / 3
    assert np.allclose(mixed[0], want / np.linalg.norm(want), atol=1e-6)
    # pool(): cls pooling, no normalization, and models that return pooled vectors.
    hidden = np.arange(24, dtype=np.float32).reshape(2, 3, 4)
    mask = np.array([[1, 1, 0], [1, 0, 0]])
    assert np.allclose(embed.pool(hidden, mask, "mean", False), [hidden[0, :2].mean(axis=0), hidden[1, 0]])
    assert np.allclose(embed.pool(hidden, mask, "cls", False), hidden[:, 0])
    assert np.allclose(np.linalg.norm(embed.pool(hidden, mask), axis=1), 1.0)
    pooled = embed.Embedder(manifest=manifest, session=StubSession(pooled=True), tokenizer=tokenizer).embed(["alpha alpha"])
    assert np.allclose(pooled[0], np.eye(16)[ID["alpha"]], atol=1e-6)


def test_a_thousand_chunks_through_the_pipeline(tmp_path):
    """Pipeline overhead only (tokenize, batch, run, pool) with the tiny model; the real model's
    speed is the BUILD-SPEC criterion and is not measured here."""
    import random

    build_tiny_model(tmp_path / "model", make_fixtures.WORDS, dims=32, max_tokens=512)
    rng = random.Random(1)
    texts = [make_fixtures.paragraph(rng, 8) for _ in range(1000)]
    embedder = embed.Embedder(tmp_path / "model")
    started = time.perf_counter()
    vectors = embedder.embed(texts)
    assert vectors.shape == (1000, 32) and time.perf_counter() - started < 20


# ---- Personal Analyst ---------------------------------------------------------------------------
RUN = "20261104-1132-ab12"


@pytest.fixture()
def analyst_repo(tmp_path) -> Path:
    repo = tmp_path / "analyst-repo"
    figures = repo / "out" / "figures" / "fit_model" / RUN
    figures.mkdir(parents=True)
    (figures / "residuals.png").write_bytes(make_fixtures.png_bytes(200, 120, seed=8))
    (figures / "residuals.svg").write_text("<svg xmlns='http://www.w3.org/2000/svg'/>")
    # The sidecar analyst_mpl writes (Analyst PROTOCOL 1.6).
    (figures / "residuals.json").write_text(json.dumps({
        "run_id": RUN, "script": "scripts/fit_model.py", "args": ["--region", "north"], "title": "Residuals by week",
        "axes": [{"title": None, "xlabel": "week", "ylabel": "residual"}], "created_at": "2026-11-04T11:32:07.120Z",
        "width_in": 6.4, "height_in": 4.8, "dpi": 100.0}))  # fmt: skip
    (figures / "fig-2.png").write_bytes(make_fixtures.png_bytes(100, 80, seed=9))  # no sidecar
    tables = repo / "out" / "tables" / RUN
    tables.mkdir(parents=True)
    (tables / "summary.csv").write_text("week,units,share\n" + "".join(f"{i},{i * 10},{i / 100}\n" for i in range(1, 101)))
    (repo / "data").mkdir()
    (repo / "data" / "secret.csv").write_text("a,b\n1,2\n")
    return repo


def test_analyst_scan(kernel, analyst_repo):
    out = kernel.dispatcher.call("analyst.scan", {"repo_abs": str(analyst_repo)})
    assert [f["item"] for f in out["figures"]] == [f"out/figures/fit_model/{RUN}/fig-2.png", f"out/figures/fit_model/{RUN}/residuals.png"]
    plain, figure = out["figures"]
    assert figure["title"] == "Residuals by week" and figure["run_id"] == RUN and figure["script"] == "scripts/fit_model.py"
    assert figure["args"] == ["--region", "north"] and figure["axes"] == [{"title": None, "xlabel": "week", "ylabel": "residual"}]
    assert figure["created"] == "2026-11-04T11:32:07.120Z" and figure["svg"] is True and figure["loc"] == f"run:{RUN}/residuals"
    assert plain["title"] == "fig-2" and plain["run_id"] == RUN and plain["svg"] is False  # run id from the folder
    assert [(t["item"], t["run_id"], t["columns"], t["loc"]) for t in out["tables"]] == [
        (f"out/tables/{RUN}/summary.csv", RUN, ["week", "units", "share"], f"run:{RUN}/summary")]  # fmt: skip
    with pytest.raises(RpcError) as info:
        kernel.dispatcher.call("analyst.scan", {"repo_abs": str(analyst_repo / "missing")})
    assert info.value.code == 2000


def test_analyst_import_figure(kernel, analyst_repo):
    call = kernel.dispatcher.call
    item = f"out/figures/fit_model/{RUN}/residuals.svg"  # either sibling names the figure; the PNG is what is copied
    info = call("analyst.import", {"repo_abs": str(analyst_repo), "item": item})
    assert info["kind"] == "analyst-figure" and info["loc"] == f"run:{RUN}/residuals" and info["title"] == "Residuals by week"
    assert info["file"] == f"references/{info['version']}.png" and info["ref_id"] == f"r-{info['version'][:8]}"
    assert info["analyst"] == {"repo": str(analyst_repo.resolve()), "item": f"out/figures/fit_model/{RUN}/residuals.png", "run_id": RUN}
    refs_dir = kernel.project / "references"
    assert sorted(p.suffix for p in refs_dir.glob(f"{info['version']}.*")) == [".json", ".png", ".svg"]
    assert json.loads((refs_dir / f"{info['version']}.json").read_text(encoding="utf-8"))["sidecar"]["script"] == "scripts/fit_model.py"
    result = call("refs.ingest", {"path": info["file"], "ref_id": info["ref_id"]})
    assert result["kind"] == "analyst-figure" and result["title"] == "Residuals by week"
    assert result["images"] == [{"loc": info["loc"], "png": info["file"], "width": 200, "height": 120, "caption": "Residuals by week"}]
    chunk = result["chunks"][0]
    assert chunk["loc"] == info["loc"] and "xlabel: week" in chunk["text"] and "scripts/fit_model.py --region north" in chunk["text"]
    assert call("refs.read", {"ref_id": info["ref_id"], "loc": info["loc"]}) == {"text": chunk["text"], "loc": info["loc"], "truncated": False}
    assert call("refs.page_image", {"ref_id": info["ref_id"], "page": 1}) == {"png": info["file"]}


def test_analyst_import_table_is_bounded(kernel, analyst_repo):
    call = kernel.dispatcher.call
    info = call("analyst.import", {"repo_abs": str(analyst_repo), "item": f"out/tables/{RUN}/summary.csv", "max_rows": 10})
    assert info["kind"] == "analyst-table" and info["loc"] == f"run:{RUN}/summary" and info["file"].endswith(".csv")
    assert len((kernel.project / info["file"]).read_text(encoding="utf-8").splitlines()) == 11  # header + 10 rows
    result = call("refs.ingest", {"path": info["file"]})
    assert result["ref_id"] == info["ref_id"] and result["kind"] == "analyst-table"
    assert result["tables"] == [{"loc": info["loc"], "rows": 11, "cols": 3, "title": "summary"}]
    assert [w["rule"] for w in result["warnings"]] == ["refs.truncated"] and "10 of 100" in result["warnings"][0]["message"]
    table = call("refs.table", {"ref_id": info["ref_id"], "loc": info["loc"]})
    assert table["columns"] == ["week", "units", "share"] and table["rows"][0] == [1, 10, 0.01] and len(table["rows"]) == 10
    assert kernel.refs.table(f"ref:{info['ref_id']}@{info['version']}#{info['loc']}") == table
    chunk = result["chunks"][0]
    assert chunk["loc"] == info["loc"] and call("refs.read", {"ref_id": info["ref_id"], "loc": info["loc"]})["text"] == chunk["text"]
    full = call("analyst.import", {"repo_abs": str(analyst_repo), "item": f"out/tables/{RUN}/summary.csv"})
    assert full["version"] != info["version"] and len((kernel.project / full["file"]).read_text(encoding="utf-8").splitlines()) == 101


def test_analyst_import_errors(kernel, analyst_repo):
    def code(item: str, **extra) -> int:
        with pytest.raises(RpcError) as info:
            kernel.dispatcher.call("analyst.import", {"repo_abs": str(analyst_repo), "item": item, **extra})
        return info.value.code

    assert code("../outside.csv") == 2001
    assert code("data/secret.csv") == 2001  # only out/ is importable: datasets stay where they are
    assert code(f"out/tables/{RUN}/missing.csv") == 2000
    assert code(f"out/figures/fit_model/{RUN}/residuals.json") == 2009
    assert code(f"out/tables/{RUN}/summary.csv", max_rows=0) == -32602
    assert not (kernel.project / "references").exists() or not list((kernel.project / "references").glob("*.csv"))
