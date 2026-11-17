"""Personal Analyst references: list a repo's figures and output tables, import one of them.

Analyst keeps figures at ``out/figures/<script>/<run-id>/<name>.png|.svg`` with a sidecar
``<name>.json`` (Analyst PROTOCOL 1.6: run_id, script, args, title, axes, created_at, ...) and
tables as CSV files under ``out/``. Only the imported item is copied: the PNG (plus SVG and
sidecar) or a bounded CSV extract, content-addressed under ``references/``. The copy's sidecar
``<first 12 hex>.json`` records the provenance and marks the file as an Analyst reference.
"""

from __future__ import annotations

import csv
import hashlib
import io
import json
import re
import shutil
from pathlib import Path
from typing import Any

from ..errors import PATH_OUTSIDE_ROOT, UNSUPPORTED_REFERENCE, RpcError, invalid_params, not_found
from ..paths import mtime_iso, resolve_in_root
from .chunker import MAX_CHARS
from .model import Ctx, clean_title, diag, grid_lines, new_doc

DEFAULT_MAX_ROWS = 5000
MARKER = "present_analyst"
_RUN_ID = re.compile(r"^\d{8}-\d{4,6}(?:-[0-9a-z]{2,12})?$")
_UNSAFE = re.compile(r"[^0-9A-Za-z._-]+")


def _repo(repo_abs: str) -> Path:
    repo = Path(repo_abs)
    if not repo.is_absolute() or not repo.is_dir():
        raise not_found(f"Analyst repo {repo_abs} not found (repo_abs must be an absolute directory path)")
    return repo.resolve()


