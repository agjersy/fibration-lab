"""PDF extraction with pypdfium2: per-page text, headings, tables, embedded images.

Offsets in ``p<page>:c<start>-<end>`` index the page text as extracted here (pdfium's text with
line breaks normalized to ``\\n``). Tables come from a geometric heuristic: consecutive lines
whose text falls into the same two or more whitespace-separated x-ranges. That finds clean grid
tables; it does not use ruling lines and it stops at multi-line cells.
"""

from __future__ import annotations

import ctypes
import itertools
import threading
from collections import Counter
from pathlib import Path
from typing import Any

from ..chunker import Section, chunk_sections, outline_from_headings
from ..model import Ctx, clean_title, diag, new_doc

PDFIUM_LOCK = threading.RLock()  # pdfium is not thread-safe; the kernel dispatches on worker threads

_HEADING_RATIO = 1.15
_MAX_HEADING_CHARS = 120
_GAP_EM = 0.9  # a horizontal gap wider than this many line heights separates table cells
_MAX_CELL_CHARS = 60


def _norm(text: str) -> str:
    return " ".join(text.split()).casefold()


def _page_lines(raw_text: str) -> tuple[str, list[tuple[int, int, int]]]:
    """Normalized page text and its lines as ``(text_start, raw_start, length)``."""
    parts = raw_text.split("\r\n")
    lines: list[tuple[int, int, int]] = []
    t = r = 0
    for part in parts:
        lines.append((t, r, len(part)))
        t += len(part) + 1
        r += len(part) + 2
    return "\n".join(parts).replace("\r", "\n"), lines


def _line_geometry(tp, raw, start: int, count: int) -> tuple[list[tuple[float, float, float, float]], float]:
    """Merged x-segments ``(left, bottom, right, top)`` of one line, and its font size."""
    n = raw.FPDFText_CountRects(tp, start, count)
    rects: list[tuple[float, float, float, float]] = []
    left, top, right, bottom = ctypes.c_double(), ctypes.c_double(), ctypes.c_double(), ctypes.c_double()
    for i in range(n):
        if raw.FPDFText_GetRect(tp, i, left, top, right, bottom):
            rects.append((left.value, bottom.value, right.value, top.value))
    rects.sort()
    merged: list[tuple[float, float, float, float]] = []
    for rect in rects:
        if merged:
            a = merged[-1]
            height = max(a[3] - a[1], rect[3] - rect[1], 1.0)
            if rect[0] - a[2] <= _GAP_EM * height:
                merged[-1] = (a[0], min(a[1], rect[1]), max(a[2], rect[2]), max(a[3], rect[3]))
                continue
        merged.append(rect)
    return merged, float(raw.FPDFText_GetFontSize(tp, start))


def _columns(rows: list[list[tuple[float, float, float, float]]]) -> list[tuple[float, float]]:
    spans = sorted((seg[0], seg[2]) for row in rows for seg in row)
    cols: list[tuple[float, float]] = []
    for lo, hi in spans:
        if cols and lo <= cols[-1][1]:
            cols[-1] = (cols[-1][0], max(cols[-1][1], hi))
        else:
            cols.append((lo, hi))
    return cols


