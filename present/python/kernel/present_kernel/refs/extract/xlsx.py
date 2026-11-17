"""XLSX with openpyxl (evaluated values only): each sheet's used range as a table, named ranges.

Locs are ``<sheet>!<A1 range>`` and ``name:<named range>``. Chunk text is one line per row, cells
separated by tabs.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from ..chunker import row_groups
from ..locs import a1_range, parse_a1
from ..model import Ctx, clean_title, diag, grid_lines, json_cell, new_doc

MAX_CELLS = 2_000_000


def _used(ws) -> tuple[int, int, list[list[Any]]] | None:
    """First row, first column, and cells of the sheet's non-empty bounding box."""
    rows: list[tuple[int, tuple]] = []
    lo = hi = None
    for r, row in enumerate(ws.iter_rows(min_row=1, min_col=1, values_only=True), start=1):
        idx = [i for i, v in enumerate(row) if v is not None and v != ""]
        if not idx:
            continue
        rows.append((r, row))
        lo = idx[0] if lo is None else min(lo, idx[0])
        hi = idx[-1] if hi is None else max(hi, idx[-1])
    if not rows or lo is None or hi is None:
        return None
    first, last = rows[0][0], rows[-1][0]
    by_row = dict(rows)
    cells = [[json_cell(v) for v in by_row[r][lo:hi + 1]] if r in by_row else [None] * (hi - lo + 1)
             for r in range(first, last + 1)]  # fmt: skip
    return first, lo + 1, cells


def extract(path: Path, ctx: Ctx) -> dict[str, Any]:
    from openpyxl import load_workbook

    wb = load_workbook(str(path), data_only=True)
    try:
        doc = new_doc("xlsx", "")
        total = 0
        nodes: dict[str, dict[str, Any]] = {}
        for ws in wb.worksheets:
            used = _used(ws)
            if used is None:
                continue
            r0, c0, cells = used
            total += len(cells) * len(cells[0])
            if total > MAX_CELLS:
                doc["warnings"].append(diag("refs.truncated", f"sheet {ws.title!r} and later sheets were skipped: the workbook has more than {MAX_CELLS} cells"))
                break
            name = ws.title
            width = len(cells[0])
            doc["grids"][name] = {"r0": r0, "c0": c0, "cells": cells}
            loc = f"{name}!{a1_range(r0, c0, r0 + len(cells) - 1, c0 + width - 1)}"
            doc["root_loc"] = doc["root_loc"] or loc
            doc["tables"].append({"loc": loc, "title": name, "range": [name, r0, c0, r0 + len(cells) - 1, c0 + width - 1]})
            nodes[name] = {"title": name, "loc": loc, "children": []}
            doc["outline"].append(nodes[name])
            lines = grid_lines(cells)
            for a, b in row_groups(lines):
                text = "\n".join(lines[a:b + 1])
                if text.strip():
                    doc["chunks"].append({"loc": f"{name}!{a1_range(r0 + a, c0, r0 + b, c0 + width - 1)}",
                                          "heading_path": [name], "text": text})  # fmt: skip
        for name, defined in _defined_names(wb):
            try:
                destinations = list(defined.destinations)
            except Exception:
                continue
            if len(destinations) != 1:
                continue
            sheet, coord = destinations[0]
            cells = parse_a1(coord.replace("$", ""))
            if sheet not in doc["grids"] or cells is None:
                continue
            doc["names"][name] = f"{sheet}!{a1_range(*cells)}"
            doc["tables"].append({"loc": f"name:{name}", "title": name, "range": [sheet, *cells]})
            nodes[sheet]["children"].append({"title": name, "loc": f"name:{name}", "children": []})
        try:
            core_title = clean_title(wb.properties.title or "")
        except Exception:
            core_title = ""
        doc["title"] = core_title or ctx.name or clean_title(", ".join(doc["grids"]), 80) or "Workbook"
        doc["root_loc"] = doc["root_loc"] or f"{wb.worksheets[0].title if wb.worksheets else 'Sheet1'}!A1"
        return doc
    finally:
        wb.close()


def _defined_names(wb) -> list[tuple[str, Any]]:
    names = wb.defined_names
    try:
        return [(k, v) for k, v in names.items()]
    except AttributeError:  # openpyxl < 3.1
        return [(d.name, d) for d in names.definedName]
