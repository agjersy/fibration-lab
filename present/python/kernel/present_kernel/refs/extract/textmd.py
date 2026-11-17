"""Markdown and plain text. The extracted text is the file's text with normalized line breaks."""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from ..model import Ctx, clean_title
from .common import decode_text, text_doc

_ATX = re.compile(r"^ {0,3}(#{1,6})[ \t]+(.+?)[ \t]*#*[ \t]*$")
_FENCE = re.compile(r"^ {0,3}(```|~~~)")
_SEPARATOR = re.compile(r"^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$")
_INLINE = re.compile(r"[*_`]+")


def _cells(line: str) -> list[str]:
    line = line.strip()
    if line.startswith("|"):
        line = line[1:]
    if line.endswith("|") and not line.endswith("\\|"):
        line = line[:-1]
    return [c.strip().replace("\\|", "|") for c in re.split(r"(?<!\\)\|", line)]


def extract_markdown(path: Path, ctx: Ctx) -> dict[str, Any]:
    text = decode_text(path.read_bytes())
    headings: list[tuple[int, int, str, int]] = []
    tables: list[dict[str, Any]] = []
    lines = text.split("\n")
    starts: list[int] = []
    pos = 0
    for line in lines:
        starts.append(pos)
        pos += len(line) + 1
    fence: str | None = None
    i = 0
    while i < len(lines):
        line = lines[i]
        m = _FENCE.match(line)
        if m:
            fence = None if fence == m.group(1) else (fence or m.group(1))
        elif fence is None:
            h = _ATX.match(line)
            if h:
                title = _INLINE.sub("", h.group(2)).strip()
                if title:
                    headings.append((starts[i], len(h.group(1)), clean_title(title), starts[i] + len(line)))
            elif "|" in line and i + 1 < len(lines) and "|" in lines[i + 1] and _SEPARATOR.match(lines[i + 1]):
                j = i + 2
                while j < len(lines) and "|" in lines[j] and lines[j].strip():
                    j += 1
                cells = [_cells(lines[i])] + [_cells(ln) for ln in lines[i + 2:j]]
                tables.append({"start": starts[i], "end": starts[j - 1] + len(lines[j - 1]), "cells": cells})
                i = j
                continue
        i += 1
    title = next((t for _, lv, t, _ in headings if lv == 1), "") or (headings[0][2] if headings else "")
    title = title or clean_title(next((ln for ln in lines if ln.strip()), "")) or ctx.name or "Markdown document"
    return text_doc("markdown", title, text, headings, None, tables)


def extract_text(path: Path, ctx: Ctx) -> dict[str, Any]:
    text = decode_text(path.read_bytes())
    title = clean_title(next((ln for ln in text.split("\n") if ln.strip()), ""), 120) or ctx.name or "Text"
    return text_doc("text", title, text, [], None)
