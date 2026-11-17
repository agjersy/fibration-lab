"""Applying deck-document content models (PROTOCOL.md section 2) to python-pptx objects.

Everything here takes the JSON shapes of the deck schema as plain dicts, so the structured
renderer, ejected build scripts, and hand-written code-mode scripts share one implementation.
"""

from __future__ import annotations

import copy
from pathlib import Path
from typing import Any

from lxml import etree
from PIL import Image
from pptx.chart.data import CategoryChartData, XyChartData
from pptx.dml.color import RGBColor
from pptx.enum.chart import XL_CHART_TYPE, XL_LEGEND_POSITION
from pptx.enum.dml import MSO_THEME_COLOR
from pptx.enum.shapes import MSO_CONNECTOR, MSO_SHAPE
from pptx.enum.text import PP_ALIGN
from pptx.util import Emu

A = "http://schemas.openxmlformats.org/drawingml/2006/main"
C = "http://schemas.openxmlformats.org/drawingml/2006/chart"
P = "http://schemas.openxmlformats.org/presentationml/2006/main"

THEME = {
    "dk1": MSO_THEME_COLOR.DARK_1, "lt1": MSO_THEME_COLOR.LIGHT_1,
    "dk2": MSO_THEME_COLOR.DARK_2, "lt2": MSO_THEME_COLOR.LIGHT_2,
    "accent1": MSO_THEME_COLOR.ACCENT_1, "accent2": MSO_THEME_COLOR.ACCENT_2,
    "accent3": MSO_THEME_COLOR.ACCENT_3, "accent4": MSO_THEME_COLOR.ACCENT_4,
    "accent5": MSO_THEME_COLOR.ACCENT_5, "accent6": MSO_THEME_COLOR.ACCENT_6,
    "hlink": MSO_THEME_COLOR.HYPERLINK, "folHlink": MSO_THEME_COLOR.FOLLOWED_HYPERLINK,
}  # fmt: skip
_ALIGN = {"left": PP_ALIGN.LEFT, "center": PP_ALIGN.CENTER, "right": PP_ALIGN.RIGHT}


class ContentError(ValueError):
    """Content that cannot be rendered; carries the element id when known."""

    def __init__(self, message: str, element_id: str | None = None):
        super().__init__(message)
        self.element_id = element_id


def as_text(value: Any) -> dict[str, Any]:
    """Accept a Text model, a plain string, or a list of strings (one paragraph each)."""
    if isinstance(value, dict) and "paragraphs" in value:
        return value
    if isinstance(value, str):
        return {"paragraphs": [{"runs": [{"t": line}]} for line in value.split("\n")]}
    if isinstance(value, list):
        return {"paragraphs": [{"runs": [{"t": str(v)}]} for v in value]}
    raise ContentError(f"not a text value: {type(value).__name__}")


def text_plain(text: dict[str, Any] | None) -> str:
    if not text:
        return ""
    return "\n".join("".join(r.get("t", "") for r in p.get("runs", [])) for p in text.get("paragraphs", []))


def set_color(color_format, color: str) -> None:
    """Theme color name, or ``#RRGGBB`` (flagged by ``tpl.theme-colors``)."""
    if color in THEME:
        color_format.theme_color = THEME[color]
    elif color.startswith("#") and len(color) == 7:
        color_format.rgb = RGBColor.from_string(color[1:].upper())
    else:
        raise ContentError(f"unknown color {color!r}")


def apply_paragraph(p, para: dict[str, Any]) -> None:
    level = para.get("level") or 0
    if level:
        p.level = int(level)
    align = para.get("align", "auto")
    if align in _ALIGN:
        p.alignment = _ALIGN[align]
    bullet = para.get("bullet", "auto")
    if bullet in ("none", "number"):
        ppr = p._p.get_or_add_pPr()
        for old in ppr.findall(f"{{{A}}}buNone") + ppr.findall(f"{{{A}}}buAutoNum") + ppr.findall(f"{{{A}}}buChar"):
            ppr.remove(old)
        if bullet == "none":
            el = etree.SubElement(ppr, f"{{{A}}}buNone")
            if level == 0:
                ppr.set("marL", "0")
                ppr.set("indent", "0")
        else:
            el = etree.SubElement(ppr, f"{{{A}}}buAutoNum")
            el.set("type", "arabicPeriod")
    for run in para.get("runs", []):
        r = p.add_run()
        r.text = run.get("t", "")
        if run.get("b"):
            r.font.bold = True
        if run.get("i"):
            r.font.italic = True
        if run.get("u"):
            r.font.underline = True
        if run.get("color"):
            set_color(r.font.color, run["color"])
        if run.get("link"):
            r.hyperlink.address = run["link"]


