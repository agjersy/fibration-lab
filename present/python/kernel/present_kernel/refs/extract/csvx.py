"""CSV and TSV with dialect sniffing. Rows are 1-based and include the header row."""

from __future__ import annotations

import csv
import io
from pathlib import Path
from typing import Any

from ..chunker import row_groups
from ..model import Ctx, clean_title, diag, grid_lines, new_doc, square
from .common import decode_text

MAX_ROWS = 200_000


def read_grid(data: bytes, tab: bool = False) -> tuple[list[list[Any]], bool]:
    """Parsed rows (strings; empty cells None) and whether the row limit cut the file."""
    text = decode_text(data)
    sample = text[:65536]
    dialect: Any = csv.excel_tab if tab else csv.excel
    if not tab:
        try:
            dialect = csv.Sniffer().sniff(sample, delimiters=",;\t|")
        except csv.Error:
            first = sample.split("\n", 1)[0]
            best = max(",;\t|", key=first.count)
            if first.count(best) and best != ",":
                class dialect(csv.excel):
                    delimiter = best
    rows: list[list[str]] = []
    truncated = False
    csv.field_size_limit(16 * 1024 * 1024)
    for row in csv.reader(io.StringIO(text, newline=""), dialect):
        if len(rows) >= MAX_ROWS:
            truncated = True
            break
        rows.append(row)
    while rows and not any(c.strip() for c in rows[-1]):
        rows.pop()
    return square(rows), truncated


def fill_doc(doc: dict[str, Any], grid: list[list[Any]]) -> None:
    lines = grid_lines(grid)
    doc["grids"][""] = {"r0": 1, "c0": 1, "cells": grid}
    doc["root_loc"] = f"r1-{max(len(grid), 1)}"
    for a, b in row_groups(lines):
        text = "\n".join(lines[a:b + 1])
        if text.strip():
            doc["chunks"].append({"loc": f"r{a + 1}-{b + 1}", "heading_path": [], "text": text})
    if grid:
        doc["tables"].append({"loc": f"r1-{len(grid)}", "range": ["", 1, 1, len(grid), len(grid[0])]})


def extract(path: Path, ctx: Ctx) -> dict[str, Any]:
    grid, truncated = read_grid(path.read_bytes(), tab=path.suffix.lower() == ".tsv")
    header = ", ".join(str(c) for c in (grid[0] if grid else []) if c is not None)
    doc = new_doc("csv", ctx.name or clean_title(header, 80) or "Table")
    fill_doc(doc, grid)
    if truncated:
        doc["warnings"].append(diag("refs.truncated", f"only the first {MAX_ROWS} rows were read"))
    return doc
