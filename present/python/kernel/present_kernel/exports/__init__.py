"""Exports derived from the deck document: Markdown outline and speaker-notes ``.docx``."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from present_sdk.content import text_cites, text_plain

from ..render import load_deck


def _slide_parts(doc: dict[str, Any]) -> tuple[str, list[tuple[int, str]], list[str], list[str]]:
    """Title, body paragraphs as ``(level, text)``, non-text element notes, and citations."""
    title = ""
    body: list[tuple[int, str]] = []
    extras: list[str] = []
    cites: list[str] = []

    def add_cites(found: list[str]) -> None:
        cites.extend(c for c in found if c not in cites)

    for key, item in (doc.get("placeholders") or {}).items():
        kind = item.get("type")
        if kind == "text":
            add_cites(text_cites(item.get("text")))
            if key == "title":
                title = text_plain(item.get("text")).replace("\n", " ")
            else:
                for para in item["text"].get("paragraphs", []):
                    t = "".join(r.get("t", "") for r in para.get("runs", []))
                    if t.strip():
                        body.append((int(para.get("level") or 0), t))
        elif kind == "chart":
            chart = item["chart"]
            extras.append(f"Chart ({chart.get('type', 'chart')}): {chart.get('title') or item.get('alt') or 'untitled'}")
            src = chart.get("source") or {}
            add_cites([src["cite"]] if src.get("cite") else list(src.get("cites") or []))
        elif kind == "table":
            extras.append(f"Table: {len(item['table'].get('rows', []))} rows")
            for row in item["table"].get("rows", []):
                for cell in row.get("cells", []):
                    if isinstance(cell.get("text"), dict):
                        add_cites(text_cites(cell["text"]))
        elif kind == "image":
            extras.append(f"Image: {item.get('alt') or item.get('asset')}")
            add_cites(item.get("cite") or [])
        elif kind == "diagram":
            extras.append(f"Diagram: {item['diagram'].get('family')} ({item['diagram'].get('variant')})")
        elif kind == "equation":
            cite = item.get("eq_cite") or {}
            extras.append(f"Equation: ${cite.get('latex', '')}$ ({cite.get('qualname')} in {cite.get('path')})")
    return title, body, extras, cites


def _ordered(deck: dict[str, Any], slides: dict[str, dict[str, Any]]):
    n = 0
    for section in deck.get("sections", []):
        for sid in section.get("slides", []):
            doc = slides[sid]
            if doc.get("hidden"):
                continue
            n += 1
            yield n, section.get("title", ""), doc


def outline_markdown(project: Path, deck_rel: str, include_sources: bool) -> str:
    deck, slides = load_deck(project, deck_rel)
    lines = [f"# {deck.get('title') or 'Untitled deck'}", ""]
    all_cites: list[str] = []
    current = None
    for n, section, doc in _ordered(deck, slides):
        if section and section != current:
            lines += [f"## {section}", ""]
            current = section
        title, body, extras, cites = _slide_parts(doc)
        lines.append(f"### {n}. {title or '(no title)'}")
        lines.append("")
        for level, t in body:
            lines.append(f"{'  ' * level}- {t}")
        for extra in extras:
            lines.append(f"- *{extra}*")
        notes = text_plain(doc.get("notes"))
        if notes.strip():
            lines += ["", "> " + notes.strip().replace("\n", "\n> ")]
        if include_sources and cites:
            marks = []
            for c in cites:
                if c not in all_cites:
                    all_cites.append(c)
                marks.append(f"[{all_cites.index(c) + 1}]")
            lines += ["", "Sources: " + " ".join(marks)]
        lines.append("")
    if include_sources and all_cites:
        lines += ["## Sources", ""]
        lines += [f"{i}. `{c}`" for i, c in enumerate(all_cites, 1)]
        lines.append("")
    return "\n".join(lines)


def notes_docx(project: Path, deck_rel: str, out: Path, include_sources: bool) -> None:
    from docx import Document

    deck, slides = load_deck(project, deck_rel)
    document = Document()
    document.add_heading(deck.get("title") or "Speaker notes", level=0)
    for n, _, doc in _ordered(deck, slides):
        title, _, _, cites = _slide_parts(doc)
        document.add_heading(f"{n}. {title or '(no title)'}", level=1)
        notes = text_plain(doc.get("notes"))
        for para in notes.split("\n") if notes.strip() else ["(no notes)"]:
            document.add_paragraph(para)
        if include_sources and cites:
            document.add_paragraph("Sources: " + "; ".join(cites)).italic = True
    out.parent.mkdir(parents=True, exist_ok=True)
    document.save(str(out))
