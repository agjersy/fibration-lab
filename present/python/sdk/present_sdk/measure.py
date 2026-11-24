"""Text measurement with HarfBuzz shaping (PROTOCOL.md ``text.measure``).

The deck checker's ``fit.overflow`` verdict and diagram text sizing both come from here. It
approximates PowerPoint's layout: greedy line breaking, line height from the font's vertical
metrics, and the shrink-on-overflow search PowerPoint applies to ``normAutofit`` boxes.
"""

from __future__ import annotations

import functools
import unicodedata
from dataclasses import dataclass
from pathlib import Path

import uharfbuzz as hb

from .fonts import FontIndex

EMU_PER_PT = 12700


@dataclass
class Para:
    text: str
    font: str
    size_pt: float
    bold: bool = False
    italic: bool = False
    space_before_pt: float = 0.0
    line_spacing: float = 1.0
    indent_emu: int = 0  # left margin of the paragraph (level indent + bullet hang)


@dataclass
class Measure:
    fits: bool
    lines: int
    height: int  # EMU, including top and bottom insets
    font_found: bool
    shrink_to_pt: float | None = None
    scale: float = 1.0


@functools.lru_cache(maxsize=64)
def _hb_font(path: str, index: int):
    blob = hb.Blob(Path(path).read_bytes())
    face = hb.Face(blob, index)
    font = hb.Font(face)
    return font, face.upem


def _is_cjk(ch: str) -> bool:
    return unicodedata.east_asian_width(ch) in ("W", "F")


def _tokens(text: str) -> list[str]:
    """Break opportunities: after spaces and between CJK characters."""
    out: list[str] = []
    cur = ""
    for ch in text:
        if _is_cjk(ch):
            if cur:
                out.append(cur)
                cur = ""
            out.append(ch)
        elif ch == " ":
            cur += ch
            out.append(cur)
            cur = ""
        else:
            cur += ch
    if cur:
        out.append(cur)
    return out


class Measurer:
    def __init__(self, fonts: FontIndex | None = None) -> None:
        self.fonts = fonts or FontIndex.default()

    def _font(self, family: str, bold: bool, italic: bool):
        hit = self.fonts.find(family, bold, italic)
        found = hit is not None
        if hit is None:
            hit = self.fonts.fallback(bold, italic)
        if hit is None:
            return None, 1000, False
        font, upem = _hb_font(*hit)
        return font, upem, found

    def advance_pt(self, text: str, family: str, size_pt: float, bold: bool = False, italic: bool = False) -> float:
        font, upem, _ = self._font(family, bold, italic)
        if font is None:
            return len(text) * size_pt * 0.5
        buf = hb.Buffer()
        buf.add_str(text)
        buf.guess_segment_properties()
        hb.shape(font, buf)
        return sum(p.x_advance for p in buf.glyph_positions) * size_pt / upem

    def line_height_pt(self, family: str, size_pt: float, bold: bool = False, italic: bool = False) -> float:
        font, upem, _ = self._font(family, bold, italic)
        if font is None:
            return size_pt * 1.2
        ext = font.get_font_extents("ltr")
        height = (ext.ascender - ext.descender + ext.line_gap) * size_pt / upem
        return max(height, size_pt)

    def wrap(self, para: Para, width_pt: float, scale: float = 1.0) -> list[str]:
        size = para.size_pt * scale
        lines: list[str] = []
        for segment in para.text.split("\n"):
            cur = ""
            for tok in _tokens(segment):
                trial = cur + tok
                if cur and self.advance_pt(trial.rstrip(" "), para.font, size, para.bold, para.italic) > width_pt:
                    lines.append(cur.rstrip(" "))
                    cur = tok
                else:
                    cur = trial
            lines.append(cur.rstrip(" "))
        return lines

    def _layout(self, paras: list[Para], cx: int, insets: dict[str, int], scale: float, ls_scale: float):
        total_pt = 0.0
        n_lines = 0
        found = True
        for i, para in enumerate(paras):
            width_pt = max((cx - insets["l"] - insets["r"] - para.indent_emu) / EMU_PER_PT, 1.0)
            lines = self.wrap(para, width_pt, scale)
            _, _, ok = self._font(para.font, para.bold, para.italic)
            found = found and ok
            lh = self.line_height_pt(para.font, para.size_pt * scale, para.bold, para.italic)
            total_pt += len(lines) * lh * para.line_spacing * ls_scale
            if i > 0:
                total_pt += para.space_before_pt * scale
            n_lines += len(lines)
        height = int(total_pt * EMU_PER_PT) + insets["t"] + insets["b"]
        return n_lines, height, found

    def measure(self, paras: list[Para], cx: int, cy: int, insets: dict[str, int] | None = None,
                autofit: str = "none", min_font_pt: float = 0.0) -> Measure:
        insets = insets or {"l": 91440, "t": 45720, "r": 91440, "b": 45720}
        if not paras:
            return Measure(True, 0, insets["t"] + insets["b"], True)
        lines, height, found = self._layout(paras, cx, insets, 1.0, 1.0)
        if height <= cy or autofit == "resize":
            return Measure(True, lines, height, found)
        if autofit != "shrink":
            return Measure(False, lines, height, found)
        base = max(p.size_pt for p in paras)
        floor = min(p.size_pt for p in paras)
        scale = 1.0
        while scale > 0.25:
            scale = round(scale - 0.025, 3)
            # PowerPoint reduces line spacing by up to 20 % alongside the font scale.
            ls = max(0.8, 1.0 - (1.0 - scale) * 0.8)
            lines, height, found = self._layout(paras, cx, insets, scale, ls)
            if height <= cy:
                ok = floor * scale >= min_font_pt
                return Measure(ok, lines, height, found, shrink_to_pt=round(base * scale, 2), scale=scale)
            if floor * scale < max(min_font_pt, 1.0):
                break
        return Measure(False, lines, height, found, shrink_to_pt=round(base * scale, 2), scale=scale)

    def fit_size(self, text: str, family: str, cx: int, cy: int, max_pt: float, min_pt: float,
                 bold: bool = False, insets: dict[str, int] | None = None) -> tuple[float, bool]:
        """Largest size in ``[min_pt, max_pt]`` (0.5 pt steps) at which ``text`` fits the box."""
        size = max_pt
        while size >= min_pt:
            m = self.measure([Para(text, family, size, bold)], cx, cy, insets)
            if m.fits:
                return size, True
            size -= 0.5
        return min_pt, False
