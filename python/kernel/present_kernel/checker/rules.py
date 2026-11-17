"""Deck checker rules (PROTOCOL.md section 7).

The checker reads the built ``.pptx`` (so it works identically for structured and code-mode
decks) and maps findings back to slide and element ids through the ``present.*`` tags. The deck
document, when given, adds run-level citation checks.
"""

from __future__ import annotations

import io
import json
import re
import tomllib
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from lxml import etree
from PIL import Image
from pptx import Presentation
from present_sdk import tags as T
from present_sdk.content import text_plain
from present_sdk.fit import measure_shape, placeholder_info
from present_sdk.measure import Measurer
from present_sdk.template import FURNITURE_TYPES, _clr_map, describe_presentation, layout_info

from .numbers import check_run, extract_numbers

A = "http://schemas.openxmlformats.org/drawingml/2006/main"
P = "http://schemas.openxmlformats.org/presentationml/2006/main"
NS = {"a": A, "p": P}

DEFAULT_SEVERITY = {
    "tpl.layout-only": "error", "tpl.theme-fonts": "error", "tpl.theme-colors": "warn", "tpl.mandatory": "warn",
    "fit.overflow": "error", "fit.min-size": "warn", "density.bullets": "warn", "density.words": "warn",
    "src.coverage": "warn", "src.numbers": "warn", "src.chart-stale": "warn", "src.dangling": "error",
    "a11y.alt-text": "error", "a11y.contrast": "warn", "a11y.reading-order": "info", "struct.titles": "error",
    "img.resolution": "warn", "code.hardcoded-style": "warn", "out.opens-clean": "error",
}  # fmt: skip
_FACTUAL = re.compile(r"\d|[%€$£¥]")


@dataclass
class Contract:
    severity: dict[str, str] = field(default_factory=lambda: dict(DEFAULT_SEVERITY))
    max_bullets: int = 6
    max_words: int = 70
    min_font_pt: float = 12.0
    min_image_dpi: int = 150
    allow_free_elements: bool = False
    allowed_fonts: list[str] = field(default_factory=list)
    layout_max_bullets: dict[str, int] = field(default_factory=dict)
    mandatory: list[str] = field(default_factory=list)
    closing: str | None = None

    @classmethod
    def load(cls, project: Path, sidecar: dict[str, Any] | None = None) -> Contract:
        c = cls()
        sidecar = sidecar or {}
        rules = sidecar.get("rules", {})
        c.mandatory = list(rules.get("mandatory", []))
        c.closing = rules.get("closing")
        c.allow_free_elements = bool(rules.get("allow_free_elements", False))
        for name, hint in (sidecar.get("layouts") or {}).items():
            if isinstance(hint, dict) and "max_bullets" in hint:
                c.layout_max_bullets[name] = int(hint["max_bullets"])
        cfg = project / ".present" / "project.toml"
        if cfg.is_file():
            contract = tomllib.loads(cfg.read_text(encoding="utf-8")).get("contract", {})
            for key, val in contract.items():
                if key in DEFAULT_SEVERITY and val in ("error", "warn", "info", "off"):
                    c.severity[key] = val
                elif key in ("max_bullets", "max_words", "min_image_dpi"):
                    setattr(c, key, int(val))
                elif key == "min_font_pt":
                    c.min_font_pt = float(val)
                elif key == "allow_free_elements":
                    c.allow_free_elements = bool(val)
                elif key == "allowed_fonts":
                    c.allowed_fonts = [str(v) for v in val]
        return c


