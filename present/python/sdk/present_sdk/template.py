"""Template loading and description (PROTOCOL.md section 3).

One implementation serves the kernel's ``template.inspect`` and the SDK's own layout and
placeholder lookup, so placeholder keys are identical everywhere.
"""

from __future__ import annotations

import hashlib
import io
import re
import zipfile
from pathlib import Path
from typing import Any

from lxml import etree
from pptx import Presentation
from pptx.enum.shapes import PP_PLACEHOLDER
from pptx.opc.constants import RELATIONSHIP_TYPE as RT
from pptx.oxml.ns import qn

from .fonts import FontIndex

A = "http://schemas.openxmlformats.org/drawingml/2006/main"
P = "http://schemas.openxmlformats.org/presentationml/2006/main"
NS = {"a": A, "p": P}

THEME_COLORS = (
    "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5", "accent6", "hlink", "folHlink",
)  # fmt: skip
_DEFAULT_CLRMAP = {"tx1": "dk1", "bg1": "lt1", "tx2": "dk2", "bg2": "lt2"}

_TEMPLATE_CT = b"application/vnd.openxmlformats-officedocument.presentationml.template.main+xml"
_PRES_CT = b"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"

_ROLE = {
    PP_PLACEHOLDER.TITLE: ("title", "title"),
    PP_PLACEHOLDER.CENTER_TITLE: ("title", "ctrTitle"),
    PP_PLACEHOLDER.VERTICAL_TITLE: ("title", "title"),
    PP_PLACEHOLDER.SUBTITLE: ("subtitle", "subTitle"),
    PP_PLACEHOLDER.BODY: ("body", "body"),
    PP_PLACEHOLDER.VERTICAL_BODY: ("body", "body"),
    PP_PLACEHOLDER.OBJECT: ("body", "obj"),
    PP_PLACEHOLDER.VERTICAL_OBJECT: ("body", "obj"),
    PP_PLACEHOLDER.PICTURE: ("picture", "pic"),
    PP_PLACEHOLDER.CHART: ("chart", "chart"),
    PP_PLACEHOLDER.TABLE: ("table", "tbl"),
    PP_PLACEHOLDER.ORG_CHART: ("diagram", "dgm"),
    PP_PLACEHOLDER.MEDIA_CLIP: ("media", "media"),
    PP_PLACEHOLDER.BITMAP: ("picture", "clipArt"),
    PP_PLACEHOLDER.DATE: ("date", "dt"),
    PP_PLACEHOLDER.FOOTER: ("footer", "ftr"),
    PP_PLACEHOLDER.SLIDE_NUMBER: ("slide_number", "sldNum"),
}
_ACCEPTS = {
    "title": ["text"],
    "ctrTitle": ["text"],
    "subTitle": ["text"],
    "body": ["text"],
    "obj": ["text", "image", "chart", "table", "diagram"],
    "pic": ["image"],
    "clipArt": ["image"],
    "chart": ["chart"],
    "tbl": ["table"],
    "dgm": ["diagram"],
    "media": [],
    "dt": [],
    "ftr": ["text"],
    "sldNum": [],
}
_TITLE_TYPES = {"title", "ctrTitle"}
_BODY_TYPES = {"body", "obj", "subTitle", "pic", "chart", "tbl", "dgm", "media", "clipArt"}
FURNITURE_TYPES = {"dt", "ftr", "sldNum"}


def load_presentation(path: str | Path):
    """Open a ``.pptx`` or ``.potx`` as a python-pptx presentation.

    python-pptx rejects the template content type, so a ``.potx`` is patched in memory to the
    presentation content type; nothing on disk changes.
    """
    data = Path(path).read_bytes()
    with zipfile.ZipFile(io.BytesIO(data)) as zin:
        ct = zin.read("[Content_Types].xml")
        if _TEMPLATE_CT not in ct:
            return Presentation(io.BytesIO(data))
        out = io.BytesIO()
        with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as zout:
            for item in zin.infolist():
                blob = zin.read(item.filename)
                if item.filename == "[Content_Types].xml":
                    blob = blob.replace(_TEMPLATE_CT, _PRES_CT)
                zout.writestr(item, blob)
    out.seek(0)
    return Presentation(out)


def sha256_file(path: str | Path) -> str:
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


# ---- theme ---------------------------------------------------------------------------------


