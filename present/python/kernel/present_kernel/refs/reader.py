"""Read passages and tables of a project's references by citation.

``ProjectRefs(project_root)`` is used by the RPC methods, by other kernel modules (the checker's
number check, chart transforms), and by the SDK inside builds. It resolves ``ref_id`` and version
to a file through ``references/manifest.toml`` (PROTOCOL 4.3), falling back to what ``refs.ingest``
cached when the manifest has no entry yet.
"""

from __future__ import annotations

import json
import threading
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..errors import UNSUPPORTED_REFERENCE, RpcError, invalid_params, not_found
from . import locs
from .model import grid_slice, grid_text, grid_to_table, is_header, square

TEXT_KINDS = ("docx", "markdown", "text", "html", "eml", "msg")
DEFAULT_DPI = 110


@dataclass(frozen=True)
class Resolved:
    ref_id: str
    version: str
    path: Path  # absolute
    name: str = ""


class ProjectRefs:
    def __init__(self, project_root: str | Path) -> None:
        self.project = Path(project_root).resolve()
        self._docs: dict[tuple[str, str], dict[str, Any]] = {}
        self._lock = threading.RLock()

    # ---- resolution ---------------------------------------------------------------------------
    def manifest(self) -> list[dict[str, Any]]:
        path = self.project / "references" / "manifest.toml"
        try:
            data = tomllib.loads(path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError, UnicodeDecodeError):
            return []
        refs = data.get("refs", [])
        return [r for r in refs if isinstance(r, dict)] if isinstance(refs, list) else []

    def _versioned_file(self, version: str, like: Path | None) -> Path | None:
        folder = self.project / "references"
        found = sorted(p for p in folder.glob(f"{version}.*") if p.is_file())
        if like is not None:
            same = [p for p in found if p.suffix.lower() == like.suffix.lower()]
            if same:
                return same[0]
        other = [p for p in found if p.suffix.lower() != ".json"] or found
        return other[0] if other else None

    def resolve(self, ref_id: str, version: str | None = None) -> Resolved:
        """The file for a reference version; the current version when ``version`` is None."""
        from ..paths import resolve_in_root

        for entry in self.manifest():
            if entry.get("id") != ref_id:
                continue
            current = str(entry.get("version", ""))
            file = str(entry.get("file", ""))
            name = Path(str(entry.get("original_path", "")).replace("\\", "/")).name
            if version is None or not current or current.startswith(version) or version.startswith(current):
                path = resolve_in_root(self.project, file)
                if not path.is_file():
                    raise not_found(f"reference {ref_id}: file {file} is missing")
                return Resolved(ref_id, current or version or path.stem, path, name)
            for previous in entry.get("previous_versions", []) or []:
                old = previous.get("version", "") if isinstance(previous, dict) else str(previous)
                if not old or not (old.startswith(version) or version.startswith(old)):
                    continue
                old_file = previous.get("file") if isinstance(previous, dict) else None
                path = resolve_in_root(self.project, old_file) if old_file else self._versioned_file(old, Path(file))
                if path is None or not path.is_file():
                    raise not_found(f"reference {ref_id}: the file of version {old} is missing")
                return Resolved(ref_id, old, path, name)
            raise not_found(f"reference {ref_id} has no version {version}")
        # Not in the manifest (yet): what refs.ingest recorded in the cache.
        folder = self.project / ".present" / "cache" / "refs" / ref_id
        candidates = []
        if folder.is_dir():
            for doc_path in folder.glob("*/doc.json"):
                if version is None or doc_path.parent.name.startswith(version) or version.startswith(doc_path.parent.name):
                    candidates.append(doc_path)
        for doc_path in sorted(candidates, key=lambda p: p.stat().st_mtime, reverse=True):
            try:
                meta = json.loads(doc_path.read_text(encoding="utf-8"))
                path = resolve_in_root(self.project, meta["file"])
            except (OSError, ValueError, KeyError, RpcError):
                continue
            if path.is_file():
                return Resolved(ref_id, doc_path.parent.name, path, meta.get("name", ""))
        raise not_found(f"reference {ref_id}{'@' + version if version else ''} not found")

    def doc(self, ref_id: str, version: str | None = None) -> dict[str, Any]:
        """The extracted document (from memory, the cache, or a fresh extraction)."""
        from . import ingest

        resolved = self.resolve(ref_id, version)
        key = (ref_id, resolved.version)
        with self._lock:
            if key not in self._docs:
                doc = ingest.load_cached(self.project, ref_id, resolved.version)
                if doc is None or doc.get("file") != resolved.path.relative_to(self.project).as_posix():
                    doc = ingest.extract_file(self.project, resolved.path, ref_id, resolved.version, resolved.name)
                self._docs[key] = doc
            return self._docs[key]

    def forget(self, ref_id: str | None = None) -> None:
        with self._lock:
            for key in [k for k in self._docs if ref_id is None or k[0] == ref_id]:
                del self._docs[key]

    def _child(self, doc: dict[str, Any], n: int) -> dict[str, Any]:
        from . import ingest

        for att in doc.get("attachments", []):
            if att["n"] == n:
                key = (att["ref_id"], att["version"])
                with self._lock:
                    if key not in self._docs:
                        child = ingest.load_cached(self.project, *key)
                        if child is None:
                            path = self.project / att["file"]
                            if not path.is_file():
                                raise not_found(f"attachment {n}: file {att['file']} is missing")
                            child = ingest.extract_file(self.project, path, att["ref_id"], att["version"], att["name"], 1)
                        self._docs[key] = child
                    return self._docs[key]
        raise not_found(f"no attachment {n} in this reference")

    # ---- public API ---------------------------------------------------------------------------
    def text(self, cite: str) -> str:
        """The text a citation points at."""
        c = locs.parse_cite(cite)
        return self.read(c.ref_id, c.loc, None, c.version)["text"]

    def table(self, cite: str) -> dict[str, Any]:
        """The table a citation points at: ``{"columns": [...], "rows": [[...]], "loc": str}``."""
        c = locs.parse_cite(cite)
        return self.table_at(c.ref_id, c.loc, c.version)

    def read(self, ref_id: str, loc: str = "", max_chars: int | None = None, version: str | None = None) -> dict[str, Any]:
        """``refs.read``: ``{text, loc, truncated}``; an empty loc reads from the start of the reference."""
        text, out_loc = self._read(self.doc(ref_id, version), (loc or "").strip())
        truncated = max_chars is not None and max_chars >= 0 and len(text) > max_chars
        if truncated:
            text = text[:max_chars]
            out_loc = _shorten(out_loc, max_chars)
        return {"text": text, "loc": out_loc, "truncated": truncated}

    def table_at(self, ref_id: str, loc: str, version: str | None = None) -> dict[str, Any]:
        """``refs.table``."""
        return self._table(self.doc(ref_id, version), (loc or "").strip())

    def page_image(self, ref_id: str, page: int = 1, dpi: int | None = None, version: str | None = None) -> dict[str, str]:
        """``refs.page_image``: renders a PDF page (or returns an image reference) as a PNG under the cache."""
        from . import ingest

        dpi = int(dpi or DEFAULT_DPI)
        if not 20 <= dpi <= 600:
            raise invalid_params("dpi must be between 20 and 600")
        resolved = self.resolve(ref_id, version)
        doc = self.doc(ref_id, version)
        kind = doc["kind"]
        folder = ingest.cache_dir(self.project, ref_id, resolved.version)
        if kind == "pdf":
            from .extract.pdf import render_page

            out = folder / f"page-{int(page)}-{dpi}.png"
            if not out.is_file():
                render_page(resolved.path, int(page), dpi, out)
            return {"png": out.relative_to(self.project).as_posix()}
        if kind in ("image", "analyst-figure"):
            if page != 1:
                raise not_found("an image reference has only page 1")
            if resolved.path.suffix.lower() == ".png":
                return {"png": resolved.path.relative_to(self.project).as_posix()}
            out = folder / "full.png"
            if not out.is_file():
                from PIL import Image

                out.parent.mkdir(parents=True, exist_ok=True)
                with Image.open(resolved.path) as image:
                    image.convert("RGBA" if "A" in image.mode or image.mode == "P" else "RGB").save(out, format="PNG")
            return {"png": out.relative_to(self.project).as_posix()}
        raise RpcError(UNSUPPORTED_REFERENCE, f"page images are available for PDF and image references, not {kind}")

    # ---- loc resolution -----------------------------------------------------------------------
    def _find_table(self, doc: dict[str, Any], loc: str) -> dict[str, Any] | None:
        for table in doc["tables"]:
            if table["loc"] == loc:
                return table
        split = locs.split_table(loc)
        if split and doc["kind"] in TEXT_KINDS:
            # Tables of a plain-text document are numbered over the whole document; the base is advisory.
            suffix = f"/t{split[1]}"
            for table in doc["tables"]:
                if table["loc"].endswith(suffix):
                    return table
        return None

    def _cells(self, doc: dict[str, Any], table: dict[str, Any]) -> tuple[list[list[Any]], list[Any] | None]:
        """A table's cells and, for a range that starts below a header row, that header."""
        if table.get("range"):
            return self._range_cells(doc, *table["range"])
        return table["cells"], None

    def _range_cells(self, doc: dict[str, Any], name: str, r1: int, c1: int, r2: int, c2: int) -> tuple[list[list[Any]], list[Any] | None]:
        grid = doc["grids"].get(name)
        if grid is None:
            raise not_found(f"no sheet {name!r} in this reference")
        if r2 - r1 > 2_000_000 or c2 - c1 > 20_000:
            raise invalid_params("range is too large")
        r2 = min(r2, grid["r0"] + len(grid["cells"]) - 1)
        c2 = min(c2, grid["c0"] + (len(grid["cells"][0]) if grid["cells"] else 0) - 1)
        cells = grid_slice(grid, r1, c1, max(r1, r2), max(c1, c2))
        header = None
        if r1 > grid["r0"]:
            first = grid_slice(grid, grid["r0"], c1, grid["r0"], max(c1, c2))[0]
            header = first if is_header(first) else None
        return cells, header

    def _sheet_range(self, doc: dict[str, Any], loc: str) -> tuple[str, int, int, int, int] | None:
        kind = doc["kind"]
        if kind == "xlsx":
            if loc.startswith("name:"):
                target = doc["names"].get(loc[5:])
                if target is None:
                    raise not_found(f"no named range {loc[5:]!r}")
                loc = target
            parsed = locs.parse_sheet_range(loc)
            if parsed:
                return (parsed[0], *parsed[1])
            if loc in doc["grids"]:
                grid = doc["grids"][loc]
                return loc, grid["r0"], grid["c0"], grid["r0"] + len(grid["cells"]) - 1, grid["c0"] + len(grid["cells"][0]) - 1
            return None
        if kind in ("csv", "analyst-table"):
            parsed = locs.parse_csv(loc)
            grid = doc["grids"].get("")
            if grid is None:
                return None
            width = len(grid["cells"][0]) if grid["cells"] else 1
            if parsed:
                r1, r2, c1, c2 = parsed
                return "", min(r1, r2), min(c1 or 1, c2 or width), max(r1, r2), max(c1 or 1, c2 or width)
            if kind == "analyst-table" and loc.startswith("run:"):
                return "", 1, 1, max(len(grid["cells"]), 1), width
        return None

    def _read(self, doc: dict[str, Any], loc: str) -> tuple[str, str]:
        att = locs.split_attachment(loc)
        if att is not None:
            text, sub = self._read(self._child(doc, att[0]), att[1])
            return text, f"att{att[0]}/{sub}"
        loc = loc or doc["root_loc"]
        kind = doc["kind"]
        table = self._find_table(doc, loc)
        if table is not None and not table.get("range"):
            return grid_text(square(table["cells"])), table["loc"]
        if kind == "pdf":
            parsed = locs.parse_pdf(loc)
            if parsed:
                page, start, end = parsed
                text = doc["texts"].get(f"p{page}")
                if text is None:
                    raise not_found(f"page {page} is out of range (1 to {doc.get('pages', 0)})")
                if start is None:
                    return text, f"p{page}"
                start, end = min(start, len(text)), min(max(end or 0, start), len(text))
                return text[start:end], f"p{page}:c{start}-{end}"
        elif kind in TEXT_KINDS:
            parsed = locs.parse_chars(loc)
            if parsed:
                text = doc["texts"].get("", "")
                start = min(parsed[0], len(text))
                end = min(max(parsed[1], start), len(text))
                return text[start:end], f"c{start}-{end}"
        elif kind == "pptx":
            if locs.parse_pptx(loc):
                slide = loc.split("/")[0]
                if slide not in doc["texts"]:
                    raise not_found(f"slide {slide[1:]} is out of range (1 to {doc.get('pages', 0)})")
                if loc in doc["texts"]:
                    return doc["texts"][loc], loc
                raise not_found(f"no text in shape {loc}")
        elif kind == "image":
            if loc == "full" or loc.startswith("box:"):
                return "", loc
        elif kind == "analyst-figure" and loc.startswith("run:"):
            return doc["texts"].get("", ""), doc["root_loc"]
        rng = self._sheet_range(doc, loc)
        if rng is not None:
            cells, _ = self._range_cells(doc, *rng)
            return grid_text(cells), loc
        raise invalid_params(f"{loc!r} is not a location in a {kind} reference (PROTOCOL 2.3)")

    def _table(self, doc: dict[str, Any], loc: str) -> dict[str, Any]:
        att = locs.split_attachment(loc)
        if att is not None:
            table = self._table(self._child(doc, att[0]), att[1])
            table["loc"] = f"att{att[0]}/{table['loc']}"
            return table
        if not loc and len(doc["tables"]) == 1:
            loc = doc["tables"][0]["loc"]
        table = self._find_table(doc, loc)
        if table is not None:
            cells, header = self._cells(doc, table)
            return grid_to_table(cells, table["loc"], header)
        rng = self._sheet_range(doc, loc)
        if rng is not None:
            cells, header = self._range_cells(doc, *rng)
            return grid_to_table(cells, loc, header)
        raise not_found(f"no table at {loc!r} in this {doc['kind']} reference")


def _shorten(loc: str, max_chars: int) -> str:
    """After truncation, a character-range loc is narrowed to what was returned."""
    head, sep, tail = loc.rpartition("/")
    prefix = f"{head}{sep}" if sep and head.startswith("att") else ""
    last = tail if prefix else loc
    pdf = locs.parse_pdf(last)
    if pdf and pdf[1] is not None:
        return f"{prefix}p{pdf[0]}:c{pdf[1]}-{pdf[1] + max_chars}"
    chars = locs.parse_chars(last)
    if chars:
        return f"{prefix}c{chars[0]}-{chars[0] + max_chars}"
    return loc