def apply_text(text_frame, text: Any) -> None:
    """Replace the frame's content with a Text model, keeping the frame's inherited styles."""
    model = as_text(text)
    text_frame.clear()
    paragraphs = model.get("paragraphs", [])
    for i, para in enumerate(paragraphs):
        p = text_frame.paragraphs[0] if i == 0 else text_frame.add_paragraph()
        apply_paragraph(p, para)


def text_cites(text: dict[str, Any] | None) -> list[str]:
    out: list[str] = []
    for para in (text or {}).get("paragraphs", []):
        for run in para.get("runs", []):
            for c in run.get("cite") or []:
                if c not in out:
                    out.append(c)
    return out


# ---- geometry --------------------------------------------------------------------------------


def fit_box(img_w: int, img_h: int, x: int, y: int, cx: int, cy: int) -> tuple[int, int, int, int]:
    """Largest box with the image's aspect ratio centered inside (x, y, cx, cy)."""
    if img_w <= 0 or img_h <= 0:
        return x, y, cx, cy
    scale = min(cx / img_w, cy / img_h)
    w, h = int(img_w * scale), int(img_h * scale)
    return x + (cx - w) // 2, y + (cy - h) // 2, w, h


def add_picture(shapes, path: str | Path, box: tuple[int, int, int, int], fit: str = "fit",
                crop: dict[str, float] | None = None):
    x, y, cx, cy = box
    with Image.open(path) as im:
        iw, ih = im.size
    if fit == "fit":
        px, py, pw, ph = fit_box(iw, ih, x, y, cx, cy)
        return shapes.add_picture(str(path), Emu(px), Emu(py), Emu(pw), Emu(ph))
    pic = shapes.add_picture(str(path), Emu(x), Emu(y), Emu(cx), Emu(cy))
    if fit == "crop" and crop:
        pic.crop_left, pic.crop_top = crop.get("l", 0.0), crop.get("t", 0.0)
        pic.crop_right, pic.crop_bottom = crop.get("r", 0.0), crop.get("b", 0.0)
    else:  # fill: crop the longer dimension symmetrically so the box is covered
        box_ratio, img_ratio = cx / cy, iw / ih
        if img_ratio > box_ratio:
            excess = (1 - box_ratio / img_ratio) / 2
            pic.crop_left = pic.crop_right = excess
        elif img_ratio < box_ratio:
            excess = (1 - img_ratio / box_ratio) / 2
            pic.crop_top = pic.crop_bottom = excess
    return pic


# ---- charts ----------------------------------------------------------------------------------

_CHART_TYPES = {
    ("bar", "none"): XL_CHART_TYPE.BAR_CLUSTERED,
    ("bar", "stacked"): XL_CHART_TYPE.BAR_STACKED,
    ("bar", "percent"): XL_CHART_TYPE.BAR_STACKED_100,
    ("column", "none"): XL_CHART_TYPE.COLUMN_CLUSTERED,
    ("column", "stacked"): XL_CHART_TYPE.COLUMN_STACKED,
    ("column", "percent"): XL_CHART_TYPE.COLUMN_STACKED_100,
    ("line", "none"): XL_CHART_TYPE.LINE_MARKERS,
    ("line", "stacked"): XL_CHART_TYPE.LINE_MARKERS_STACKED,
    ("line", "percent"): XL_CHART_TYPE.LINE_MARKERS_STACKED_100,
    ("area", "none"): XL_CHART_TYPE.AREA,
    ("area", "stacked"): XL_CHART_TYPE.AREA_STACKED,
    ("area", "percent"): XL_CHART_TYPE.AREA_STACKED_100,
    ("pie", "none"): XL_CHART_TYPE.PIE,
    ("doughnut", "none"): XL_CHART_TYPE.DOUGHNUT,
    ("scatter", "none"): XL_CHART_TYPE.XY_SCATTER,
    ("combo", "none"): XL_CHART_TYPE.COLUMN_CLUSTERED,
}
_LEGEND = {
    "right": XL_LEGEND_POSITION.RIGHT, "bottom": XL_LEGEND_POSITION.BOTTOM, "top": XL_LEGEND_POSITION.TOP,
}  # fmt: skip


def _num(value: Any) -> float | None:
    if value is None or isinstance(value, bool):
        return None
    if isinstance(value, (int, float)):
        return float(value)
    try:
        return float(str(value).replace(",", ""))
    except ValueError:
        return None


