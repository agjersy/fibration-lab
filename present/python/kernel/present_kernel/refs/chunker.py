"""Chunking (PROTOCOL 8.1): 800 to 1200 characters on paragraph boundaries within a heading
section, 150-character overlap, never across PDF pages.

The chunker works on character spans of one extracted text, so a chunk's text is always exactly
``text[start:end]`` and its location round-trips through ``refs.read``.
"""

from __future__ import annotations

import re
from bisect import bisect_left, bisect_right
from dataclasses import dataclass, field

MIN_CHARS = 800
MAX_CHARS = 1200
OVERLAP = 150

_SENTENCE_END = re.compile(r"[.!?;:]['\")\]]?\s")
_SPACE = re.compile(r"\s")


@dataclass
class Section:
    """A heading section: ``[start, end)`` in the text, the heading path, and paragraph starts."""

    start: int
    end: int
    heading_path: list[str] = field(default_factory=list)


def _trim(text: str, start: int, end: int) -> tuple[int, int]:
    while start < end and text[start].isspace():
        start += 1
    while end > start and text[end - 1].isspace():
        end -= 1
    return start, end


def _cut(text: str, pos: int, end: int, breaks: list[int]) -> int:
    """Where the chunk starting at ``pos`` ends: the last paragraph boundary in the window,
    else the last sentence end, else the last whitespace, else a hard cut."""
    lo, hi = pos + MIN_CHARS, min(pos + MAX_CHARS, end)
    i = bisect_right(breaks, hi) - 1
    if i >= 0 and breaks[i] >= lo:
        return breaks[i]
    window = text[lo:hi]
    last = None
    for m in _SENTENCE_END.finditer(window):
        last = m
    if last is not None:
        return lo + last.end()
    for j in range(len(window) - 1, -1, -1):
        if window[j].isspace():
            return lo + j + 1
    return hi


def _overlap_start(text: str, prev_start: int, cut: int) -> int:
    """Start of the next chunk: at most 150 characters before ``cut``, moved forward to a word start."""
    pos = max(cut - OVERLAP, prev_start + 1)
    if pos > 0 and not text[pos - 1].isspace():
        m = _SPACE.search(text, pos, cut)
        pos = m.end() if m else cut
    return min(pos, cut)


def chunk_spans(text: str, start: int, end: int, breaks: list[int]) -> list[tuple[int, int]]:
    """Chunk ``text[start:end]``; ``breaks`` are sorted offsets where paragraphs start."""
    spans: list[tuple[int, int]] = []
    pos, end = _trim(text, start, end)
    breaks = breaks[bisect_left(breaks, pos):bisect_right(breaks, end)]
    while pos < end:
        if end - pos <= MAX_CHARS:
            spans.append((pos, end))
            break
        cut = _cut(text, pos, end, breaks)
        a, b = _trim(text, pos, cut)
        if b > a:
            spans.append((a, b))
        nxt = _overlap_start(text, pos, b)
        pos = _trim(text, max(nxt, pos + 1), end)[0]
    return spans


def chunk_sections(text: str, sections: list[Section], breaks: list[int], loc_prefix: str = "") -> list[dict]:
    """Chunks for every section, with ``loc`` = ``<loc_prefix>c<start>-<end>``."""
    out: list[dict] = []
    for sec in sections:
        for a, b in chunk_spans(text, sec.start, sec.end, breaks):
            out.append({"loc": f"{loc_prefix}c{a}-{b}", "heading_path": list(sec.heading_path), "text": text[a:b]})
    return out


def paragraph_breaks(text: str) -> list[int]:
    """Offsets where a paragraph starts in plain text: after one or more blank lines."""
    return [m.end() for m in re.finditer(r"\n[ \t]*\n\s*", text)]


def sections_from_headings(text_len: int, headings: list[tuple[int, int, str]]) -> list[Section]:
    """Sections from ``(offset, level, title)`` headings; the heading line belongs to its section."""
    sections: list[Section] = []
    stack: list[tuple[int, str]] = []
    prev = 0
    path: list[str] = []
    for offset, level, title in sorted(headings):
        if offset > prev:
            sections.append(Section(prev, offset, path))
        while stack and stack[-1][0] >= level:
            stack.pop()
        stack.append((level, title))
        path = [t for _, t in stack]
        prev = offset
    if text_len > prev:
        sections.append(Section(prev, text_len, path))
    return sections


def outline_from_headings(headings: list[tuple[int, int, str]], loc_of) -> list[dict]:
    """Nested OutlineNode list from ``(offset, level, title)``; ``loc_of(index)`` gives each loc."""
    root: list[dict] = []
    stack: list[tuple[int, dict]] = []
    for i, (_, level, title) in enumerate(headings):
        node = {"title": title, "loc": loc_of(i), "children": []}
        while stack and stack[-1][0] >= level:
            stack.pop()
        (stack[-1][1]["children"] if stack else root).append(node)
        stack.append((level, node))
    return root


def row_groups(lines: list[str]) -> list[tuple[int, int]]:
    """Group consecutive row lines into chunks of at most MAX_CHARS: ``(first, last)`` indexes, inclusive.

    Rows cannot overlap by characters, so tabular chunks do not overlap; a single row longer than
    the maximum becomes its own chunk.
    """
    groups: list[tuple[int, int]] = []
    first, size = 0, 0
    for i, line in enumerate(lines):
        add = len(line) + (1 if i > first else 0)
        if i > first and size + add > MAX_CHARS:
            groups.append((first, i - 1))
            first, size, add = i, 0, len(line)
        size += add
    if lines:
        groups.append((first, len(lines) - 1))
    return groups
