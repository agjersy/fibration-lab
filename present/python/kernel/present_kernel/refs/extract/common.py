"""Helpers shared by the extractors whose loc is ``c<start>-<end>`` over one plain text."""

from __future__ import annotations

from typing import Any

from ..chunker import chunk_sections, outline_from_headings, paragraph_breaks, sections_from_headings
from ..model import new_doc


def decode_text(data: bytes) -> str:
    """Decode a text file: BOMs first, then UTF-8, then Windows-1252."""
    if data.startswith((b"\xff\xfe", b"\xfe\xff")):
        text = data.decode("utf-16", errors="replace")
    else:
        try:
            text = data.decode("utf-8-sig")
        except UnicodeDecodeError:
            text = data.decode("cp1252", errors="replace")
    return text.replace("\r\n", "\n").replace("\r", "\n").replace("\x00", "")


class TextBuilder:
    """Accumulates blocks into one plain text and records headings, paragraph starts, and tables."""

    def __init__(self) -> None:
        self.parts: list[str] = []
        self.length = 0
        self.breaks: list[int] = []
        self.headings: list[tuple[int, int, str, int]] = []  # offset, level, title, end
        self.tables: list[dict[str, Any]] = []

    def _add(self, text: str) -> tuple[int, int]:
        if self.parts:
            self.parts.append("\n\n")
            self.length += 2
        start = self.length
        self.parts.append(text)
        self.length += len(text)
        self.breaks.append(start)
        return start, self.length

    def paragraph(self, text: str) -> tuple[int, int] | None:
        text = text.strip("\n").rstrip()
        return self._add(text) if text.strip() else None

    def heading(self, level: int, text: str) -> None:
        title = " ".join(text.split())
        if title:
            start, end = self._add(title)
            self.headings.append((start, max(1, min(level, 9)), title, end))

    def table(self, cells: list[list[Any]], title: str | None = None) -> None:
        from ..model import grid_text, square

        grid = square(cells)
        if not grid or not grid[0]:
            return
        start, end = self._add(grid_text(grid))
        self.tables.append({"start": start, "end": end, "cells": grid, "title": title})

    @property
    def text(self) -> str:
        return "".join(self.parts)


def text_doc(kind: str, title: str, text: str, headings: list[tuple[int, int, str, int]], breaks: list[int] | None,
             tables: list[dict[str, Any]] = ()) -> dict[str, Any]:  # fmt: skip
    """A document over one plain text: sections from headings, chunks, outline, tables."""
    doc = new_doc(kind, title)
    doc["texts"][""] = text
    doc["root_loc"] = f"c0-{len(text)}"
    flat = [(o, lv, t) for o, lv, t, _ in headings]
    doc["chunks"] = chunk_sections(text, sections_from_headings(len(text), flat),
                                   paragraph_breaks(text) if breaks is None else sorted(breaks))  # fmt: skip
    doc["outline"] = outline_from_headings(flat, lambda i: f"c{headings[i][0]}-{headings[i][3]}")
    for n, table in enumerate(tables, start=1):
        entry = {"loc": f"c{table['start']}-{table['end']}/t{n}", "cells": table["cells"]}
        if table.get("title"):
            entry["title"] = table["title"]
        doc["tables"].append(entry)
    return doc


def built_doc(kind: str, title: str, builder: TextBuilder) -> dict[str, Any]:
    return text_doc(kind, title, builder.text, builder.headings, builder.breaks, builder.tables)