class _Out:
    def __init__(self, contract: Contract, only: set[str] | None):
        self.contract = contract
        self.only = only
        self.items: list[dict[str, Any]] = []

    def add(self, rule: str, message: str, slide: _SlideCtx | None = None, element_id: str | None = None,
            fix_hint: str | None = None, pointer: str | None = None) -> None:
        sev = self.contract.severity.get(rule, "warn")
        if sev == "off" or (self.only is not None and rule not in self.only):
            return
        d: dict[str, Any] = {"rule": rule, "severity": sev, "message": message}
        if slide is not None:
            d["slide_id"] = slide.slide_id
            d["slide_index"] = slide.index
        if element_id:
            d["element_id"] = element_id
        if fix_hint:
            d["fix_hint"] = fix_hint
        if pointer:
            d["pointer"] = pointer
        self.items.append(d)


@dataclass
class _SlideCtx:
    index: int
    slide: Any
    slide_id: str
    layout: dict[str, Any] | None
    hidden: bool


# ---- color helpers ---------------------------------------------------------------------------


def _hex_of(el: etree._Element | None, theme: dict[str, str], clrmap: dict[str, str]) -> str | None:
    """Hex of the first color child of ``el`` (a solidFill, a fontRef, a fillRef...)."""
    if el is None:
        return None
    for child in el:
        local = etree.QName(child).localname
        if local == "srgbClr":
            return child.get("val", "000000").upper()
        if local == "schemeClr":
            name = clrmap.get(child.get("val", ""), child.get("val", ""))
            return theme.get(name)
        if local == "sysClr":
            return child.get("lastClr", "000000").upper()
    return None


def _luminance(hex_color: str) -> float:
    def chan(v: int) -> float:
        c = v / 255
        return c / 12.92 if c <= 0.03928 else ((c + 0.055) / 1.055) ** 2.4

    r, g, b = (int(hex_color[i:i + 2], 16) for i in (0, 2, 4))
    return 0.2126 * chan(r) + 0.7152 * chan(g) + 0.0722 * chan(b)


def contrast_ratio(a: str, b: str) -> float:
    la, lb = _luminance(a), _luminance(b)
    hi, lo = max(la, lb), min(la, lb)
    return (hi + 0.05) / (lo + 0.05)


def _background(slide, theme: dict[str, str], clrmap: dict[str, str]) -> str:
    for holder in (slide, slide.slide_layout, slide.slide_layout.slide_master):
        bg = holder._element.find("p:cSld/p:bg", NS)
        if bg is None:
            continue
        hexv = _hex_of(bg.find("p:bgPr/a:solidFill", NS), theme, clrmap) or _hex_of(bg.find("p:bgRef", NS), theme, clrmap)
        if hexv:
            return hexv
    return theme.get("lt1", "FFFFFF")


def _resolve_named(color: str, theme: dict[str, str]) -> str | None:
    if color.startswith("#"):
        return color[1:].upper()
    return theme.get(color)


# ---- shape helpers ---------------------------------------------------------------------------


def _shape_id(slide, el: etree._Element) -> str | None:
    return T.read_shape_tags(slide.part, el).get("present.element")


def _cites(slide, el: etree._Element) -> list[str]:
    tags = T.read_shape_tags(slide.part, el)
    return [v for k, v in sorted(tags.items()) if k.startswith("present.cite.")]


def _shape_text(el: etree._Element) -> str:
    return "\n".join("".join(t.text or "" for t in p.iter(f"{{{A}}}t")) for p in el.iter(f"{{{A}}}p"))


def _ph_type(el: etree._Element) -> str | None:
    ph = el.find(".//p:nvPr/p:ph", NS)
    return None if ph is None else ph.get("type", "obj")


def _is_furniture(el: etree._Element) -> bool:
    return _ph_type(el) in FURNITURE_TYPES


def _descr(el: etree._Element) -> str:
    cnv = el.find(".//p:cNvPr", NS)
    return (cnv.get("descr") or "").strip() if cnv is not None else ""


# ---- the check -------------------------------------------------------------------------------


