"""Drawing surface shared by the diagram families: one group, theme styling, measured text.

Every diagram is drawn into a single ``p:grpSp``. Child coordinates are slide coordinates (the
group's child space equals its own extents), text is sized with the same ``Measurer`` the deck
checker uses, and colors are theme color names only.
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Any

from lxml import etree
from pptx.enum.shapes import MSO_CONNECTOR, MSO_SHAPE
from pptx.enum.text import MSO_ANCHOR, MSO_AUTO_SIZE, PP_ALIGN
from pptx.util import Emu, Pt

from ..content import A, apply_text, as_text, set_color
from ..measure import EMU_PER_PT, Measurer, Para, _tokens

PAD = 45720  # default text inset: 0.05 in
DEFAULT_MIN_PT = 10.0
MAX_LABEL_PT = 24.0
ACCENTS = ("accent1", "accent2", "accent3", "accent4", "accent5", "accent6")
TOP, LEFT, BOTTOM, RIGHT = 0, 1, 2, 3  # connection site indices of rectangular presets
_ALIGN = {"left": PP_ALIGN.LEFT, "center": PP_ALIGN.CENTER, "right": PP_ALIGN.RIGHT}
_ANCHOR = {"top": MSO_ANCHOR.TOP, "middle": MSO_ANCHOR.MIDDLE, "bottom": MSO_ANCHOR.BOTTOM}
_PRST = {
    "rect": MSO_SHAPE.RECTANGLE, "roundRect": MSO_SHAPE.ROUNDED_RECTANGLE, "ellipse": MSO_SHAPE.OVAL,
    "diamond": MSO_SHAPE.DIAMOND, "chevron": MSO_SHAPE.CHEVRON, "homePlate": MSO_SHAPE.PENTAGON,
    "flowChartTerminator": MSO_SHAPE.FLOWCHART_TERMINATOR, "pie": MSO_SHAPE.PIE,
}  # fmt: skip
_DEFAULT_ADJ = {"roundRect": 16667, "chevron": 50000, "homePlate": 50000}


@dataclass
class Label:
    """Text for one shape: Text-model paragraphs with a bold flag and a relative size each."""

    paras: list[dict[str, Any]] = field(default_factory=list)
    bold: list[bool] = field(default_factory=list)
    scale: list[float] = field(default_factory=list)

    def add(self, value: Any, bold: bool = False, scale: float = 1.0) -> Label:
        if value is None or value == "":
            return self
        for para in as_text(value).get("paragraphs", []):
            self.paras.append(para)
            self.bold.append(bold)
            self.scale.append(scale)
        return self

    @property
    def empty(self) -> bool:
        return not any(r.get("t") for p in self.paras for r in p.get("runs", []))

    def texts(self) -> list[str]:
        return ["".join(r.get("t", "") for r in p.get("runs", [])) for p in self.paras]


def label(value: Any, bold: bool = False, scale: float = 1.0) -> Label:
    return Label().add(value, bold, scale)


def text_area(prst: str, w: int, h: int, adj: int | None = None) -> tuple[int, int, int, int]:
    """The preset geometry's text rectangle as ``(left, top, width, height)`` inside a ``w`` x ``h`` shape."""
    ss = min(w, h)
    if adj is None:
        adj = _DEFAULT_ADJ.get(prst, 0)
    if prst == "diamond":
        return w // 4, h // 4, w // 2, h // 2
    if prst == "ellipse":
        ix, iy = int(w * 0.14645), int(h * 0.14645)
        return ix, iy, w - 2 * ix, h - 2 * iy
    if prst == "roundRect":
        i = int(ss * min(adj, 50000) / 100000 * 0.29289)
        return i, i, w - 2 * i, h - 2 * i
    if prst == "chevron":
        x1 = int(ss * min(adj, 100000 * w // max(ss, 1)) / 100000)
        return (x1, 0, w - 2 * x1, h) if w - 2 * x1 > 0 else (0, 0, w, h)
    if prst == "homePlate":
        dx = int(ss * min(adj, 100000 * w // max(ss, 1)) / 100000)
        return 0, 0, w - dx // 2, h
    if prst == "flowChartTerminator":
        ix, iy = int(w * 1018 / 21600), int(h * 3163 / 21600)
        return ix, iy, w - 2 * ix, h - 2 * iy
    return 0, 0, w, h


class Canvas:
    def __init__(self, slide: Any, box: tuple[int, int, int, int], min_pt: float = DEFAULT_MIN_PT) -> None:
        self.x, self.y, self.cx, self.cy = (int(v) for v in box)
        self.group = slide.raw.shapes.add_group_shape()
        self.shapes = self.group.shapes
        deck = getattr(slide, "deck", None)
        theme = (getattr(deck, "description", None) or {}).get("theme", {})
        self.font = (theme.get("fonts") or {}).get("minor") or "Calibri"
        body_pt = 18.0
        for ph in (getattr(slide, "layout", None) or {}).get("placeholders", []):
            if ph.get("key") == "body" and ph.get("levels"):
                body_pt = float(ph["levels"][0].get("size_pt") or 18.0)
                break
        self.min_pt = float(min_pt)
        self.max_pt = max(self.min_pt, min(body_pt, MAX_LABEL_PT))
        self.measurer = Measurer()
        self.overflow: list[str] = []
        self._adv: dict[tuple[str, float, bool], float] = {}

    # ---- measurement ------------------------------------------------------------------------
    def _paras(self, lab: Label, size: float) -> list[Para]:
        return [Para(t, self.font, size * s, b) for t, b, s in zip(lab.texts(), lab.bold, lab.scale)]

    def _advance(self, text: str, size: float, bold: bool) -> float:
        key = (text, size, bold)
        if key not in self._adv:
            self._adv[key] = self.measurer.advance_pt(text, self.font, size, bold)
        return self._adv[key]

    def fits(self, lab: Label, w: int, h: int, size: float, pad: int = PAD) -> bool:
        """True when the label fits a ``w`` x ``h`` text area: wrapped height and every unbreakable word."""
        if lab.empty:
            return True
        if w <= 2 * pad or h <= 2 * pad:
            return False
        insets = {"l": pad, "t": pad, "r": pad, "b": pad}
        avail = (w - 2 * pad) / EMU_PER_PT
        for t, b, s in zip(lab.texts(), lab.bold, lab.scale):
            for seg in t.split("\n"):
                for tok in _tokens(seg):
                    if self._advance(tok.rstrip(" "), size * s, b) > avail:
                        return False
        return self.measurer.measure(self._paras(lab, size), w, h, insets).fits

    def best_size(self, items: list[tuple[str, Label, int, int]], max_pt: float | None = None,
                  pad: int = PAD) -> tuple[float, list[str]]:
        """Largest common size (0.5 pt grid) at which every label fits; ids that fail at the minimum."""
        hi = round((max_pt or self.max_pt) * 2)
        lo = round(self.min_pt * 2)
        hi = max(hi, lo)

        def ok(half: int) -> bool:
            return all(self.fits(lab, w, h, half / 2, pad) for _, lab, w, h in items)

        if ok(hi):
            return hi / 2, []
        if not ok(lo):
            return lo / 2, [i for i, lab, w, h in items if not self.fits(lab, w, h, lo / 2, pad)]
        while hi - lo > 1:
            mid = (hi + lo) // 2
            if ok(mid):
                lo = mid
            else:
                hi = mid
        return lo / 2, []

    def fit(self, items: list[tuple[str, Label, int, int]], max_pt: float | None = None, pad: int = PAD) -> float:
        """``best_size`` that also records failing ids in ``overflow``."""
        size, failed = self.best_size(items, max_pt, pad)
        for i in failed:
            if i not in self.overflow:
                self.overflow.append(i)
        return size

    def height_of(self, lab: Label, w: int, size: float, pad: int = PAD) -> int:
        insets = {"l": pad, "t": pad, "r": pad, "b": pad}
        return self.measurer.measure(self._paras(lab, size), w, 10**9, insets).height + EMU_PER_PT

    def width_of(self, lab: Label, size: float, pad: int = PAD) -> int:
        """Width of the longest paragraph set on one line, including insets."""
        widest = max((self._advance(t, size * s, b) for t, b, s in zip(lab.texts(), lab.bold, lab.scale)), default=0.0)
        return math.ceil(widest * EMU_PER_PT) + 2 * pad + EMU_PER_PT

    def word_width(self, lab: Label, size: float, pad: int = PAD) -> int:
        """Width of the longest unbreakable word, including insets."""
        widest = 0.0
        for t, b, s in zip(lab.texts(), lab.bold, lab.scale):
            for seg in t.split("\n"):
                for tok in _tokens(seg):
                    widest = max(widest, self._advance(tok.rstrip(" "), size * s, b))
        return math.ceil(widest * EMU_PER_PT) + 2 * pad + EMU_PER_PT

    # ---- shapes -----------------------------------------------------------------------------
    def shape(self, prst: str, x: float, y: float, w: float, h: float, name: str, fill: str | None = None,
              line: str | None = None, line_pt: float = 1.0, adj: dict[str, int] | None = None):
        shp = self.shapes.add_shape(_PRST[prst], Emu(int(x)), Emu(int(y)), Emu(max(int(w), 1)), Emu(max(int(h), 1)))
        shp.name = name
        shp.shadow.inherit = False
        _no_effects(shp)
        if fill is None:
            shp.fill.background()
        else:
            shp.fill.solid()
            set_color(shp.fill.fore_color, fill)
        if line is None:
            shp.line.fill.background()
        else:
            set_color(shp.line.color, line)
            shp.line.width = Pt(line_pt)
        if adj:
            set_adjust(shp, adj)
        return shp

    def text(self, shp, lab: Label, size: float, color: str = "dk1", align: str = "center",
             anchor: str = "middle", pad: int | tuple[int, int, int, int] = PAD, vert: str | None = None) -> None:
        """Put ``lab`` into the shape at ``size`` pt. ``pad`` is one inset or (left, top, right, bottom)."""
        tf = shp.text_frame
        tf.word_wrap = True
        tf.auto_size = MSO_AUTO_SIZE.NONE
        left, top, right, bottom = (pad, pad, pad, pad) if isinstance(pad, int) else pad
        tf.margin_left, tf.margin_top, tf.margin_right, tf.margin_bottom = Emu(left), Emu(top), Emu(right), Emu(bottom)
        tf.vertical_anchor = _ANCHOR[anchor]
        if vert:
            tf._txBody.find(f"{{{A}}}bodyPr").set("vert", vert)
        apply_text(tf, {"paragraphs": lab.paras})
        for p, model, bold, scale in zip(tf.paragraphs, lab.paras, lab.bold, lab.scale):
            if model.get("align", "auto") not in _ALIGN:
                p.alignment = _ALIGN[align]
            for r, run in zip(p.runs, model.get("runs", [])):
                r.font.size = Pt(round(size * scale * 2) / 2)
                if bold:
                    r.font.bold = True
                if not run.get("color") and not run.get("link"):
                    set_color(r.font.color, color)

    def textbox(self, x: float, y: float, w: float, h: float, name: str, lab: Label, size: float,
                color: str = "dk1", align: str = "center", anchor: str = "middle",
                pad: int | tuple[int, int, int, int] = PAD, fill: str | None = None, vert: str | None = None):
        shp = self.shapes.add_textbox(Emu(int(x)), Emu(int(y)), Emu(max(int(w), 1)), Emu(max(int(h), 1)))
        shp.name = name
        if fill:
            shp.fill.solid()
            set_color(shp.fill.fore_color, fill)
        self.text(shp, lab, size, color, align, anchor, pad, vert)
        return shp

    # ---- lines and connectors ---------------------------------------------------------------
    def _style_line(self, cxn, color: str, width_pt: float, arrow: bool, dash: bool = False) -> None:
        set_color(cxn.line.color, color)
        cxn.line.width = Pt(width_pt)
        ln = cxn.line._get_or_add_ln()
        if dash:
            etree.SubElement(ln, f"{{{A}}}prstDash").set("val", "dash")
        if arrow:
            etree.SubElement(ln, f"{{{A}}}tailEnd").set("type", "triangle")

    def line(self, x1: float, y1: float, x2: float, y2: float, name: str, color: str = "dk2",
             width_pt: float = 1.0, arrow: bool = False, dash: bool = False):
        cxn = self.shapes.add_connector(MSO_CONNECTOR.STRAIGHT, Emu(int(x1)), Emu(int(y1)), Emu(int(x2)), Emu(int(y2)))
        cxn.name = name
        _no_effects(cxn)
        self._style_line(cxn, color, width_pt, arrow, dash)
        return cxn

    def connect(self, a, site_a: int, b, site_b: int, name: str, mid: float | None = None,
                color: str = "dk2", width_pt: float = 1.0, arrow: bool = True, dash: bool = False):
        """Connector from ``a``'s site to ``b``'s site, glued to both shapes.

        Sites on opposite or equal sides give a three-segment elbow whose middle segment sits at
        ``mid`` (an x for left/right sites, a y for top/bottom sites; default halfway); a
        top/bottom site paired with a left/right site gives a single-bend elbow; aligned
        endpoints give a straight connector.
        """
        bx, by = site_xy(a, site_a)
        ex, ey = site_xy(b, site_b)
        horiz_a, horiz_b = site_a in (LEFT, RIGHT), site_b in (LEFT, RIGHT)
        if horiz_a != horiz_b:
            kind = "bent2"
        elif site_a != site_b and ((horiz_a and by == ey) or (not horiz_a and bx == ex)):
            kind = "straight"
        else:
            kind = "bent3"
        ctype = MSO_CONNECTOR.STRAIGHT if kind == "straight" else MSO_CONNECTOR.ELBOW
        cxn = self.shapes.add_connector(ctype, Emu(bx), Emu(by), Emu(ex), Emu(ey))
        cxn.name = name
        _no_effects(cxn)
        cxn.begin_connect(a, site_a)
        cxn.end_connect(b, site_b)
        if kind == "bent3":
            _bent3(cxn, bx, by, ex, ey, horiz_a, mid)
        elif kind == "bent2":
            _bent2(cxn, bx, by, ex, ey, horiz_a)
        self._style_line(cxn, color, width_pt, arrow, dash)
        return cxn

    def finish(self):
        """Pin the group's frame to the diagram box (child space equals the frame, so no scaling)."""
        pr = self.group._element.grpSpPr
        xfrm = pr.get_or_add_xfrm()
        for tag, a, b in (("off", "x", "y"), ("ext", "cx", "cy"), ("chOff", "x", "y"), ("chExt", "cx", "cy")):
            el = xfrm.find(f"{{{A}}}{tag}")
            if el is None:
                el = etree.SubElement(xfrm, f"{{{A}}}{tag}")
            vals = (self.x, self.y) if a == "x" else (self.cx, self.cy)
            el.set(a, str(vals[0]))
            el.set(b, str(vals[1]))
        return self.group


def _no_effects(shp) -> None:
    """Point the shape style's effect reference at "none" so the theme adds no shadow."""
    for ref in shp._element.iter(f"{{{A}}}effectRef"):
        ref.set("idx", "0")


def site_xy(shp, site: int) -> tuple[int, int]:
    x, y, w, h = int(shp.left), int(shp.top), int(shp.width), int(shp.height)
    return {TOP: (x + w // 2, y), LEFT: (x, y + h // 2), BOTTOM: (x + w // 2, y + h), RIGHT: (x + w, y + h // 2)}[site]


def set_adjust(shp, adj: dict[str, int]) -> None:
    geom = shp._element.spPr.find(f"{{{A}}}prstGeom")
    av = geom.find(f"{{{A}}}avLst")
    if av is None:
        av = etree.SubElement(geom, f"{{{A}}}avLst")
    for old in list(av):
        av.remove(old)
    for name in sorted(adj):
        gd = etree.SubElement(av, f"{{{A}}}gd")
        gd.set("name", name)
        gd.set("fmla", f"val {int(adj[name])}")


def _set_xfrm(cxn, x: float, y: float, w: float, h: float, rot: int = 0, flip_h: bool = False,
              flip_v: bool = False) -> None:
    xfrm = cxn._element.spPr.get_or_add_xfrm()
    for attr, val in (("rot", str(rot) if rot else None), ("flipH", "1" if flip_h else None),
                      ("flipV", "1" if flip_v else None)):  # fmt: skip
        if val is None:
            xfrm.attrib.pop(attr, None)
        else:
            xfrm.set(attr, val)
    off, ext = xfrm.find(f"{{{A}}}off"), xfrm.find(f"{{{A}}}ext")
    off.set("x", str(round(x)))
    off.set("y", str(round(y)))
    ext.set("cx", str(max(round(w), 0)))
    ext.set("cy", str(max(round(h), 0)))


_MIN_SPAN = 12700  # PowerPoint keeps degenerate elbow extents at 1 pt


def _bent3(cxn, bx: int, by: int, ex: int, ey: int, horizontal: bool, mid: float | None) -> None:
    """Geometry of ``bentConnector3`` from (bx, by) to (ex, ey) with its middle segment at ``mid``."""
    if horizontal:  # horizontal, vertical, horizontal: the preset's native orientation
        w = max(abs(ex - bx), _MIN_SPAN)
        flip_h = ex < bx
        local = None if mid is None else ((bx - mid) if flip_h else (mid - bx))
        _set_xfrm(cxn, bx - w if flip_h else bx, min(by, ey), w, abs(ey - by), 0, flip_h, ey < by)
    else:  # vertical, horizontal, vertical: the preset rotated a quarter turn
        w = max(abs(ey - by), _MIN_SPAN)
        h = abs(ex - bx)
        down = ey >= by
        end_y = by + w if down else by - w
        cx, cy = (bx + ex) / 2, (by + end_y) / 2
        flip_v = (ex > bx) if down else (ex < bx)
        local = None if mid is None else ((mid - by) if down else (by - mid))
        _set_xfrm(cxn, cx - w / 2, cy - h / 2, w, h, 5400000 if down else 16200000, False, flip_v)
    if local is not None:
        set_adjust(cxn, {"adj1": round(local / w * 100000)})


def _bent2(cxn, bx: int, by: int, ex: int, ey: int, horizontal_first: bool) -> None:
    """Geometry of ``bentConnector2`` (one bend) from (bx, by) to (ex, ey)."""
    geom = cxn._element.spPr.find(f"{{{A}}}prstGeom")
    geom.set("prst", "bentConnector2")
    if horizontal_first:  # native: along x, then along y
        _set_xfrm(cxn, min(bx, ex), min(by, ey), abs(ex - bx), abs(ey - by), 0, ex < bx, ey < by)
        return
    # Along y first: the preset rotated a quarter turn about the box center.
    w, h = abs(ey - by), abs(ex - bx)
    cx, cy = (bx + ex) / 2, (by + ey) / 2
    down = ey >= by
    flip_v = (ex > bx) if down else (ex < bx)
    _set_xfrm(cxn, cx - w / 2, cy - h / 2, w, h, 5400000 if down else 16200000, False, flip_v)
