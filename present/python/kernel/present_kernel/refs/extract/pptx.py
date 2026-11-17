"""PPTX with python-pptx: text per slide and shape, tables, chart data, pictures, notes.

Locs are ``s<slide>`` and ``s<slide>/sh<shape_id>``. Speaker notes have no shape on the slide, so
they are addressed as ``s<slide>/sh0`` (shape ids on a slide start at 1).
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from ..chunker import MAX_CHARS, chunk_spans
from ..model import Ctx, clean_title, grid_text, new_doc, square

NOTES_SHAPE = 0


def _chart_cells(chart) -> list[list[Any]]:
    cells: list[list[Any]] = []
    for plot in chart.plots:
        try:
            categories = [str(c) if c is not None else "" for c in plot.categories]
        except Exception:
            categories = []
        series = list(plot.series)
        values = []
        for s in series:
            try:
                values.append(list(s.values))
            except Exception:
                values.append([])
        rows = max([len(categories), *[len(v) for v in values]], default=0)
        if not series or not rows:
            continue
        if cells:
            cells.append([])
        cells.append(["Category", *[(s.name or f"Series {i + 1}") for i, s in enumerate(series)]])
        for r in range(rows):
            cells.append([categories[r] if r < len(categories) else "", *[(v[r] if r < len(v) else None) for v in values]])
    return cells


def _walk(shapes):
    for shape in shapes:
        if shape.shape_type is not None and getattr(shape, "shapes", None) is not None:
            yield from _walk(shape.shapes)
        else:
            yield shape


def extract(path: Path, ctx: Ctx) -> dict[str, Any]:
    from pptx import Presentation

    prs = Presentation(str(path))
    doc = new_doc("pptx", "")
    doc["root_loc"] = "s1"
    first_title = ""
    for number, slide in enumerate(prs.slides, start=1):
        try:
            title_shape = slide.shapes.title
        except Exception:
            title_shape = None
        title = clean_title(title_shape.text_frame.text) if title_shape is not None and title_shape.has_text_frame else ""
        first_title = first_title or title
        heading = [title or f"Slide {number}"]
        parts: list[tuple[int, str]] = []
        for shape in _walk(slide.shapes):
            shape_id = int(shape.shape_id)
            loc = f"s{number}/sh{shape_id}"
            text = ""
            cells: list[list[Any]] | None = None
            table_title = title or None
            if getattr(shape, "has_chart", False) and shape.has_chart:
                chart = shape.chart
                cells = _chart_cells(chart)
                try:
                    if chart.has_title and chart.chart_title.has_text_frame:
                        table_title = clean_title(chart.chart_title.text_frame.text) or table_title
                except Exception:
                    pass
            elif getattr(shape, "has_table", False) and shape.has_table:
                cells = [[" ".join(cell.text.split()) for cell in row.cells] for row in shape.table.rows]
            elif getattr(shape, "has_text_frame", False) and shape.has_text_frame:
                text = shape.text_frame.text.replace("\x0b", "\n").strip()
            elif hasattr(shape, "image"):
                try:
                    ref = ctx.save_image(f"img-s{number}-sh{shape_id}", shape.image.blob, loc, shape.name or None)
                except Exception:
                    ref = None
                if ref:
                    doc["images"].append(ref)
            if cells:
                grid = square(cells)
                text = grid_text(grid)
                entry = {"loc": f"{loc}/t1", "cells": grid}
                if table_title:
                    entry["title"] = table_title
                doc["tables"].append(entry)
            if text:
                doc["texts"][loc] = text
                parts.append((shape_id, text))
        notes = ""
        if slide.has_notes_slide and slide.notes_slide.notes_text_frame is not None:
            notes = slide.notes_slide.notes_text_frame.text.replace("\x0b", "\n").strip()
        if notes:
            doc["texts"][f"s{number}/sh{NOTES_SHAPE}"] = notes
        slide_text = "\n".join([t for _, t in parts] + ([f"Notes: {notes}"] if notes else []))
        doc["texts"][f"s{number}"] = slide_text
        doc["outline"].append({"title": heading[0], "loc": f"s{number}", "children": []})
        if not slide_text:
            continue
        if len(slide_text) <= MAX_CHARS:
            doc["chunks"].append({"loc": f"s{number}", "heading_path": heading, "text": slide_text})
        else:
            # The loc grammar has no character ranges inside a slide, so a long slide is split by
            # shape, and a shape longer than a chunk becomes several chunks that share the shape's loc.
            pieces = [(f"s{number}/sh{shape_id}", heading, text) for shape_id, text in parts]
            if notes:
                pieces.append((f"s{number}/sh{NOTES_SHAPE}", [*heading, "Notes"], notes))
            for loc, path_, text in pieces:
                breaks = [i + 1 for i, ch in enumerate(text) if ch == "\n"]
                for a, b in chunk_spans(text, 0, len(text), breaks):
                    doc["chunks"].append({"loc": loc, "heading_path": path_, "text": text[a:b]})
    doc["pages"] = len(doc["outline"])
    try:
        core_title = clean_title(prs.core_properties.title or "")
    except Exception:
        core_title = ""
    if core_title.lower() == "powerpoint presentation":
        core_title = ""
    doc["title"] = core_title or first_title or ctx.name or "Presentation"
    return doc