def check_deck(project: Path, pptx: Path, deck: dict[str, Any] | None = None,
               slide_docs: dict[str, dict[str, Any]] | None = None, sidecar: dict[str, Any] | None = None,
               slides: list[str] | None = None, rules: list[str] | None = None,
               resolve_cite: Callable[[str], str | None] | None = None,
               chart_values: Callable[[dict[str, Any]], dict[str, Any] | None] | None = None,
               opens_clean: Callable[[Path, int], list[str]] | None = None) -> list[dict[str, Any]]:
    """Run the checker. ``resolve_cite`` returns the cited passage text (None when it cannot be
    resolved); ``chart_values`` recomputes a chart's table; ``opens_clean`` returns extra problems
    found by schema validation or the fidelity engine."""
    contract = Contract.load(project, sidecar)
    out = _Out(contract, set(rules) if rules else None)
    try:
        prs = Presentation(str(pptx))
    except Exception as exc:
        out.add("out.opens-clean", f"the file does not open: {type(exc).__name__}: {exc}")
        return out.items
    description = describe_presentation(prs, sidecar=sidecar)
    theme = description["theme"]["colors"]
    theme_fonts = description["theme"]["fonts"]
    measurer = Measurer()
    slide_docs = slide_docs or {}
    ctxs: list[_SlideCtx] = []
    for i, slide in enumerate(prs.slides):
        sid = T.read_slide_tags(slide.part).get("present.slide") or f"slide-{i + 1}"
        try:
            layout = layout_info(description, slide.slide_layout.name)
        except KeyError:
            layout = None
        ctxs.append(_SlideCtx(i, slide, sid, layout, slide._element.get("show") == "0"))
    wanted = set(slides) if slides else None
    titles: dict[str, list[_SlideCtx]] = {}

    for ctx in ctxs:
        selected = wanted is None or ctx.slide_id in wanted
        slide = ctx.slide
        clrmap = _clr_map(slide.slide_layout.slide_master)
        bg = _background(slide, theme, clrmap)
        doc = slide_docs.get(ctx.slide_id)
        title_text = ""
        bullets = 0
        words = 0
        order: list[tuple[int, int, str | None]] = []
        for el in slide.shapes._spTree:
            local = etree.QName(el).localname
            if local not in ("sp", "pic", "graphicFrame", "grpSp", "cxnSp"):
                continue
            eid = _shape_id(slide, el)
            ptype = _ph_type(el)
            text = _shape_text(el) if local in ("sp", "grpSp") else ""
            if ptype in ("title", "ctrTitle"):
                title_text = text.strip()
            if not selected:
                continue
            furniture = _is_furniture(el)
            if local == "sp" and not furniture:
                info = placeholder_info(ctx.layout, el)
                _check_text_shape(out, ctx, el, eid, info, theme, theme_fonts, clrmap, bg, measurer)
                if text.strip() and ptype not in ("title", "ctrTitle"):
                    paras = [p for p in text.split("\n") if p.strip()]
                    if ptype in ("body", "obj", None) and info is not None:
                        bullets += len(paras)
                    words += sum(len(p.split()) for p in paras)
                box = _box_of(el, info)
                if box and text.strip():
                    order.append((box[1], box[0], ptype))
            if local == "grpSp":
                _check_fonts_colors(out, ctx, el, eid, theme_fonts)
                words += len(text.split())
            if local == "pic":
                if not _descr(el):
                    out.add("a11y.alt-text", "picture has no alt text", ctx, eid, "set `alt` on the image")
                _check_resolution(out, ctx, el, eid)
            if local == "graphicFrame":
                is_chart = el.find(f".//{{{A}}}graphicData").get("uri", "").endswith("/chart")
                if is_chart and not _descr(el):
                    out.add("a11y.alt-text", "chart has no alt text", ctx, eid, "set `alt` on the chart")
            if not furniture and local in ("sp", "graphicFrame", "pic") and doc is None:
                _coverage_from_tags(out, ctx, slide, el, eid, local, text, ptype)
        if not ctx.hidden:
            titles.setdefault(title_text, []).append(ctx)
        if not selected:
            continue
        limit = contract.layout_max_bullets.get(slide.slide_layout.name, contract.max_bullets)
        if bullets > limit:
            out.add("density.bullets", f"{bullets} bullets on the slide; the limit is {limit}", ctx,
                    fix_hint="split the slide or cut supporting points")  # fmt: skip
        if words > contract.max_words:
            out.add("density.words", f"{words} words on the slide; the limit is {contract.max_words}", ctx,
                    fix_hint="move detail to the speaker notes")  # fmt: skip
        if order and order[0][2] not in ("title", "ctrTitle") and any(o[2] in ("title", "ctrTitle") for o in order):
            out.add("a11y.reading-order", "the title is not first in the reading order", ctx)
        if doc is not None:
            _check_doc(out, ctx, doc, contract, resolve_cite, chart_values)
        elif resolve_cite is not None:
            _numbers_from_tags(out, ctx, slide, resolve_cite)

    for title, group in titles.items():
        for ctx in group:
            if wanted is not None and ctx.slide_id not in wanted:
                continue
            if not title:
                out.add("struct.titles", "the slide has no title", ctx, fix_hint="give every slide an action title")
            elif len(group) > 1:
                others = ", ".join(str(c.index + 1) for c in group if c is not ctx)
                out.add("struct.titles", f"the title is also used on slide {others}", ctx)

    if wanted is None:
        used = [c.slide.slide_layout.name for c in ctxs]
        for name in contract.mandatory:
            if name not in used:
                out.add("tpl.mandatory", f"the template requires a slide with layout {name!r}", fix_hint=f"add a {name!r} slide")
        visible = [c for c in ctxs if not c.hidden]
        if contract.closing and visible and visible[-1].slide.slide_layout.name != contract.closing:
            out.add("tpl.mandatory", f"the last slide must use layout {contract.closing!r}", visible[-1])
        if deck is not None and deck.get("mode", "structured") == "structured":
            _check_layout_only(out, ctxs, slide_docs, contract)
        if opens_clean is not None:
            for problem in opens_clean(pptx, len([c for c in ctxs if not c.hidden])):
                out.add("out.opens-clean", problem)
    return out.items