def _theme_root(master) -> etree._Element | None:
    try:
        part = master.part.part_related_by(RT.THEME)
    except KeyError:
        return None
    return etree.fromstring(part.blob)


def _read_theme(master) -> dict[str, Any]:
    root = _theme_root(master)
    colors = {name: "000000" for name in THEME_COLORS}
    fonts = {"major": "Calibri", "minor": "Calibri"}
    name = ""
    if root is not None:
        name = root.get("name", "")
        scheme = root.find(".//a:clrScheme", NS)
        if scheme is not None:
            for key in THEME_COLORS:
                el = scheme.find(f"a:{key}", NS)
                if el is None or len(el) == 0:
                    continue
                child = el[0]
                val = child.get("lastClr") if child.tag == f"{{{A}}}sysClr" else child.get("val")
                if val:
                    colors[key] = val.upper()
        fs = root.find(".//a:fontScheme", NS)
        if fs is not None:
            for which in ("major", "minor"):
                latin = fs.find(f"a:{which}Font/a:latin", NS)
                if latin is not None and latin.get("typeface"):
                    fonts[which] = latin.get("typeface")
                for script, key in (("ea", "ea"), ("cs", "cs")):
                    el = fs.find(f"a:minorFont/a:{script}", NS)
                    if el is not None and el.get("typeface"):
                        fonts[key] = el.get("typeface")
    return {"name": name, "colors": colors, "fonts": fonts}


def _clr_map(master) -> dict[str, str]:
    el = master._element.find("p:clrMap", NS)
    mapping = dict(_DEFAULT_CLRMAP)
    if el is not None:
        for key in ("tx1", "bg1", "tx2", "bg2"):
            if el.get(key):
                mapping[key] = el.get(key)
    return mapping


# ---- text style resolution -------------------------------------------------------------------


def _lvl_sources(layout_ph, master, ph_type: str, level: int) -> list[etree._Element]:
    """``a:lvlNpPr`` elements for ``level`` (0-based), highest priority first."""
    tag = f"a:lvl{level + 1}pPr"
    out: list[etree._Element] = []

    def from_sp(sp) -> None:
        if sp is None:
            return
        el = sp.find(f"p:txBody/a:lstStyle/{tag}", NS)
        if el is not None:
            out.append(el)

    from_sp(layout_ph._element)
    master_sp = _master_placeholder_sp(master, ph_type)
    from_sp(master_sp)
    style = "titleStyle" if ph_type in _TITLE_TYPES else "bodyStyle" if ph_type in _BODY_TYPES else "otherStyle"
    el = master._element.find(f"p:txStyles/p:{style}/{tag}", NS)
    if el is not None:
        out.append(el)
    return out


def _master_placeholder_sp(master, ph_type: str):
    want = "title" if ph_type in _TITLE_TYPES else ph_type if ph_type in FURNITURE_TYPES else "body"
    for sp in master._element.iterfind(".//p:sp", NS):
        ph = sp.find("p:nvSpPr/p:nvPr/p:ph", NS)
        if ph is None:
            continue
        if ph.get("type", "obj") == want:
            return sp
    return None


def _first(sources: list[etree._Element], getter):
    for el in sources:
        val = getter(el)
        if val is not None:
            return val
    return None


def _color_of(parent: etree._Element | None, clrmap: dict[str, str]) -> str | None:
    if parent is None:
        return None
    fill = parent.find("a:solidFill", NS)
    if fill is None or len(fill) == 0:
        return None
    child = fill[0]
    if child.tag == f"{{{A}}}schemeClr":
        val = child.get("val", "")
        return clrmap.get(val, val)
    if child.tag == f"{{{A}}}srgbClr":
        return "#" + child.get("val", "000000").upper()
    if child.tag == f"{{{A}}}sysClr":
        return "#" + child.get("lastClr", "000000").upper()
    return None


def _resolve_typeface(face: str | None, theme_fonts: dict[str, str], default: str) -> str:
    if not face:
        return theme_fonts[default]
    if face.startswith("+mj"):
        return theme_fonts["major"]
    if face.startswith("+mn"):
        return theme_fonts["minor"]
    return face