def chart_series(chart: dict[str, Any], table: dict[str, Any]) -> tuple[list[Any], list[dict[str, Any]]]:
    """Resolve categories and series ``{name, values, type, color}`` from a chart model and its table."""
    columns = list(table.get("columns", []))
    rows = [list(r) for r in table.get("rows", [])]
    if not columns:
        raise ContentError("chart data has no columns")
    if chart.get("series_from", "columns") == "rows":
        # Each row is a series; the first cell names it; remaining headers are the categories.
        categories = columns[1:]
        series = [{"name": str(r[0]), "values": [_num(v) for v in r[1:]]} for r in rows]
        return categories, series
    cat_col = chart.get("category") or columns[0]
    if cat_col not in columns:
        raise ContentError(f"category column {cat_col!r} not in chart data {columns}")
    ci = columns.index(cat_col)
    categories = [r[ci] for r in rows]
    specs = chart.get("series")
    if not specs:
        specs = [{"name": c, "column": c} for i, c in enumerate(columns)
                 if i != ci and any(_num(r[i]) is not None for r in rows)]  # fmt: skip
    series = []
    for spec in specs:
        if spec["column"] not in columns:
            raise ContentError(f"series column {spec['column']!r} not in chart data {columns}")
        si = columns.index(spec["column"])
        series.append({
            "name": spec.get("name") or spec["column"],
            "values": [_num(r[si]) for r in rows],
            "type": spec.get("type"), "color": spec.get("color"), "axis": spec.get("axis"),
        })  # fmt: skip
    if not series:
        raise ContentError("chart has no numeric series")
    return categories, series


def _combo_split(chart_el: etree._Element, line_names: set[str]) -> None:
    """Move the named series of a clustered column chart into a ``c:lineChart`` on the same axes."""
    plot = chart_el.find(f".//{{{C}}}plotArea")
    bar = plot.find(f"{{{C}}}barChart")
    if bar is None:
        return
    line = etree.Element(f"{{{C}}}lineChart")
    etree.SubElement(line, f"{{{C}}}grouping").set("val", "standard")
    etree.SubElement(line, f"{{{C}}}varyColors").set("val", "0")
    moved = 0
    for ser in list(bar.findall(f"{{{C}}}ser")):
        name = "".join(ser.find(f"{{{C}}}tx").itertext()).strip() if ser.find(f"{{{C}}}tx") is not None else ""
        v = ser.find(f".//{{{C}}}tx//{{{C}}}v")
        if (v is not None and v.text in line_names) or name in line_names:
            bar.remove(ser)
            for tag in ("invertIfNegative",):
                for el in ser.findall(f"{{{C}}}{tag}"):
                    ser.remove(el)
            marker = etree.Element(f"{{{C}}}marker")
            etree.SubElement(marker, f"{{{C}}}symbol").set("val", "circle")
            cat = ser.find(f"{{{C}}}cat")
            (cat if cat is not None else ser[-1]).addprevious(marker)
            etree.SubElement(ser, f"{{{C}}}smooth").set("val", "0")
            line.append(ser)
            moved += 1
    if not moved:
        return
    etree.SubElement(line, f"{{{C}}}marker").set("val", "1")
    for ax in bar.findall(f"{{{C}}}axId"):
        line.append(copy.deepcopy(ax))
    bar.addnext(line)


def add_chart(shapes, chart: dict[str, Any], table: dict[str, Any], box: tuple[int, int, int, int]):
    """Add a native chart (embedded workbook) and return its graphic frame."""
    ctype = chart.get("type", "column")
    stacked = chart.get("stacked", "none") or "none"
    xl = _CHART_TYPES.get((ctype, stacked)) or _CHART_TYPES.get((ctype, "none"))
    if xl is None:
        raise ContentError(f"unsupported chart type {ctype!r}")
    categories, series = chart_series(chart, table)
    number_format = chart.get("number_format") or "General"
    if ctype == "scatter":
        data = XyChartData(number_format=number_format)
        xs = [_num(c) for c in categories]
        for s in series:
            ser = data.add_series(s["name"])
            for xv, yv in zip(xs, s["values"]):
                if xv is not None and yv is not None:
                    ser.add_data_point(xv, yv)
    else:
        data = CategoryChartData(number_format=number_format)
        data.categories = ["" if c is None else (c if isinstance(c, (int, float)) else str(c)) for c in categories]
        for s in series:
            data.add_series(s["name"], s["values"])
    x, y, cx, cy = box
    frame = shapes.add_chart(xl, Emu(x), Emu(y), Emu(cx), Emu(cy), data)
    ch = frame.chart
    if ctype == "combo":
        lines = {s["name"] for s in series if s.get("type") == "line"}
        if lines:
            _combo_split(ch._chartSpace, lines)
    if chart.get("title"):
        ch.has_title = True
        ch.chart_title.text_frame.text = chart["title"]
    else:
        ch.has_title = False
    legend = chart.get("legend")
    if legend is None:
        legend = "bottom" if (len(series) > 1 or ctype in ("pie", "doughnut")) else "none"
    if legend == "none":
        ch.has_legend = False
    else:
        ch.has_legend = True
        ch.legend.position = _LEGEND.get(legend, XL_LEGEND_POSITION.BOTTOM)
        ch.legend.include_in_layout = False
    if ctype not in ("pie", "doughnut"):
        for title_key, axis_name in (("x_title", "category_axis"), ("y_title", "value_axis")):
            if chart.get(title_key):
                try:
                    axis = getattr(ch, axis_name)
                    axis.has_title = True
                    axis.axis_title.text_frame.text = chart[title_key]
                except (ValueError, AttributeError):
                    pass
        if chart.get("number_format"):
            try:
                ch.value_axis.tick_labels.number_format = chart["number_format"]
                ch.value_axis.tick_labels.number_format_is_linked = False
            except (ValueError, AttributeError):
                pass
    if chart.get("labels"):
        for plot in ch.plots:
            plot.has_data_labels = True
            if chart.get("number_format"):
                plot.data_labels.number_format = chart["number_format"]
                plot.data_labels.number_format_is_linked = False
    by_name = {s["name"]: s for s in series}
    for plot in ch.plots:
        for ser in plot.series:
            spec = by_name.get(ser.name)
            if spec and spec.get("color"):
                fmt = ser.format.line if (ctype == "line" or spec.get("type") == "line") else ser.format.fill
                if fmt is ser.format.fill:
                    fmt.solid()
                    set_color(fmt.fore_color, spec["color"])
                else:
                    set_color(fmt.color, spec["color"])
    return frame