def _box_of(el: etree._Element, info: dict[str, Any] | None) -> tuple[int, int, int, int] | None:
    off = el.find("p:spPr/a:xfrm/a:off", NS)
    ext = el.find("p:spPr/a:xfrm/a:ext", NS)
    if off is not None and ext is not None:
        return int(off.get("x")), int(off.get("y")), int(ext.get("cx")), int(ext.get("cy"))
    if info:
        return info["x"], info["y"], info["cx"], info["cy"]
    return None


def _check_fonts_colors(out: _Out, ctx: _SlideCtx, el: etree._Element, eid: str | None, theme_fonts: dict[str, str]) -> None:
    allowed = {f.lower() for f in (theme_fonts.get("major", ""), theme_fonts.get("minor", ""), *out.contract.allowed_fonts)}
    seen: set[str] = set()
    for latin in el.iterfind(".//a:rPr/a:latin", NS):
        face = latin.get("typeface", "")
        if face and not face.startswith("+") and face.lower() not in allowed and face not in seen:
            seen.add(face)
            out.add("tpl.theme-fonts", f"font {face!r} is not a template font", ctx, eid,
                    "remove the font override so the text uses the theme font")  # fmt: skip
    colors: set[str] = set()
    for path in (".//a:rPr/a:solidFill/a:srgbClr", "p:spPr/a:solidFill/a:srgbClr", "p:spPr/a:ln/a:solidFill/a:srgbClr",
                 ".//p:sp/p:spPr/a:solidFill/a:srgbClr"):  # fmt: skip
        for clr in el.iterfind(path, NS):
            colors.add(clr.get("val", "").upper())
    for val in sorted(colors):
        out.add("tpl.theme-colors", f"color #{val} is not a theme color", ctx, eid, "use a theme color such as accent1")