def _level_style(layout_ph, master, ph_type, level, theme, clrmap) -> dict[str, Any]:
    src = _lvl_sources(layout_ph, master, ph_type, level)
    is_title = ph_type in _TITLE_TYPES

    def rpr(el):
        return el.find("a:defRPr", NS)

    def attr(name):
        return lambda el: (rpr(el).get(name) if rpr(el) is not None else None)

    size = _first(src, attr("sz"))
    bold = _first(src, attr("b"))
    face = _first(
        src,
        lambda el: (
            rpr(el).find("a:latin", NS).get("typeface")
            if rpr(el) is not None and rpr(el).find("a:latin", NS) is not None
            else None
        ),
    )
    color = _first(src, lambda el: _color_of(rpr(el), clrmap))

    def bullet(el):
        if el.find("a:buNone", NS) is not None:
            return False
        if el.find("a:buChar", NS) is not None or el.find("a:buAutoNum", NS) is not None:
            return True
        return None

    def spc_before(el):
        pts = el.find("a:spcBef/a:spcPts", NS)
        if pts is not None:
            return int(pts.get("val", "0")) / 100
        pct = el.find("a:spcBef/a:spcPct", NS)
        if pct is not None:
            return -int(pct.get("val", "0")) / 100000  # negative = fraction of font size
        return None

    def line_spacing(el):
        pct = el.find("a:lnSpc/a:spcPct", NS)
        if pct is not None:
            return int(pct.get("val", "100000")) / 100000
        return None

    size_pt = int(size) / 100 if size else (44.0 if is_title else 18.0)
    before = _first(src, spc_before)
    if before is None:
        before = 0.0
    elif before < 0:
        before = -before * size_pt
    has_bullet = _first(src, bullet)
    return {
        "level": level,
        "font": _resolve_typeface(face, theme["fonts"], "major" if is_title else "minor"),
        "size_pt": size_pt,
        "bold": bold in ("1", "true"),
        "color": color or "dk1",
        "bullet": bool(has_bullet) if has_bullet is not None else False,
        "space_before_pt": round(before, 2),
        "line_spacing": _first(src, line_spacing) or 1.0,
        "mar_l": int(_first(src, lambda el: el.get("marL")) or 0),
    }


def _body_pr_chain(layout_ph, master, ph_type) -> list[etree._Element]:
    chain = []
    for sp in (layout_ph._element, _master_placeholder_sp(master, ph_type)):
        if sp is None:
            continue
        bp = sp.find("p:txBody/a:bodyPr", NS)
        if bp is not None:
            chain.append(bp)
    return chain


def _autofit(chain) -> str:
    for bp in chain:
        if bp.find("a:normAutofit", NS) is not None:
            return "shrink"
        if bp.find("a:spAutoFit", NS) is not None:
            return "resize"
        if bp.find("a:noAutofit", NS) is not None:
            return "none"
    return "none"


def _insets(chain) -> dict[str, int]:
    defaults = {"l": 91440, "t": 45720, "r": 91440, "b": 45720}
    out = {}
    for key, attr in (("l", "lIns"), ("t", "tIns"), ("r", "rIns"), ("b", "bIns")):
        val = None
        for bp in chain:
            if bp.get(attr) is not None:
                val = int(bp.get(attr))
                break
        out[key] = defaults[key] if val is None else val
    return out


def _anchor(chain) -> str:
    for bp in chain:
        if bp.get("anchor"):
            return bp.get("anchor")
    return "t"


# ---- description -----------------------------------------------------------------------------


def placeholder_keys(layout) -> list[tuple[str, Any]]:
    """Stable ``(key, placeholder)`` pairs for a layout, ordered by placeholder idx."""
    counts: dict[str, int] = {}
    out = []
    for ph in sorted(layout.placeholders, key=lambda p: p.placeholder_format.idx):
        role, _ = _ROLE.get(ph.placeholder_format.type, (f"idx:{ph.placeholder_format.idx}", "obj"))
        if role.startswith("idx:"):
            out.append((role, ph))
            continue
        counts[role] = counts.get(role, 0) + 1
        out.append((role if counts[role] == 1 else f"{role}-{counts[role]}", ph))
    return out


def _ph_type(ph) -> str:
    return _ROLE.get(ph.placeholder_format.type, ("", "obj"))[1]


def _table_styles(prs) -> tuple[list[dict[str, str]], str | None]:
    try:
        part = prs.part.part_related_by(RT.TABLE_STYLES)
    except KeyError:
        return [], None
    root = etree.fromstring(part.blob)
    styles = [{"id": el.get("styleId", ""), "name": el.get("styleName", "")} for el in root.iterfind("a:tblStyle", NS)]
    return styles, root.get("def")