# ---- tables ----------------------------------------------------------------------------------


def add_table(shapes, table: dict[str, Any], box: tuple[int, int, int, int], style_id: str | None = None):
    rows = table.get("rows", [])
    n_cols = int(table.get("columns") or max((sum(c.get("span", 1) for c in r.get("cells", [])) for r in rows), default=1))
    if not rows:
        raise ContentError("table has no rows")
    x, y, cx, cy = box
    # Rows take their natural height (PowerPoint grows them to fit text); never taller than the box.
    height = min(cy, len(rows) * 370840)
    frame = shapes.add_table(len(rows), n_cols, Emu(x), Emu(y), Emu(cx), Emu(height))
    tbl = frame.table
    tbl.first_row = bool(table.get("header_row", True))
    tbl.horz_banding = bool(table.get("banded_rows", True))
    if style_id:
        tbl_pr = tbl._tbl.tblPr
        sid = tbl_pr.find(f"{{{A}}}tableStyleId")
        if sid is None:
            sid = etree.SubElement(tbl_pr, f"{{{A}}}tableStyleId")
        sid.text = style_id
    for ri, row in enumerate(rows):
        col = 0
        for cell in row.get("cells", []):
            if col >= n_cols:
                break
            target = tbl.cell(ri, col)
            span = max(1, int(cell.get("span", 1)))
            if span > 1 and col + span - 1 < n_cols:
                target.merge(tbl.cell(ri, col + span - 1))
            apply_text(target.text_frame, cell.get("text", ""))
            col += span
    return frame


# ---- simple shapes ---------------------------------------------------------------------------

_SHAPES = {
    "rect": MSO_SHAPE.RECTANGLE, "rounded-rect": MSO_SHAPE.ROUNDED_RECTANGLE, "ellipse": MSO_SHAPE.OVAL,
}  # fmt: skip


def add_shape(shapes, spec: dict[str, Any], box: tuple[int, int, int, int]):
    x, y, cx, cy = box
    kind = spec.get("shape", "rect")
    if kind in ("line", "arrow"):
        shp = shapes.add_connector(MSO_CONNECTOR.STRAIGHT, Emu(x), Emu(y), Emu(x + cx), Emu(y + cy))
        if spec.get("line"):
            set_color(shp.line.color, spec["line"])
        if kind == "arrow":
            ln = shp.line._get_or_add_ln()
            etree.SubElement(ln, f"{{{A}}}tailEnd").set("type", "triangle")
        return shp
    shp = shapes.add_shape(_SHAPES.get(kind, MSO_SHAPE.RECTANGLE), Emu(x), Emu(y), Emu(cx), Emu(cy))
    if spec.get("fill"):
        shp.fill.solid()
        set_color(shp.fill.fore_color, spec["fill"])
    if spec.get("line"):
        set_color(shp.line.color, spec["line"])
    if spec.get("text"):
        apply_text(shp.text_frame, spec["text"])
    return shp


def set_alt(shape_el: etree._Element, alt: str) -> None:
    """Alt text lives in ``descr`` on the shape's ``p:cNvPr``."""
    for child in shape_el:
        if etree.QName(child).localname.startswith("nv"):
            cnv = child.find(f"{{{P}}}cNvPr")
            if cnv is not None:
                cnv.set("descr", alt)
            return


def set_name(shape_el: etree._Element, name: str) -> None:
    for child in shape_el:
        if etree.QName(child).localname.startswith("nv"):
            cnv = child.find(f"{{{P}}}cNvPr")
            if cnv is not None:
                cnv.set("name", name)
            return