def _sidecar(image: Path) -> dict[str, Any]:
    try:
        data = json.loads(image.with_suffix(".json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def _run_id(path: Path, out_root: Path, sidecar: dict[str, Any] | None = None) -> str:
    if sidecar and sidecar.get("run_id"):
        return str(sidecar["run_id"])
    for part in reversed(path.relative_to(out_root).parts[:-1]):
        if _RUN_ID.match(part):
            return part
    return "local"


def _loc(run_id: str, name: str) -> str:
    return f"run:{_UNSAFE.sub('-', run_id) or 'local'}/{_UNSAFE.sub('-', name) or 'item'}"


def _figure(repo: Path, image: Path) -> dict[str, Any]:
    side = _sidecar(image)
    out_root = repo / "out"
    run_id = _run_id(image, out_root, side)
    axes = [a for a in side.get("axes", []) if isinstance(a, dict)] if isinstance(side.get("axes"), list) else []
    return {
        "item": image.relative_to(repo).as_posix(), "name": image.stem, "run_id": run_id,
        "script": str(side.get("script") or ""), "args": [str(a) for a in side.get("args") or []],
        "target": str(side.get("target") or ""), "title": clean_title(str(side.get("title") or "")) or image.stem,
        "axes": [{k: a.get(k) for k in ("title", "xlabel", "ylabel")} for a in axes],
        "created": str(side.get("created_at") or side.get("timestamp") or mtime_iso(image.stat().st_mtime)),
        "png": image.suffix.lower() == ".png", "svg": image.with_suffix(".svg").is_file(),
        "loc": _loc(run_id, image.stem),
    }  # fmt: skip


def scan(repo_abs: str) -> dict[str, Any]:
    """``analyst.scan``: figures (one entry per stem; PNG preferred) and CSV tables under ``out/``."""
    repo = _repo(repo_abs)
    out_root = repo / "out"
    figures: list[dict[str, Any]] = []
    tables: list[dict[str, Any]] = []
    fig_root = out_root / "figures"
    if fig_root.is_dir():
        seen: set[Path] = set()
        for image in sorted([*fig_root.rglob("*.png"), *fig_root.rglob("*.svg")]):
            stem = image.with_suffix("")
            if stem in seen or not image.is_file():
                continue
            seen.add(stem)
            figures.append(_figure(repo, image))
    if out_root.is_dir():
        for path in sorted(out_root.rglob("*.csv")):
            if not path.is_file():
                continue
            run_id = _run_id(path, out_root)
            columns: list[str] = []
            try:
                with open(path, encoding="utf-8-sig", errors="replace", newline="") as handle:
                    columns = next(csv.reader(io.StringIO(handle.readline())), [])
            except OSError:
                pass
            stat = path.stat()
            tables.append({"item": path.relative_to(repo).as_posix(), "name": path.stem, "run_id": run_id,
                           "title": path.stem, "columns": columns, "size_bytes": stat.st_size,
                           "created": mtime_iso(stat.st_mtime), "loc": _loc(run_id, path.stem)})  # fmt: skip
    return {"figures": figures, "tables": tables}


def import_item(project: Path, repo_abs: str, item: str, max_rows: int | None = None) -> dict[str, Any]:
    """``analyst.import``: copy one figure or a bounded table extract into ``references/``."""
    repo = _repo(repo_abs)
    source = resolve_in_root(repo, item)
    if (repo / "out").resolve() not in source.parents:
        raise RpcError(PATH_OUTSIDE_ROOT, f"{item} is not under the repo's out/ folder")
    if not source.is_file():
        raise not_found(f"{item} not found in {repo_abs}")
    refs = project / "references"
    refs.mkdir(parents=True, exist_ok=True)
    ext = source.suffix.lower()
    marker: dict[str, Any] = {"repo": str(repo), "item": source.relative_to(repo).as_posix()}
    original: dict[str, Any] | None = None
    if ext in (".png", ".svg"):
        png = source.with_suffix(".png")
        if not png.is_file():
            raise RpcError(UNSUPPORTED_REFERENCE, f"{item}: the figure has no PNG next to it; only PNG figures can be imported")
        info = _figure(repo, png)
        marker["item"] = png.relative_to(repo).as_posix()
        data = png.read_bytes()
        sha = hashlib.sha256(data).hexdigest()
        target = refs / f"{sha[:12]}.png"
        if not target.is_file():
            target.write_bytes(data)
        if png.with_suffix(".svg").is_file():
            shutil.copyfile(png.with_suffix(".svg"), refs / f"{sha[:12]}.svg")
        original = _sidecar(png) or None
        marker.update(kind="analyst-figure", run_id=info["run_id"], name=info["name"], title=info["title"], loc=info["loc"])
    elif ext == ".csv":
        limit = DEFAULT_MAX_ROWS if max_rows is None else int(max_rows)
        if limit < 1:
            raise invalid_params("max_rows must be at least 1")
        lines: list[bytes] = []
        total = 0
        with open(source, "rb") as handle:
            for line in handle:
                total += 1
                if len(lines) <= limit:  # header plus max_rows data rows
                    lines.append(line)
        data = b"".join(lines)
        sha = hashlib.sha256(data).hexdigest()
        target = refs / f"{sha[:12]}.csv"
        if not target.is_file():
            target.write_bytes(data)
        run_id = _run_id(source, repo / "out")
        marker.update(kind="analyst-table", run_id=run_id, name=source.stem, title=source.stem, loc=_loc(run_id, source.stem),
                      total_rows=max(total - 1, 0), truncated=total > len(lines))  # fmt: skip
    else:
        raise RpcError(UNSUPPORTED_REFERENCE, f"{item}: only figures (.png, .svg) and tables (.csv) can be imported")
    (refs / f"{sha[:12]}.json").write_text(json.dumps({MARKER: marker, "sidecar": original}, indent=2) + "\n", encoding="utf-8")
    return {
        "ref_id": f"r-{sha[:8]}", "version": sha[:12], "file": f"references/{target.name}", "kind": marker["kind"],
        "title": marker["title"], "loc": marker["loc"],
        "analyst": {"repo": marker["repo"], "item": marker["item"], "run_id": marker["run_id"]},
    }  # fmt: skip


def read_import_sidecar(path: Path) -> dict[str, Any] | None:
    """The import sidecar of a file under ``references/`` when it was imported from Analyst."""
    if path.suffix.lower() not in (".png", ".csv"):
        return None
    try:
        data = json.loads(path.with_suffix(".json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    marker = data.get(MARKER) if isinstance(data, dict) else None
    return data if isinstance(marker, dict) and marker.get("kind") in ("analyst-figure", "analyst-table") else None


def extract(path: Path, ctx: Ctx, sidecar: dict[str, Any]) -> dict[str, Any]:
    marker = sidecar[MARKER]
    original = sidecar.get("sidecar") or {}
    loc = str(marker.get("loc") or _loc(str(marker.get("run_id", "")), path.stem))
    doc = new_doc(marker["kind"], clean_title(str(marker.get("title") or "")) or ctx.name or path.stem)
    doc["root_loc"] = loc
    doc["analyst"] = {"repo": marker.get("repo", ""), "item": marker.get("item", ""), "run_id": marker.get("run_id", "")}
    if marker["kind"] == "analyst-figure":
        from PIL import Image

        with Image.open(path) as image:
            width, height = image.size
        lines = [doc["title"]]
        if original.get("script"):
            lines.append("Script: " + " ".join([str(original["script"]), *[str(a) for a in original.get("args") or []]]))
        lines.append(f"Run: {marker.get('run_id', '')}")
        if original.get("target"):
            lines.append(f"Target: {original['target']}")
        for axis in original.get("axes") or []:
            if isinstance(axis, dict):
                bits = [f"{k}: {axis[k]}" for k in ("title", "xlabel", "ylabel") if axis.get(k)]
                if bits:
                    lines.append("Axes: " + "; ".join(bits))
        if original.get("created_at"):
            lines.append(f"Created: {original['created_at']}")
        text = "\n".join(lines)
        doc["texts"][""] = text
        doc["chunks"].append({"loc": loc, "heading_path": [], "text": text[:MAX_CHARS]})
        doc["images"].append({"loc": loc, "png": doc_rel(ctx, path), "width": width, "height": height, "caption": doc["title"]})
        return doc
    from .extract.csvx import read_grid

    grid, _ = read_grid(path.read_bytes())
    doc["grids"][""] = {"r0": 1, "c0": 1, "cells": grid}
    if grid:
        doc["tables"].append({"loc": loc, "title": doc["title"], "range": ["", 1, 1, len(grid), len(grid[0])]})
        # One chunk: the loc grammar addresses the whole table, so a long table is indexed by its first rows.
        text = ""
        for line in grid_lines(grid):
            if text and len(text) + 1 + len(line) > MAX_CHARS:
                break
            text = f"{text}\n{line}" if text else line[:MAX_CHARS]
        doc["chunks"].append({"loc": loc, "heading_path": [], "text": text})
    if marker.get("truncated"):
        doc["warnings"].append(diag("refs.truncated", f"the import holds the first {len(grid) - 1} of {marker.get('total_rows')} rows", "info"))
    return doc


def doc_rel(ctx: Ctx, path: Path) -> str:
    try:
        return path.resolve().relative_to(ctx.project.resolve()).as_posix()
    except ValueError:
        return path.as_posix()
