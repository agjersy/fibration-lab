"""DOCX with python-docx: paragraphs with heading paths from Heading styles, tables, inline images.

The loc is ``c<start>-<end>`` over the extracted plain text: body blocks in document order joined
by blank lines, tables as tab-separated rows.
"""

from __future__ import annotations

import re
from pathlib import Path
from typing import Any

from ..model import Ctx, clean_title
from .common import TextBuilder, built_doc

_HEADING_NAME = re.compile(r"^(?:heading|titre|überschrift|ueberschrift|título|titolo|kop|rubrik|overskrift)\s*(\d)$", re.I)
_HEADING_ID = re.compile(r"^(?:heading|berschrift|titre|ttulo|titolo|kop|rubrik|overskrift)(\d)$", re.I)


def _heading_level(paragraph) -> int | None:
    """1 to 9 for a heading paragraph, 0 for the document title style, None for body text."""
    try:
        style = paragraph.style
    except Exception:
        style = None
    seen = 0
    while style is not None and seen < 8:
        name, style_id = (style.name or ""), (style.style_id or "")
        if name.lower() == "title" or style_id.lower() == "title":
            return 0
        m = _HEADING_NAME.match(name) or _HEADING_ID.match(style_id)
        if m:
            return int(m.group(1))
        ppr = style.element.pPr
        if ppr is not None:
            lvl = ppr.find("{http://schemas.openxmlformats.org/wordprocessingml/2006/main}outlineLvl")
            if lvl is not None:
                value = lvl.get("{http://schemas.openxmlformats.org/wordprocessingml/2006/main}val", "9")
                if value.isdigit() and int(value) < 9:
                    return int(value) + 1
        style, seen = style.base_style, seen + 1
    return None


def _table_cells(table) -> list[list[str]]:
    rows: list[list[str]] = []
    for row in table.rows:
        cells: list[str] = []
        previous = None
        for cell in row.cells:
            if cell._tc is previous:  # horizontally merged cells repeat
                cells.append("")
            else:
                cells.append(" ".join(cell.text.split()))
            previous = cell._tc
        rows.append(cells)
    return rows


def extract(path: Path, ctx: Ctx) -> dict[str, Any]:
    from docx import Document
    from docx.table import Table
    from docx.text.paragraph import Paragraph

    document = Document(str(path))
    builder = TextBuilder()
    title_style = ""
    pending_images: list[tuple[str, bytes]] = []
    images: list[tuple[str, bytes]] = []
    blip = "{http://schemas.openxmlformats.org/drawingml/2006/main}blip"
    embed = "{http://schemas.openxmlformats.org/officeDocument/2006/relationships}embed"
    for element in document.element.body.iterchildren():
        tag = element.tag.rsplit("}", 1)[-1]
        if tag == "p":
            paragraph = Paragraph(element, document)
            text = paragraph.text.replace("\x0b", "\n")
            for node in element.iter(blip):
                part = document.part.related_parts.get(node.get(embed))
                if part is not None and hasattr(part, "blob"):
                    pending_images.append(("", part.blob))
            level = _heading_level(paragraph) if text.strip() else None
            span = None
            if level == 0:
                title_style = title_style or clean_title(text)
                builder.heading(1, text)
                span = (builder.headings[-1][0], builder.headings[-1][3])
            elif level:
                # The Title style takes level 1 when present, so real headings nest below it.
                builder.heading(level + (1 if title_style else 0), text)
                span = (builder.headings[-1][0], builder.headings[-1][3])
            else:
                span = builder.paragraph(text)
            if pending_images and span is not None:
                images += [(f"c{span[0]}-{span[1]}", blob) for _, blob in pending_images]
                pending_images = []
        elif tag == "tbl":
            builder.table(_table_cells(Table(element, document)))
    if pending_images:
        loc = f"c{max(builder.length - 1, 0)}-{builder.length}" if builder.length else "c0-0"
        images += [(loc, blob) for _, blob in pending_images]
    try:
        core_title = clean_title(document.core_properties.title or "")
    except Exception:
        core_title = ""
    title = core_title or title_style or (builder.headings[0][2] if builder.headings else "") or ctx.name or "Document"
    doc = built_doc("docx", title, builder)
    for k, (loc, blob) in enumerate(images):
        ref = ctx.save_image(f"img-{k + 1}", blob, loc)
        if ref:
            doc["images"].append(ref)
    return doc