def _check_text_shape(out: _Out, ctx: _SlideCtx, el, eid, info, theme, theme_fonts, clrmap, bg, measurer) -> None:
    _check_fonts_colors(out, ctx, el, eid, theme_fonts)
    contract = out.contract
    got = measure_shape(el, info, theme_fonts, measurer, contract.min_font_pt)
    if got is None:
        return
    tb, m = got
    if not m.fits:
        if tb.autofit == "shrink" and m.shrink_to_pt is not None:
            msg = f"text does not fit even when shrunk to the {contract.min_font_pt:g} pt minimum"
        else:
            over = (m.height - tb.cy) / 12700
            msg = f"text overflows its box by about {over:.0f} pt ({m.lines} lines)"
        out.add("fit.overflow", msg, ctx, eid, "shorten the text, split the slide, or use a layout with more room")
    small = [s for s in tb.explicit_sizes if s < contract.min_font_pt]
    if small:
        out.add("fit.min-size", f"text is set to {min(small):g} pt; the minimum is {contract.min_font_pt:g} pt", ctx, eid)
    # Contrast: explicit run colors, else the level color, against the shape fill or the background.
    fill = _hex_of(el.find("p:spPr/a:solidFill", NS), theme, clrmap)
    style = el.find("p:style", NS)
    default_text = None
    if fill is None and el.find("p:spPr/a:noFill", NS) is None and style is not None:
        fill = _hex_of(style.find("a:fillRef", NS), theme, clrmap)
    if style is not None:
        default_text = _hex_of(style.find("a:fontRef", NS), theme, clrmap)
    against = fill or bg
    levels = info["levels"] if info else []
    reported: set[str] = set()
    for p in el.iterfind("p:txBody/a:p", NS):
        ppr = p.find("a:pPr", NS)
        level = int(ppr.get("lvl", "0")) if ppr is not None else 0
        lvl = levels[min(level, len(levels) - 1)] if levels else None
        for r in p.iterfind("a:r", NS):
            if not (r.findtext("a:t", "", NS) or "").strip():
                continue
            rpr = r.find("a:rPr", NS)
            color = _hex_of(rpr.find("a:solidFill", NS), theme, clrmap) if rpr is not None else None
            if color is None and rpr is not None and rpr.find("a:hlinkClick", NS) is not None:
                color = theme.get("hlink")
            if color is None:
                color = default_text or (_resolve_named(lvl["color"], theme) if lvl else theme.get("dk1"))
            size = int(rpr.get("sz")) / 100 if rpr is not None and rpr.get("sz") else (lvl["size_pt"] if lvl else 18.0)
            if not color or color in reported:
                continue
            ratio = contrast_ratio(color, against)
            need = 3.0 if size >= 18 else 4.5
            if ratio < need:
                reported.add(color)
                out.add("a11y.contrast", f"text color #{color} on #{against} has contrast {ratio:.1f}:1; {need:g}:1 is required",
                        ctx, eid, "use a darker or lighter theme color")  # fmt: skip


def _check_resolution(out: _Out, ctx: _SlideCtx, el: etree._Element, eid: str | None) -> None:
    blip = el.find(".//a:blip", NS)
    ext = el.find("p:spPr/a:xfrm/a:ext", NS)
    if blip is None or ext is None:
        return
    rid = blip.get("{http://schemas.openxmlformats.org/officeDocument/2006/relationships}embed")
    try:
        blob = ctx.slide.part.related_part(rid).blob
        with Image.open(io.BytesIO(blob)) as im:
            px_w, px_h = im.size
    except Exception:
        return
    rect = el.find(".//a:srcRect", NS)
    if rect is not None:
        px_w *= 1 - (int(rect.get("l", "0")) + int(rect.get("r", "0"))) / 100000
        px_h *= 1 - (int(rect.get("t", "0")) + int(rect.get("b", "0"))) / 100000
    inches_w, inches_h = int(ext.get("cx")) / 914400, int(ext.get("cy")) / 914400
    if inches_w <= 0 or inches_h <= 0:
        return
    dpi = min(px_w / inches_w, px_h / inches_h)
    if dpi < out.contract.min_image_dpi:
        out.add("img.resolution", f"image is about {dpi:.0f} dpi at this size; {out.contract.min_image_dpi} dpi is the minimum",
                ctx, eid, "use a larger image or place it smaller")  # fmt: skip


