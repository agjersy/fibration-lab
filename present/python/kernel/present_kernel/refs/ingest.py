"""Ingest one reference file: pick the extractor, run it, cache the extracted document.

The cache (``.present/cache/refs/<ref_id>/<version>/doc.json`` plus extracted images) is safe to
delete: the reader re-extracts from the file under ``references/`` when it is missing.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
from typing import Any

from ..errors import INVALID_PARAMS, UNSUPPORTED_REFERENCE, RpcError, not_found
from ..paths import rel_to_root, resolve_in_root
from .extract import kind_for, run_extractor
from .locs import REF_ID_RE
from .model import DOC_FORMAT, Ctx, table_ref

MAX_ATTACHMENT_DEPTH = 3


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def derive_ref_id(sha256: str) -> str:
    return f"r-{sha256[:8]}"


def cache_dir(project: Path, ref_id: str, version: str) -> Path:
    return project / ".present" / "cache" / "refs" / ref_id / version


def load_cached(project: Path, ref_id: str, version: str) -> dict[str, Any] | None:
    path = cache_dir(project, ref_id, version) / "doc.json"
    try:
        doc = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return doc if isinstance(doc, dict) and doc.get("format") == DOC_FORMAT else None


def _store(project: Path, ref_id: str, version: str, doc: dict[str, Any]) -> None:
    folder = cache_dir(project, ref_id, version)
    try:
        folder.mkdir(parents=True, exist_ok=True)
        tmp = folder / f"doc.json.{os.getpid()}.tmp"
        tmp.write_text(json.dumps(doc, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
        os.replace(tmp, folder / "doc.json")
    except OSError:
        pass  # read-only project (sandboxed build): the document is still returned


def extract_file(project: Path, full: Path, ref_id: str, version: str, name: str = "", depth: int = 0) -> dict[str, Any]:
    """Run the extractor for ``full`` and cache the result. Raises 2009 for unsupported or unreadable files."""
    display = name or full.name
    ctx = Ctx(project=project, ref_id=ref_id, version=version, name=name, file_name=full.name, depth=depth)

    def attach(n: int, filename: str, data: bytes) -> dict[str, Any] | None:
        filename = Path(filename.replace("\\", "/")).name if filename else ""
        if not data or depth >= MAX_ATTACHMENT_DEPTH or kind_for(filename) is None:
            return None
        sha = hashlib.sha256(data).hexdigest()
        target = project / "references" / f"{sha[:12]}{Path(filename).suffix.lower()}"
        try:
            if not target.is_file():
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            child_id = derive_ref_id(sha)
            child = load_cached(project, child_id, sha[:12]) or extract_file(project, target, child_id, sha[:12], filename, depth + 1)
        except (RpcError, OSError):
            return None
        return {"n": n, "name": filename, "file": rel_to_root(project, target), "ref_id": child_id, "version": sha[:12],
                "kind": child["kind"], "title": child["title"], "root_loc": child["root_loc"]}  # fmt: skip

    ctx.attach = attach
    try:
        if kind_for(full.name) is None:
            raise RpcError(UNSUPPORTED_REFERENCE, f"{display}: {full.suffix or 'files without an extension'} is not a supported reference format")
        else:
            doc = run_extractor(full, ctx)
    except RpcError:
        raise
    except Exception as exc:
        raise RpcError(UNSUPPORTED_REFERENCE, f"{display}: could not be read as {full.suffix.lstrip('.') or 'a reference'} ({type(exc).__name__}: {exc})") from exc
    doc.update({"ref_id": ref_id, "version": version, "file": rel_to_root(project, full), "name": name})
    _store(project, ref_id, version, doc)
    return doc


def ingest(project: Path, rel: str, ref_id: str | None = None, name: str = "") -> dict[str, Any]:
    """``refs.ingest``: the extracted document for a project-relative path under ``references/``."""
    full = resolve_in_root(project, rel)
    refs_root = (project / "references").resolve()
    if refs_root not in full.parents:
        raise RpcError(INVALID_PARAMS, f"{rel} is not under references/")
    if not full.is_file():
        raise not_found(f"{rel} not found")
    sha = sha256_file(full)
    ref_id = ref_id or derive_ref_id(sha)
    if not REF_ID_RE.match(ref_id):
        raise RpcError(INVALID_PARAMS, f"ref_id {ref_id!r} does not match ^r-[0-9a-z]{{4,12}}$")
    return extract_file(project, full, ref_id, sha[:12], name)


def public_result(doc: dict[str, Any]) -> dict[str, Any]:
    """The ``refs.ingest`` result (PROTOCOL 1.3) from an extracted document."""
    out: dict[str, Any] = {"ref_id": doc["ref_id"], "kind": doc["kind"], "title": doc["title"]}
    if doc.get("pages") is not None:
        out["pages"] = doc["pages"]
    out.update(
        outline=doc["outline"], chunks=doc["chunks"], tables=[table_ref(t) for t in doc["tables"]],
        images=doc["images"], children=[a["ref_id"] for a in doc["attachments"]], warnings=doc["warnings"],
        # Additions the core needs to register what was ingested (not in PROTOCOL 1.3 yet):
        version=doc["version"], file=doc["file"],
        child_refs=[{"ref_id": a["ref_id"], "version": a["version"], "file": a["file"], "kind": a["kind"],
                     "title": a["title"], "name": a["name"], "loc": f"att{a['n']}"} for a in doc["attachments"]],
    )  # fmt: skip
    return out
