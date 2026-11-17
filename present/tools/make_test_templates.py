#!/usr/bin/env python3
"""Generate the test templates in evals/templates/ (BUILD-SPEC W4).

Run with the kernel environment:  cd python/kernel && uv run python ../../tools/make_test_templates.py

Three templates are derived from python-pptx's default template by XML edits so that tests do
not depend on any proprietary file:

  plain/plain.pptx        the default template, unchanged apart from deterministic packaging
  corp/corp.potx          custom theme (colors, Carlito + DejaVu Serif), renamed and unhelpfully
                          named layouts, a Big Number layout, Disclaimer and Closing layouts,
                          shrink-on-overflow bodies, and a template.toml sidecar
  wide-dense/wide-dense.pptx   16:9, Liberation Sans, small text sizes
"""

from __future__ import annotations

import copy
import io
import sys
from pathlib import Path

import pptx
from lxml import etree
from pptx import Presentation
from pptx.opc.constants import RELATIONSHIP_TYPE as RT
from pptx.parts.slide import SlideLayoutPart

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "python" / "sdk"))
from present_sdk.zipfix import normalize_zip  # noqa: E402

A = "http://schemas.openxmlformats.org/drawingml/2006/main"
P = "http://schemas.openxmlformats.org/presentationml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
NS = {"a": A, "p": P}
ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "evals" / "templates"
DEFAULT = Path(pptx.__file__).parent / "templates" / "default.pptx"

TEMPLATE_CT = b"application/vnd.openxmlformats-officedocument.presentationml.template.main+xml"
PRES_CT = b"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"


def save(prs, path: Path, as_template: bool = False) -> None:
    buf = io.BytesIO()
    prs.save(buf)
    data = normalize_zip(buf.getvalue())
    if as_template:
        import zipfile

        src = zipfile.ZipFile(io.BytesIO(data))
        out = io.BytesIO()
        with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as dst:
            for name in src.namelist():
                blob = src.read(name)
                if name == "[Content_Types].xml":
                    blob = blob.replace(PRES_CT, TEMPLATE_CT)
                dst.writestr(name, blob)
        data = normalize_zip(out.getvalue())
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    print(f"wrote {path.relative_to(ROOT)} ({len(data)} bytes)")


def set_theme(prs, name: str, colors: dict[str, str], major: str, minor: str) -> None:
    for master in prs.slide_masters:
        part = master.part.part_related_by(RT.THEME)
        root = etree.fromstring(part.blob)
        root.set("name", name)
        scheme = root.find(".//a:clrScheme", NS)
        scheme.set("name", name)
        for key, val in colors.items():
            el = scheme.find(f"a:{key}", NS)
            for child in list(el):
                el.remove(child)
            etree.SubElement(el, f"{{{A}}}srgbClr", val=val)
        fs = root.find(".//a:fontScheme", NS)
        fs.set("name", name)
        fs.find("a:majorFont/a:latin", NS).set("typeface", major)
        fs.find("a:minorFont/a:latin", NS).set("typeface", minor)
        part._blob = etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def layout_by_name(prs, name: str):
    for layout in prs.slide_layouts:
        if layout.name == name:
            return layout
    raise KeyError(name)


def rename(layout, name: str) -> None:
    layout._element.find("p:cSld", NS).set("name", name)


def clone_layout(prs, source, name: str):
    master = prs.slide_masters[0]
    package = prs.part.package
    partname = package.next_partname("/ppt/slideLayouts/slideLayout%d.xml")
    part = SlideLayoutPart.load(partname, source.part.content_type, package, source.part.blob)
    part.relate_to(master.part, RT.SLIDE_MASTER)
    rid = master.part.relate_to(part, RT.SLIDE_LAYOUT)
    lst = master._element.find("p:sldLayoutIdLst", NS)
    next_id = max(int(el.get("id")) for el in lst) + 1
    el = etree.SubElement(lst, f"{{{P}}}sldLayoutId")
    el.set("id", str(next_id))
    el.set(f"{{{R}}}id", rid)
    layout = part.slide_layout
    rename(layout, name)
    return layout