def _coverage_from_tags(out: _Out, ctx: _SlideCtx, slide, el, eid, local, text, ptype) -> None:
    """Shape-level coverage for decks without a document (code mode)."""
    if ptype in ("title", "ctrTitle"):
        return
    cites = _cites(slide, el)
    if local == "graphicFrame":
        if not cites:
            kind = "chart" if el.find(f".//{{{A}}}graphicData").get("uri", "").endswith("/chart") else "table"
            out.add("src.coverage", f"{kind} has no citation", ctx, eid, "cite the reference the data came from")
    elif local == "sp" and not cites and _FACTUAL.search(text or ""):
        out.add("src.coverage", "text with figures has no citation", ctx, eid, "cite the source with cite(...)")


def _numbers_from_tags(out: _Out, ctx: _SlideCtx, slide, resolve_cite) -> None:
    for el in slide.shapes._spTree.iterfind(f"{{{P}}}sp"):
        cites = _cites(slide, el)
        if not cites or _ph_type(el) in FURNITURE_TYPES:
            continue
        _number_verdicts(out, ctx, _shape_id(slide, el), _shape_text(el), cites, resolve_cite)


def _number_verdicts(out: _Out, ctx, eid, text: str, cites: list[str], resolve_cite, pointer: str | None = None) -> None:
    sources = []
    for c in cites:
        passage = resolve_cite(c)
        if passage is None:
            out.add("src.dangling", f"citation {c} does not resolve to a reference", ctx, eid, pointer=pointer)
        else:
            sources.append(passage)
    if not sources:
        return
    for number, verdict in check_run(text, sources):
        if verdict == "red":
            out.add("src.numbers", f"{number.text!r} does not appear in the cited source", ctx, eid,
                    "correct the number or cite the passage that states it", pointer)  # fmt: skip


def _iter_text_items(doc: dict[str, Any]):
    """Yield ``(element_id, pointer, text_model, is_title)`` for every text in a slide document."""
    for key, item in (doc.get("placeholders") or {}).items():
        base = f"/placeholders/{key}"
        kind = item.get("type")
        if kind == "text":
            yield item.get("id"), f"{base}/text", item.get("text"), key == "title"
        elif kind == "table":
            for ri, row in enumerate(item["table"].get("rows", [])):
                for ci, cell in enumerate(row.get("cells", [])):
                    if isinstance(cell.get("text"), dict):
                        yield item.get("id"), f"{base}/table/rows/{ri}/cells/{ci}/text", cell["text"], False
    for i, el in enumerate(doc.get("elements") or []):
        content = el.get("content") or {}
        if content.get("type") in ("text", "shape") and isinstance(content.get("text"), dict):
            yield el.get("id"), f"/elements/{i}/content/text", content["text"], False


