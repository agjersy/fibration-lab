"""Shared pieces of the extractors: the extraction context, diagnostics, and table helpers.

An extractor returns an internal *document* dict. It carries the public ``refs.ingest`` fields
(title, pages, outline, chunks, images, warnings) plus what the reader needs to resolve any loc
without the original parser: ``texts`` (plain text per page, slide, shape, or whole document),
``grids`` (cell grids of sheets and CSV files), ``tables`` (with their cells), ``names`` (named
ranges), ``attachments``, and ``root_loc`` (a loc that addresses the whole document).
"""

from __future__ import annotations

import io
import re
from dataclasses import dataclass, field
from datetime import date, datetime, time
from pathlib import Path
from typing import Any

DOC_FORMAT = 1
MAX_IMAGES = 200
MIN_IMAGE_PX = 24

_NUMBER = re.compile(r"^[+-]?(?:0|[1-9]\d*)(?:\.\d+)?$")
_CELL_WS = re.compile(r"[\t\r\n\x0b\x0c]+")

Cell = str | int | float | None


def diag(rule: str, message: str, severity: str = "warn", **extra: Any) -> dict[str, Any]:
    return {"rule": rule, "severity": severity, "message": message, **extra}


@dataclass
class Ctx:
    """Where an extraction writes its by-products."""

    project: Path
    ref_id: str
    version: str
    name: str = ""  # display name when one is known: an attachment's file name, or the original file name
    file_name: str = ""  # the content-addressed name under references/
    depth: int = 0
    attach: Any = None  # callable(n, filename, data) -> attachment dict | None, set by ingest
    _images: int = field(default=0, repr=False)

    @property
    def label(self) -> str:
        """What to call the file in messages."""
        return self.name or self.file_name

    @property
    def cache_rel(self) -> str:
        return f".present/cache/refs/{self.ref_id}/{self.version}"

    @property
    def cache_dir(self) -> Path:
        return self.project / ".present" / "cache" / "refs" / self.ref_id / self.version

    def save_image(self, name: str, image: Any, loc: str, caption: str | None = None) -> dict[str, Any] | None:
        """Write a PIL image (or encoded bytes) as PNG in the cache; returns an ImageRef or None
        when the image is tiny, unreadable, or over the per-reference limit."""
        from PIL import Image

        if self._images >= MAX_IMAGES:
            return None
        try:
            if isinstance(image, (bytes, bytearray)):
                image = Image.open(io.BytesIO(image))
                image.load()
            if image.width < MIN_IMAGE_PX or image.height < MIN_IMAGE_PX:
                return None
            if image.mode not in ("RGB", "RGBA", "L", "LA"):
                image = image.convert("RGBA" if "A" in image.mode or image.mode == "P" else "RGB")
            self.cache_dir.mkdir(parents=True, exist_ok=True)
            image.save(self.cache_dir / f"{name}.png", format="PNG")
        except Exception:
            return None
        self._images += 1
        ref = {"loc": loc, "png": f"{self.cache_rel}/{name}.png", "width": image.width, "height": image.height}
        if caption:
            ref["caption"] = caption
        return ref


# ---- cells and tables -------------------------------------------------------------------------
def json_cell(value: Any) -> Cell:
    """A JSON-safe cell that keeps the source's type: dates become ISO strings, empty becomes None."""
    if value is None:
        return None
    if isinstance(value, bool):
        return "TRUE" if value else "FALSE"
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        if value != value or value in (float("inf"), float("-inf")):
            return None
        return value
    if isinstance(value, datetime):
        return value.isoformat(sep=" ") if (value.hour or value.minute or value.second) else value.date().isoformat()
    if isinstance(value, (date, time)):
        return value.isoformat()
    text = str(value)
    return text if text.strip() else None


