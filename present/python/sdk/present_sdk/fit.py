"""Text fit for shapes in a built slide: shared by the SDK (to write autofit scales) and the checker.

PowerPoint only recomputes ``normAutofit`` when text is edited, so a generated file must carry
the font scale itself or shrink-on-overflow boxes simply overflow until someone types in them.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from lxml import etree

from .measure import Measure, Measurer, Para

A = "http://schemas.openxmlformats.org/drawingml/2006/main"
P = "http://schemas.openxmlformats.org/presentationml/2006/main"
NS = {"a": A, "p": P}

_DEFAULT_LEVEL = {
    "font": "", "size_pt": 18.0, "bold": False, "color": "dk1", "bullet": False,
    "space_before_pt": 0.0, "line_spacing": 1.0, "mar_l": 0,
}  # fmt: skip
_DEFAULT_INSETS = {"l": 91440, "t": 45720, "r": 91440, "b": 45720}


@dataclass
class TextBox:
    paras: list[Para]
    cx: int
    cy: int
    insets: dict[str, int]
    autofit: str
    explicit_sizes: list[float]
    wraps: bool = True
    vertical: bool = False


def ph_of(sp: etree._Element) -> etree._Element | None:
    return sp.find("p:nvSpPr/p:nvPr/p:ph", NS)


def placeholder_info(layout: dict[str, Any] | None, sp: etree._Element) -> dict[str, Any] | None:
    ph = ph_of(sp)
    if ph is None or layout is None:
        return None
    idx = int(ph.get("idx", "0"))
    for info in layout["placeholders"]:
        if info["idx"] == idx:
            return info
    return None


def _xfrm(sp: etree._Element) -> tuple[int, int, int, int] | None:
    off = sp.find("p:spPr/a:xfrm/a:off", NS)
    ext = sp.find("p:spPr/a:xfrm/a:ext", NS)
    if off is None or ext is None:
        return None
    return int(off.get("x")), int(off.get("y")), int(ext.get("cx")), int(ext.get("cy"))


def shape_box(sp: etree._Element, info: dict[str, Any] | None) -> tuple[int, int, int, int] | None:
    own = _xfrm(sp)
    if own is not None:
        return own
    if info is not None:
        return info["x"], info["y"], info["cx"], info["cy"]
    return None


def text_box(sp: etree._Element, info: dict[str, Any] | None, theme_fonts: dict[str, str]) -> TextBox | None:
    """Describe a shape's text for measurement, or None when it has no text."""
    body = sp.find("p:txBody", NS)
    box = shape_box(sp, info)
    if body is None or box is None:
        return None
    body_pr = body.find("a:bodyPr", NS)
    autofit = info["autofit"] if info else "none"
    insets = dict(info["insets"]) if info else dict(_DEFAULT_INSETS)
    wraps, vertical = True, False
    if body_pr is not None:
        if body_pr.find("a:normAutofit", NS) is not None:
            autofit = "shrink"
        elif body_pr.find("a:spAutoFit", NS) is not None:
            autofit = "resize"
        elif body_pr.find("a:noAutofit", NS) is not None:
            autofit = "none"
        for key, attr in (("l", "lIns"), ("t", "tIns"), ("r", "rIns"), ("b", "bIns")):
            if body_pr.get(attr) is not None:
                insets[key] = int(body_pr.get(attr))
        wraps = body_pr.get("wrap", "square") != "none"
        vertical = body_pr.get("vert", "horz") not in ("horz", "")
    levels = info["levels"] if info else []
    minor = theme_fonts.get("minor", "")
    paras: list[Para] = []
    explicit: list[float] = []
    for p in body.iterfind("a:p", NS):
        ppr = p.find("a:pPr", NS)
        level = int(ppr.get("lvl", "0")) if ppr is not None else 0
        style = dict(levels[min(level, len(levels) - 1)]) if levels else dict(_DEFAULT_LEVEL, font=minor)
        parts: list[str] = []
        sizes: list[float] = []
        bold = style["bold"]
        italic = False
        font = style["font"] or minor
        for child in p:
            local = etree.QName(child).localname
            if local == "br":
                parts.append("\n")
            elif local in ("r", "fld"):
                t = child.find("a:t", NS)
                parts.append(t.text or "" if t is not None else "")
                rpr = child.find("a:rPr", NS)
                if rpr is not None:
                    if rpr.get("sz"):
                        sizes.append(int(rpr.get("sz")) / 100)
                    if rpr.get("b") in ("1", "true"):
                        bold = True
                    if rpr.get("i") in ("1", "true"):
                        italic = True
                    latin = rpr.find("a:latin", NS)
                    if latin is not None and latin.get("typeface"):
                        face = latin.get("typeface")
                        font = theme_fonts.get("major" if face.startswith("+mj") else "minor", font) if face.startswith("+") else face
        text = "".join(parts)
        if not text.strip() and not paras:
            # Leading empty paragraphs still take a line; keep them only once real text follows.
            pass
        size = max(sizes) if sizes else style["size_pt"]
        explicit += sizes
        mar_l = style.get("mar_l", 0)
        if ppr is not None and ppr.get("marL") is not None:
            mar_l = int(ppr.get("marL"))
        paras.append(Para(text, font, size, bold, italic, style["space_before_pt"], style["line_spacing"], mar_l))
    if not any(p.text.strip() for p in paras):
        return None
    return TextBox(paras, box[2], box[3], insets, autofit, explicit, wraps, vertical)


def measure_shape(sp: etree._Element, info: dict[str, Any] | None, theme_fonts: dict[str, str],
                  measurer: Measurer, min_font_pt: float = 0.0) -> tuple[TextBox, Measure] | None:
    tb = text_box(sp, info, theme_fonts)
    if tb is None or tb.vertical or not tb.wraps:
        return None
    return tb, measurer.measure(tb.paras, tb.cx, tb.cy, tb.insets, tb.autofit, min_font_pt)


def write_autofit(sp: etree._Element, m: Measure) -> None:
    """Record the computed shrink in ``a:normAutofit`` so every renderer shows the fitted text."""
    body = sp.find("p:txBody", NS)
    if body is None:
        return
    body_pr = body.find("a:bodyPr", NS)
    if body_pr is None:
        body_pr = etree.Element(f"{{{A}}}bodyPr")
        body.insert(0, body_pr)
    for old in list(body_pr):
        if etree.QName(old).localname in ("normAutofit", "spAutoFit", "noAutofit"):
            body_pr.remove(old)
    el = etree.Element(f"{{{A}}}normAutofit")
    if m.scale < 1.0:
        el.set("fontScale", str(round(m.scale * 100000)))
        reduction = round((1.0 - max(0.8, 1.0 - (1.0 - m.scale) * 0.8)) * 100000)
        if reduction:
            el.set("lnSpcReduction", str(min(reduction, 20000)))
    # Schema order inside bodyPr: prstTxWarp?, autofit, scene3d?, sp3d?, flatTx?, extLst?
    warp = body_pr.find("a:prstTxWarp", NS)
    if warp is not None:
        warp.addnext(el)
    else:
        body_pr.insert(0, el)