def describe_presentation(prs, path: str = "", sha256: str = "", sidecar: dict | None = None,
                          fonts: FontIndex | None = None) -> dict[str, Any]:
    """Build the TemplateDescription for an opened template."""
    sidecar = sidecar or {}
    layout_hints = sidecar.get("layouts", {})
    masters = []
    used_fonts: set[str] = set()
    theme: dict[str, Any] = {"name": "", "colors": {}, "fonts": {"major": "Calibri", "minor": "Calibri"}}
    index = 0
    for mi, master in enumerate(prs.slide_masters):
        m_theme = _read_theme(master)
        if mi == 0:
            theme = m_theme
        clrmap = _clr_map(master)
        used_fonts.update(v for k, v in m_theme["fonts"].items() if k in ("major", "minor"))
        layouts = []
        for layout in master.slide_layouts:
            hint = layout_hints.get(layout.name, {})
            phs = []
            for key, ph in placeholder_keys(layout):
                ptype = _ph_type(ph)
                chain = _body_pr_chain(ph, master, ptype)
                n_levels = 5 if ptype in ("body", "obj") else 1
                levels = [_level_style(ph, master, ptype, lv, m_theme, clrmap) for lv in range(n_levels)]
                used_fonts.update(lv["font"] for lv in levels)
                phs.append({
                    "key": key,
                    "type": ptype,
                    "idx": ph.placeholder_format.idx,
                    "x": int(ph.left or 0), "y": int(ph.top or 0),
                    "cx": int(ph.width or 0), "cy": int(ph.height or 0),
                    "autofit": _autofit(chain),
                    "anchor": _anchor(chain),
                    "levels": levels,
                    "insets": _insets(chain),
                    "accepts": list(_ACCEPTS.get(ptype, [])),
                })  # fmt: skip
            fillable = any(p["accepts"] and p["type"] not in FURNITURE_TYPES for p in phs)
            info = {
                "name": layout.name,
                "master": master.name or f"Master {mi + 1}",
                "index": index,
                "placeholders": phs,
                "usable": (fillable or layout.name.lower() == "blank") and not hint.get("avoid", False),
            }
            desc = hint.get("description") or hint.get("use_for")
            if desc:
                info["description"] = desc
            layouts.append(info)
            index += 1
        masters.append({"name": master.name or f"Master {mi + 1}", "layouts": layouts})
    styles, default_style = _table_styles(prs)
    fonts = fonts or FontIndex.default()
    missing = sorted(f for f in used_fonts if f and not fonts.has_family(f))
    out = {
        "path": path,
        "sha256": sha256,
        "slide_size": {"cx": int(prs.slide_width), "cy": int(prs.slide_height)},
        "theme": theme,
        "masters": masters,
        "table_styles": styles,
        "fonts_missing": missing,
    }
    if default_style:
        out["default_table_style"] = default_style
    return out


def describe_template(path: str | Path, rel_path: str | None = None, sidecar: dict | None = None,
                      fonts: FontIndex | None = None) -> dict[str, Any]:
    prs = load_presentation(path)
    return describe_presentation(prs, rel_path or str(path), sha256_file(path), sidecar, fonts)


def find_layout(prs, name: str):
    for master in prs.slide_masters:
        for layout in master.slide_layouts:
            if layout.name == name:
                return layout
    raise KeyError(name)


def layout_info(description: dict[str, Any], name: str) -> dict[str, Any]:
    for master in description["masters"]:
        for layout in master["layouts"]:
            if layout["name"] == name:
                return layout
    raise KeyError(name)


_KEY_IDX = re.compile(r"^idx:(\d+)$")


def resolve_placeholder_info(layout: dict[str, Any], key: str) -> dict[str, Any]:
    """Find a placeholder description by role key or ``idx:<n>``."""
    m = _KEY_IDX.match(key)
    for ph in layout["placeholders"]:
        if ph["key"] == key or (m and ph["idx"] == int(m.group(1))):
            return ph
    raise KeyError(key)


__all__ = [
    "FURNITURE_TYPES",
    "THEME_COLORS",
    "describe_presentation",
    "describe_template",
    "find_layout",
    "layout_info",
    "load_presentation",
    "placeholder_keys",
    "qn",
    "resolve_placeholder_info",
    "sha256_file",
]  # fmt: skip