def coerce_cell(value: Any) -> Cell:
    """``json_cell`` plus: plain numeric strings (``12``, ``-3.5``; no leading zeros, no separators) become numbers."""
    value = json_cell(value)
    if isinstance(value, str):
        text = value.strip()
        if _NUMBER.match(text) and len(text) <= 15:
            return float(text) if "." in text else int(text)
        return text
    return value


def fmt_cell(value: Cell) -> str:
    if value is None:
        return ""
    if isinstance(value, float):
        return str(int(value)) if value.is_integer() and abs(value) < 1e15 else repr(value)
    return _CELL_WS.sub(" ", str(value))


def square(grid: list[list[Any]], coerce: bool = False) -> list[list[Cell]]:
    """Pad rows to one width; ``coerce`` also turns numeric strings into numbers (for ``Table``)."""
    width = max((len(r) for r in grid), default=0)
    conv = coerce_cell if coerce else json_cell
    return [[conv(c) for c in row] + [None] * (width - len(row)) for row in grid]


def grid_lines(grid: list[list[Cell]]) -> list[str]:
    return ["\t".join(fmt_cell(c) for c in row) for row in grid]


def grid_text(grid: list[list[Cell]]) -> str:
    return "\n".join(grid_lines(grid))


def grid_to_table(grid: list[list[Any]], loc: str, header: list[Any] | None = None) -> dict[str, Any]:
    """PROTOCOL ``Table``. ``header`` carries column names borrowed from the sheet's header row
    when the range starts below it; otherwise the first row is the header when every cell in it
    is a string; otherwise the columns are ``col<n>``."""
    raw = square(grid)
    grid = square(grid, coerce=True)
    width = len(grid[0]) if grid else 0
    if header and len(header) == width:
        columns, rows = [str(c).strip() for c in header], grid
    elif raw and is_header(raw[0]):
        columns, rows = [str(c).strip() for c in raw[0]], grid[1:]
    else:
        columns, rows = [f"col{i + 1}" for i in range(width)], grid
    return {"columns": columns, "rows": rows, "loc": loc}


def is_header(row: list[Any]) -> bool:
    """A header row: every cell is a non-empty string, and they are not all numerals."""
    if not row or not all(isinstance(c, str) and c.strip() for c in row):
        return False
    return any(not isinstance(coerce_cell(c), (int, float)) for c in row)


def table_ref(table: dict[str, Any]) -> dict[str, Any]:
    """PROTOCOL ``TableRef``. A table holds its ``cells`` or a ``range`` into one of the document's grids."""
    if table.get("range"):
        _, r1, c1, r2, c2 = table["range"]
        rows, cols = r2 - r1 + 1, c2 - c1 + 1
    else:
        cells = table["cells"]
        rows, cols = len(cells), max((len(r) for r in cells), default=0)
    ref = {"loc": table["loc"], "rows": rows, "cols": cols}
    if table.get("title"):
        ref["title"] = table["title"]
    return ref


def grid_slice(grid: dict[str, Any], r1: int, c1: int, r2: int, c2: int) -> list[list[Cell]]:
    """Cells of an absolute 1-based range from a grid ``{r0, c0, cells}``; outside the used range is None."""
    r0, c0, cells = grid["r0"], grid["c0"], grid["cells"]
    out: list[list[Cell]] = []
    for r in range(r1, r2 + 1):
        row = cells[r - r0] if 0 <= r - r0 < len(cells) else []
        out.append([row[c - c0] if 0 <= c - c0 < len(row) else None for c in range(c1, c2 + 1)])
    return out


def new_doc(kind: str, title: str) -> dict[str, Any]:
    return {"format": DOC_FORMAT, "kind": kind, "title": title, "outline": [], "chunks": [], "tables": [],
            "images": [], "warnings": [], "texts": {}, "grids": {}, "names": {}, "attachments": [],
            "root_loc": ""}  # fmt: skip


def clean_title(text: str, limit: int = 160) -> str:
    text = " ".join((text or "").split())
    return text if len(text) <= limit else text[: limit - 1].rstrip() + "…"