def _check_doc(out: _Out, ctx: _SlideCtx, doc: dict[str, Any], contract: Contract, resolve_cite, chart_values) -> None:
    for eid, pointer, text, is_title in _iter_text_items(doc):
        for pi, para in enumerate((text or {}).get("paragraphs", [])):
            runs = para.get("runs", [])
            plain = "".join(r.get("t", "") for r in runs)
            cited = any(r.get("cite") for r in runs)
            declared_none = any(r.get("cite") == [] for r in runs)
            if not is_title and not cited and not declared_none and _FACTUAL.search(plain) and extract_numbers(plain):
                out.add("src.coverage", f"paragraph with figures has no citation: {plain[:60]!r}", ctx, eid,
                        "cite the source, or mark the run non-factual with an empty cite list", f"{pointer}/paragraphs/{pi}")  # fmt: skip
            if resolve_cite is None:
                continue
            para_cites = [c for r in runs for c in (r.get("cite") or [])]
            for ri, run in enumerate(runs):
                if not extract_numbers(run.get("t", "")):
                    continue
                # A number is checked against its own run's cites, else against the paragraph's.
                cites = run.get("cite") or para_cites
                if cites:
                    _number_verdicts(out, ctx, eid, run.get("t", ""), cites, resolve_cite, f"{pointer}/paragraphs/{pi}/runs/{ri}")
    items = list((doc.get("placeholders") or {}).items()) + [
        (f"elements/{i}", el.get("content") or {}) for i, el in enumerate(doc.get("elements") or [])
    ]
    for _key, item in items:
        kind = item.get("type")
        if kind == "chart":
            chart = item["chart"]
            src = chart.get("source") or {}
            cites = [src["cite"]] if src.get("kind") == "ref" and src.get("cite") else list(src.get("cites") or [])
            if not cites:
                out.add("src.coverage", "chart has no citation", ctx, item.get("id"), "cite the reference the data came from")
            if resolve_cite is not None:
                for c in cites:
                    if resolve_cite(c) is None:
                        out.add("src.dangling", f"citation {c} does not resolve to a reference", ctx, item.get("id"))
            if chart_values is not None and src.get("kind") == "ref" and chart.get("computed") is not None:
                fresh = chart_values(chart)
                if fresh is not None and not _tables_equal(fresh, chart["computed"]):
                    out.add("src.chart-stale", "the chart's stored values differ from its source recomputed now", ctx,
                            item.get("id"), "recompute the chart from the current reference version")  # fmt: skip
        elif kind == "table":
            table = item["table"]
            has = bool((table.get("source") or {}).get("cite")) or any(
                r.get("cite") for row in table.get("rows", []) for cell in row.get("cells", [])
                if isinstance(cell.get("text"), dict) for p in cell["text"].get("paragraphs", []) for r in p.get("runs", [])
            )  # fmt: skip
            if not has:
                out.add("src.coverage", "table has no citation", ctx, item.get("id"), "cite the reference the data came from")
        elif kind == "image" and resolve_cite is not None:
            for c in item.get("cite") or []:
                if resolve_cite(c) is None:
                    out.add("src.dangling", f"citation {c} does not resolve to a reference", ctx, item.get("id"))


def _tables_equal(a: dict[str, Any], b: dict[str, Any]) -> bool:
    def norm(t: dict[str, Any]):
        rows = []
        for row in t.get("rows", []):
            rows.append([round(v, 9) if isinstance(v, float) else v for v in row])
        return list(t.get("columns", [])), rows

    return json.dumps(norm(a), sort_keys=True, default=str) == json.dumps(norm(b), sort_keys=True, default=str)


def _check_layout_only(out: _Out, ctxs: list[_SlideCtx], slide_docs: dict[str, dict[str, Any]], contract: Contract) -> None:
    for ctx in ctxs:
        doc = slide_docs.get(ctx.slide_id)
        if doc is None:
            continue
        if ctx.layout is None:
            out.add("tpl.layout-only", f"layout {doc.get('layout')!r} is not in the template", ctx)
        if doc.get("elements") and not contract.allow_free_elements and doc.get("kind") != "generated":
            ids = ", ".join(e.get("id", "?") for e in doc["elements"])
            out.add("tpl.layout-only", f"free elements are not allowed by the template contract ({ids})", ctx,
                    fix_hint="use a layout placeholder or a diagram primitive instead")  # fmt: skip


def doc_text(doc: dict[str, Any]) -> str:
    """All visible text of a slide document (for search and density outside the checker)."""
    return "\n".join(text_plain(t) for _, _, t, _ in _iter_text_items(doc))