def ph_sp(layout, idx: int):
    for sp in layout._element.iterfind(".//p:sp", NS):
        ph = sp.find("p:nvSpPr/p:nvPr/p:ph", NS)
        if ph is not None and int(ph.get("idx", "0")) == idx:
            return sp
    raise KeyError(idx)


def set_xfrm(sp, x: int, y: int, cx: int, cy: int) -> None:
    sppr = sp.find("p:spPr", NS)
    for old in sppr.findall("a:xfrm", NS):
        sppr.remove(old)
    xfrm = etree.Element(f"{{{A}}}xfrm")
    etree.SubElement(xfrm, f"{{{A}}}off", x=str(x), y=str(y))
    etree.SubElement(xfrm, f"{{{A}}}ext", cx=str(cx), cy=str(cy))
    sppr.insert(0, xfrm)


def set_lvl1(sp, size: int, bullet: bool | None = None, align: str | None = None, bold: bool | None = None) -> None:
    body = sp.find("p:txBody", NS)
    lst = body.find("a:lstStyle", NS)
    for old in lst.findall("a:lvl1pPr", NS):
        lst.remove(old)
    lvl = etree.Element(f"{{{A}}}lvl1pPr")
    if align:
        lvl.set("algn", align)
    if bullet is False:
        lvl.set("marL", "0")
        lvl.set("indent", "0")
        etree.SubElement(lvl, f"{{{A}}}buNone")
    rpr = etree.SubElement(lvl, f"{{{A}}}defRPr", sz=str(size))
    if bold is not None:
        rpr.set("b", "1" if bold else "0")
    lst.insert(0, lvl)


def set_autofit(layout_or_master, kind: str) -> None:
    for bp in layout_or_master._element.iterfind(".//p:sp/p:txBody/a:bodyPr", NS):
        for old in list(bp):
            if etree.QName(old).localname in ("normAutofit", "spAutoFit", "noAutofit"):
                bp.remove(old)
        etree.SubElement(bp, f"{{{A}}}{kind}")


def unvertical(layout) -> None:
    for bp in layout._element.iterfind(".//p:txBody/a:bodyPr", NS):
        if bp.get("vert"):
            del bp.attrib["vert"]
    for ph in layout._element.iterfind(".//p:ph", NS):
        if ph.get("orient"):
            del ph.attrib["orient"]


def make_plain() -> None:
    save(Presentation(str(DEFAULT)), OUT / "plain" / "plain.pptx")


def make_corp() -> None:
    prs = Presentation(str(DEFAULT))
    set_theme(prs, "Corp 2026", {
        "dk1": "1B1F24", "lt1": "FFFFFF", "dk2": "0B3C5D", "lt2": "EEF2F5",
        "accent1": "0B6E8E", "accent2": "D9822B", "accent3": "5B8C5A", "accent4": "8E4585",
        "accent5": "C0392B", "accent6": "7F8C8D", "hlink": "0B6E8E", "folHlink": "8E4585",
    }, major="DejaVu Serif", minor="Carlito")  # fmt: skip
    master = prs.slide_masters[0]
    set_autofit(master, "normAutofit")
    w, h = prs.slide_width, prs.slide_height

    # Unhelpful name, as real templates have: this one is a big-number layout.
    big = layout_by_name(prs, "Vertical Title and Text")
    unvertical(big)
    rename(big, "Layout 7")
    set_xfrm(ph_sp(big, 0), 457200, 274638, w - 914400, 1143000)
    set_xfrm(ph_sp(big, 1), 457200, 1700000, w - 914400, 2400000)
    set_lvl1(ph_sp(big, 1), 9600, bullet=False, align="ctr", bold=True)
    caption = copy.deepcopy(ph_sp(big, 1))
    caption.find("p:nvSpPr/p:cNvPr", NS).set("id", "9")
    caption.find("p:nvSpPr/p:cNvPr", NS).set("name", "Text Placeholder 8")
    caption.find("p:nvSpPr/p:nvPr/p:ph", NS).set("idx", "13")
    set_xfrm(caption, 457200, 4200000, w - 914400, 1100000)
    set_lvl1(caption, 2000, bullet=False, align="ctr")
    ph_sp(big, 1).addnext(caption)

    disclaimer = layout_by_name(prs, "Title and Vertical Text")
    unvertical(disclaimer)
    rename(disclaimer, "Disclaimer")
    set_lvl1(ph_sp(disclaimer, 1), 1200, bullet=False)

    closing = clone_layout(prs, layout_by_name(prs, "Title Slide"), "Closing")
    set_xfrm(ph_sp(closing, 0), 685800, 2500000, w - 1371600, 1200000)

    kpi = clone_layout(prs, layout_by_name(prs, "Two Content"), "Layout 12")
    set_lvl1(ph_sp(kpi, 1), 2000)
    set_lvl1(ph_sp(kpi, 2), 2000)

    for layout in prs.slide_layouts:
        if layout.name in ("Title and Content", "Two Content", "Comparison", "Content with Caption"):
            for sp in layout._element.iterfind(".//p:sp", NS):
                ph = sp.find("p:nvSpPr/p:nvPr/p:ph", NS)
                if ph is not None and ph.get("type") is None:
                    pass  # content placeholders keep master styles
    _ = h
    save(prs, OUT / "corp" / "corp.potx", as_template=True)
    (OUT / "corp" / "template.toml").write_text(CORP_SIDECAR, encoding="utf-8")