def _find_tables(textpage, geo: list[list[tuple[float, float, float, float]]]) -> list[dict[str, Any]]:
    """Runs of consecutive multi-segment lines that share column x-ranges."""
    tables: list[dict[str, Any]] = []
    i, n = 0, len(geo)
    while i < n:
        if len(geo[i]) < 2:
            i += 1
            continue
        j = i
        while j < n and len(geo[j]) >= 2:
            j += 1
        run = geo[i:j]
        # Shrink the run until its lines agree on at least two columns.
        while len(run) >= 2:
            cols = _columns(run)
            if len(cols) >= 2:
                break
            run = run[:-1]
        cols = _columns(run) if len(run) >= 2 else []
        enough = len(run) >= 3 or (len(run) == 2 and len(cols) >= 3)
        if len(cols) >= 2 and enough:
            cells: list[list[Any]] = []
            for segs in run:
                row: list[Any] = [None] * len(cols)
                for seg in segs:
                    k = next((c for c, (lo, hi) in enumerate(cols) if seg[0] >= lo - 0.01 and seg[2] <= hi + 0.01), None)
                    if k is None:
                        continue
                    text = textpage.get_text_bounded(left=seg[0] - 1, bottom=seg[1] - 1, right=seg[2] + 1, top=seg[3] + 1)
                    text = " ".join(text.split())
                    row[k] = f"{row[k]} {text}" if row[k] else text
                cells.append(row)
            texts = [c for row in cells for c in row if c]
            filled = len(texts) / (len(cells) * len(cols))
            if texts and filled >= 0.6 and sorted(len(t) for t in texts)[len(texts) // 2] <= _MAX_CELL_CHARS:
                tables.append({"first_line": i, "last_line": i + len(run) - 1,
                               "cells": cells})  # fmt: skip
        i += max(len(run), 1) if len(run) >= 2 else 1
    return tables


def _bookmarks(pdf) -> list[tuple[int, int, str]]:
    out: list[tuple[int, int, str]] = []
    try:
        for item in pdf.get_toc(max_depth=8):
            title = clean_title(item.get_title() or "")
            dest = item.get_dest()
            index = dest.get_index() if dest is not None else None
            if title and index is not None:
                out.append((int(index), int(item.level) + 1, title))
    except Exception:
        return []
    return out


def _ranges(pages: list[int]) -> str:
    parts: list[str] = []
    start = prev = pages[0]
    for p in [*pages[1:], None]:
        if p is not None and p == prev + 1:
            prev = p
            continue
        parts.append(str(start) if start == prev else f"{start}-{prev}")
        if p is not None:
            start = prev = p
    return ", ".join(parts)


def extract(path: Path, ctx: Ctx) -> dict[str, Any]:
    with PDFIUM_LOCK:
        return _extract(path, ctx)


def _extract(path: Path, ctx: Ctx) -> dict[str, Any]:
    import pypdfium2 as pdfium
    import pypdfium2.raw as raw

    pdf = pdfium.PdfDocument(str(path))
    try:
        doc = new_doc("pdf", "")
        n_pages = len(pdf)
        doc["pages"] = n_pages
        doc["root_loc"] = "p1"
        pages: list[dict[str, Any]] = []
        sizes: Counter[float] = Counter()
        for index in range(n_pages):
            page = pdf[index]
            textpage = page.get_textpage()
            try:
                count = textpage.count_chars()
                raw_text = textpage.get_text_range() if count > 0 else ""
                text, lines = _page_lines(raw_text)
                info: dict[str, Any] = {"text": text, "lines": lines, "sizes": [], "tops": [], "tables": []}
                if text.strip() and len(raw_text) == count:
                    geo: list[list[tuple[float, float, float, float]]] = []
                    for _, raw_start, length in lines:
                        if length == 0:
                            geo.append([])
                            info["sizes"].append(0.0)
                            info["tops"].append(None)
                            continue
                        segs, size = _line_geometry(textpage.raw, raw, raw_start, length)
                        geo.append(segs)
                        size = round(size * 2) / 2
                        info["sizes"].append(size)
                        info["tops"].append(max(s[3] for s in segs) if segs else None)
                        sizes[size] += length
                    info["tables"] = _find_tables(textpage, geo)
                pages.append(info)
                _page_images(page, raw, index + 1, ctx, doc)
            finally:
                textpage.close()
                page.close()
        _assemble(pdf, doc, pages, sizes)
        try:
            meta_title = clean_title((pdf.get_metadata_dict() or {}).get("Title", ""))
        except Exception:
            meta_title = ""
        first_heading = doc["outline"][0]["title"] if doc["outline"] and not doc.get("_page_outline") else ""
        first_line = next((ln.strip() for p in pages for ln in p["text"].split("\n") if ln.strip()), "")
        doc["title"] = meta_title if meta_title and meta_title.lower() != "untitled" else clean_title(first_heading or first_line) or ctx.name or "PDF document"
        doc.pop("_page_outline", None)
        return doc
    finally:
        pdf.close()


def _page_images(page, raw, number: int, ctx: Ctx, doc: dict[str, Any]) -> None:
    try:
        objects = list(page.get_objects(filter=(raw.FPDF_PAGEOBJ_IMAGE,), max_depth=3))
    except Exception:
        return
    k = 0
    for obj in objects:
        try:
            try:
                image = obj.get_bitmap(render=False).to_pil()
            except Exception:
                image = obj.get_bitmap(render=True).to_pil()
        except Exception:
            continue
        ref = ctx.save_image(f"img-p{number}-{k + 1}", image, f"p{number}")
        if ref:
            k += 1
            doc["images"].append(ref)


def _assemble(pdf, doc: dict[str, Any], pages: list[dict[str, Any]], sizes: Counter) -> None:
    body = sizes.most_common(1)[0][0] if sizes else 0.0
    bookmarks = _bookmarks(pdf)
    # Headings per page as (offset, level, title, end); from bookmarks when the file has them.
    per_page: list[list[tuple[int, int, str, int]]] = [[] for _ in pages]
    if bookmarks:
        for index, level, title in bookmarks:
            if not 0 <= index < len(pages):
                continue
            info = pages[index]
            offset, end = 0, 0
            want = _norm(title)
            for start, _, length in info["lines"]:
                if length and _norm(info["text"][start:start + length]) == want:
                    offset, end = start, start + length
                    break
            per_page[index].append((offset, level, title, end))
    elif body:
        heading_sizes = sorted({s for p in pages for s in p["sizes"] if s >= body * _HEADING_RATIO}, reverse=True)
        level_of = {s: min(i + 1, 6) for i, s in enumerate(heading_sizes)}
        for index, info in enumerate(pages):
            prev_line = -2
            for li, ((start, _, length), size) in enumerate(zip(info["lines"], info["sizes"])):
                line = info["text"][start:start + length].strip()
                if size not in level_of or not 3 <= len(line) <= _MAX_HEADING_CHARS or not any(c.isalpha() for c in line):
                    continue
                found = per_page[index]
                if found and prev_line == li - 1 and found[-1][1] == level_of[size]:
                    o, lv, t, _ = found[-1]
                    if len(t) + len(line) < _MAX_HEADING_CHARS:
                        found[-1] = (o, lv, f"{t} {line}", start + length)
                        prev_line = li
                        continue
                found.append((start, level_of[size], clean_title(line), start + length))
                prev_line = li
    for found in per_page:
        found.sort(key=lambda h: h[0])

    stack: list[tuple[int, str]] = []
    flat: list[tuple[int, int, str]] = []
    locs: list[str] = []
    no_text: list[int] = []
    for index, info in enumerate(pages):
        number = index + 1
        text = info["text"]
        doc["texts"][f"p{number}"] = text
        if not text.strip():
            no_text.append(number)
        sections: list[Section] = []
        prev, path = 0, [t for _, t in stack]
        for offset, level, title, end in per_page[index]:
            if offset > prev:
                sections.append(Section(prev, offset, path))
            while stack and stack[-1][0] >= level:
                stack.pop()
            stack.append((level, title))
            path, prev = [t for _, t in stack], offset
            flat.append((len(flat), level, title))
            locs.append(f"p{number}:c{offset}-{end}" if end > offset else f"p{number}")
        if len(text) > prev:
            sections.append(Section(prev, len(text), path))
        doc["chunks"] += chunk_sections(text, sections, _paragraph_breaks(info), f"p{number}:")
        for k, table in enumerate(info["tables"]):
            doc["tables"].append({"loc": f"p{number}/t{k + 1}", "cells": table["cells"]})
    if flat:
        doc["outline"] = outline_from_headings(flat, lambda i: locs[i])
    else:
        doc["outline"] = [{"title": f"Page {i + 1}", "loc": f"p{i + 1}", "children": []} for i in range(len(pages))]
        doc["_page_outline"] = True
    if no_text:
        doc["no_text_pages"] = no_text
        doc["warnings"].append(diag(
            "refs.no-text",
            f"{len(no_text)} of {len(pages)} pages have no text layer (page {_ranges(no_text)}); "
            "that content is not searchable. Use refs.page_image to look at them.",
        ))  # fmt: skip


def _paragraph_breaks(info: dict[str, Any]) -> list[int]:
    """Text offsets of lines that start a paragraph: after a vertical gap wider than the usual pitch."""
    tops, lines = info["tops"], info["lines"]
    if not tops:
        return []
    gaps = [a - b for a, b in itertools.pairwise(tops) if a is not None and b is not None and a > b]
    if not gaps:
        return []
    pitch = sorted(gaps)[len(gaps) // 2]
    breaks: list[int] = []
    for i in range(1, len(lines)):
        a, b = tops[i - 1], tops[i]
        if a is None or b is None or b > a or a - b > pitch * 1.25:
            breaks.append(lines[i][0])
    return breaks


def render_page(path: Path, page: int, dpi: int, out: Path) -> None:
    """Render one page (1-based) to a PNG file."""
    import pypdfium2 as pdfium

    with PDFIUM_LOCK:
        pdf = pdfium.PdfDocument(str(path))
        try:
            if not 1 <= page <= len(pdf):
                from ...errors import not_found

                raise not_found(f"page {page} is out of range (1 to {len(pdf)})")
            pg = pdf[page - 1]
            try:
                image = pg.render(scale=dpi / 72).to_pil()
                out.parent.mkdir(parents=True, exist_ok=True)
                image.save(out, format="PNG")
            finally:
                pg.close()
        finally:
            pdf.close()
