#!/usr/bin/env python3
"""Download the bundled sentence-embedding model for development and bundling.

Writes ONNX weights, ``tokenizer.json``, and ``manifest.json`` (with SHA-256 checksums) into
``vendor/models/embedding/``, where the kernel finds them in a development checkout
(``present_kernel/refs/embed.py``). Installed builds ship the same directory as
``<home>/models/embedding/``; nothing is downloaded at run time.

    python tools/fetch_embedding_model.py                     # default candidate
    python tools/fetch_embedding_model.py --candidate e5-small-fp16
    python tools/fetch_embedding_model.py --verify            # re-check an existing download

Standard library only. Honors HTTPS_PROXY and SSL_CERT_FILE; ``HF_ENDPOINT`` overrides the host
(for a mirror). See docs/notes/embedding-model.md for the candidates and what was verified.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
DEFAULT_OUT = REPO / "vendor" / "models" / "embedding"
MAX_ONNX_BYTES = 300 * 1024 * 1024  # BUILD-SPEC W9: ONNX weights of at most 300 MB

# File names in the upstream repositories are recorded from memory of their layout and have not
# been confirmed from this checkout's build environment (see the note). ``sha256`` is empty until
# a first verified download pins it; with a pin, a mismatch fails the fetch.
E5 = {"license": "MIT", "dims": 384, "max_tokens": 512, "pooling": "mean", "normalize": True,
      "passage_prefix": "passage: ", "query_prefix": "query: ", "pad_token": "<pad>"}  # fmt: skip
CANDIDATES: dict[str, dict] = {
    "e5-small-int8": {**E5, "name": "intfloat/multilingual-e5-small (int8)", "repo": "Xenova/multilingual-e5-small",
                      "revision": "main", "files": {"model.onnx": "onnx/model_quantized.onnx", "tokenizer.json": "tokenizer.json"},
                      "sha256": {}},
    "e5-small-fp16": {**E5, "name": "intfloat/multilingual-e5-small (fp16)", "repo": "Xenova/multilingual-e5-small",
                      "revision": "main", "files": {"model.onnx": "onnx/model_fp16.onnx", "tokenizer.json": "tokenizer.json"},
                      "sha256": {}},
    "e5-small-fp32": {**E5, "name": "intfloat/multilingual-e5-small", "repo": "intfloat/multilingual-e5-small",
                      "revision": "main", "files": {"model.onnx": "onnx/model.onnx", "tokenizer.json": "tokenizer.json"},
                      "sha256": {}},
}
DEFAULT = "e5-small-int8"


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def download(url: str, target: Path) -> None:
    tmp = target.with_name(target.name + ".part")
    request = urllib.request.Request(url, headers={"User-Agent": "present-fetch-embedding-model"})
    with urllib.request.urlopen(request, timeout=60) as response, open(tmp, "wb") as out:
        total = int(response.headers.get("Content-Length") or 0)
        done = 0
        while True:
            block = response.read(1 << 20)
            if not block:
                break
            out.write(block)
            done += len(block)
            if total and sys.stderr.isatty():
                print(f"\r  {target.name}: {done * 100 // total}%", end="", file=sys.stderr)
    if sys.stderr.isatty():
        print(file=sys.stderr)
    os.replace(tmp, target)


def verify(out: Path) -> int:
    try:
        manifest = json.loads((out / "manifest.json").read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        print(f"no readable manifest in {out}: {exc}", file=sys.stderr)
        return 1
    bad = 0
    for name, expected in manifest.get("sha256", {}).items():
        path = out / name
        actual = sha256_of(path) if path.is_file() else "missing"
        print(f"{'ok ' if actual == expected else 'BAD'} {name} {actual}")
        bad += actual != expected
    return 1 if bad else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--candidate", choices=sorted(CANDIDATES), default=DEFAULT)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--revision", help="upstream revision (commit) instead of the candidate's default")
    parser.add_argument("--allow-large", action="store_true", help="keep weights over the 300 MB limit")
    parser.add_argument("--verify", action="store_true", help="only check the files in --out against its manifest")
    args = parser.parse_args(argv)
    if args.verify:
        return verify(args.out)

    spec = CANDIDATES[args.candidate]
    revision = args.revision or spec["revision"]
    endpoint = os.environ.get("HF_ENDPOINT", "https://huggingface.co").rstrip("/")
    args.out.mkdir(parents=True, exist_ok=True)
    sums: dict[str, str] = {}
    for local, remote in spec["files"].items():
        url = f"{endpoint}/{spec['repo']}/resolve/{revision}/{remote}"
        print(f"fetching {url}")
        try:
            download(url, args.out / local)
        except (urllib.error.URLError, OSError) as exc:
            print(f"download failed: {exc}\nNothing was installed. If this network blocks the host, fetch on another "
                  f"machine (or set HF_ENDPOINT to a mirror) and copy the directory to {args.out}.", file=sys.stderr)  # fmt: skip
            return 2
        sums[local] = sha256_of(args.out / local)
        pinned = spec["sha256"].get(local)
        if pinned and pinned != sums[local]:
            print(f"checksum mismatch for {local}: expected {pinned}, got {sums[local]}", file=sys.stderr)
            return 3
    size = (args.out / "model.onnx").stat().st_size
    if size > MAX_ONNX_BYTES and not args.allow_large:
        (args.out / "model.onnx").unlink()
        print(f"model.onnx is {size / 1e6:.0f} MB, over the 300 MB limit; removed (pass --allow-large to keep it)", file=sys.stderr)
        return 4
    manifest = {k: spec[k] for k in ("name", "license", "dims", "max_tokens", "pooling", "normalize", "passage_prefix",
                                     "query_prefix", "pad_token")}  # fmt: skip
    manifest.update(onnx="model.onnx", tokenizer="tokenizer.json", source=f"{endpoint}/{spec['repo']}", revision=revision,
                    sha256=sums, size_bytes=size)  # fmt: skip
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(f"installed {manifest['name']} ({size / 1e6:.0f} MB) in {args.out}")
    for name, digest in sums.items():
        print(f"  sha256 {name} {digest}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