CORP_SIDECAR = """\
[template]
name = "Corp 2026"
owner = "Brand team (test fixture)"

[layouts."Title Slide"]
use_for = "Opening slide: deck title and audience or date"

[layouts."Title and Content"]
use_for = "One message with up to five supporting points, or one chart, table, or diagram"
max_bullets = 5

[layouts."Two Content"]
use_for = "Two things side by side: text next to a chart, image, or table"

[layouts."Layout 7"]
description = "Big number with caption"
use_for = "A single headline metric: the number in the body, its meaning in the caption"

[layouts."Layout 12"]
description = "Two columns with smaller text"
use_for = "Dense side-by-side comparison"

[layouts."Disclaimer"]
use_for = "Legal disclaimer text; required once per deck"

[layouts."Closing"]
use_for = "Final slide: next steps or thank you"

[layouts."Blank"]
avoid = true

[rules]
mandatory = ["Title Slide", "Disclaimer"]
closing = "Closing"
allow_free_elements = false
"""


def make_wide_dense() -> None:
    prs = Presentation(str(DEFAULT))
    set_theme(prs, "Wide Dense", {
        "dk1": "000000", "lt1": "FFFFFF", "dk2": "1F3864", "lt2": "F2F2F2",
        "accent1": "2F5597", "accent2": "C55A11", "accent3": "7B7B7B", "accent4": "BF9000",
        "accent5": "2E75B6", "accent6": "548235", "hlink": "0563C1", "folHlink": "954F72",
    }, major="Liberation Sans", minor="Liberation Sans")  # fmt: skip
    old_w = prs.slide_width
    prs.slide_width = 12192000
    scale = prs.slide_width / old_w
    master = prs.slide_masters[0]
    holders = [master, *master.slide_layouts]
    for holder in holders:
        for xfrm in holder._element.iterfind(".//p:spPr/a:xfrm", NS):
            off, ext = xfrm.find("a:off", NS), xfrm.find("a:ext", NS)
            off.set("x", str(int(int(off.get("x")) * scale)))
            ext.set("cx", str(int(int(ext.get("cx")) * scale)))
    # Dense text: shrink every master level.
    sizes = {"titleStyle": [2800], "bodyStyle": [1600, 1400, 1200, 1100, 1100]}
    for style, values in sizes.items():
        el = master._element.find(f"p:txStyles/p:{style}", NS)
        for i, size in enumerate(values):
            rpr = el.find(f"a:lvl{i + 1}pPr/a:defRPr", NS)
            if rpr is not None:
                rpr.set("sz", str(size))
    set_autofit(master, "noAutofit")
    save(prs, OUT / "wide-dense" / "wide-dense.pptx")


if __name__ == "__main__":
    make_plain()
    make_corp()
    make_wide_dense()
