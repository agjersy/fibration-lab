"""A corpus of fixture slides covering every content type (BUILD-SPEC W5 acceptance)."""

from __future__ import annotations

import json
import shutil
from pathlib import Path
from typing import Any

from PIL import Image, ImageDraw

REPO = Path(__file__).resolve().parents[3]
TEMPLATES = {
    "plain": REPO / "evals/templates/plain/plain.pptx",
    "corp": REPO / "evals/templates/corp/corp.potx",
    "wide-dense": REPO / "evals/templates/wide-dense/wide-dense.pptx",
}
# Layouts that only the corp template has, with stand-ins for the others.
_ALT = {"Layout 7": "Section Header", "Disclaimer": "Title and Content", "Closing": "Title Slide", "Layout 12": "Two Content"}

CITE = "ref:r-q3rep@3f9a1c2b7e01#p11:c120-188"
SALES = {
    "columns": ["Region", "Revenue", "Cost", "Margin"],
    "rows": [["EMEA", 120.5, 80, 0.336], ["AMER", 200, 150, 0.25], ["APAC", 90, 70, 0.222], ["LATAM", 45, 30, 0.333]],
    "loc": "Q3!A1:D5",
}


def T(*paras: Any) -> dict[str, Any]:
    out = []
    for p in paras:
        if isinstance(p, str):
            out.append({"runs": [{"t": p}]})
        elif isinstance(p, tuple):
            out.append({"level": p[0], "runs": [{"t": p[1]}]})
        else:
            out.append(p)
    return {"paragraphs": out}


def text(eid: str, *paras: Any) -> dict[str, Any]:
    return {"type": "text", "id": eid, "text": T(*paras)}


def chart(eid: str, ctype: str, **extra: Any) -> dict[str, Any]:
    model = {"type": ctype, "source": {"kind": "inline", "table": SALES, "cites": [CITE]}, "series_from": "columns",
             "category": "Region", "series": [{"name": "Revenue", "column": "Revenue"}, {"name": "Cost", "column": "Cost"}]}  # fmt: skip
    model.update(extra)
    return {"type": "chart", "id": eid, "chart": model}


def slide(n: int, layout: str, title: str | None, **extra: Any) -> dict[str, Any]:
    doc: dict[str, Any] = {"id": f"s-c{n:03d}", "kind": "layout", "layout": layout, "placeholders": {}, "elements": []}
    if title is not None:
        doc["placeholders"]["title"] = text(f"e-t{n:03d}", title)
    placeholders = extra.pop("placeholders", {})
    doc["placeholders"].update(placeholders)
    doc.update(extra)
    return doc


def corpus_slides() -> list[dict[str, Any]]:
    s: list[dict[str, Any]] = []
    n = 0

    def add(layout: str, title: str | None, **extra: Any) -> None:
        nonlocal n
        n += 1
        s.append(slide(n, layout, title, **extra))

    add("Title Slide", "Quarterly review", placeholders={"subtitle": text("e-sub1", "Leadership team · Q3")})
    add("Title and Content", "Margin recovered to 18% on pricing actions", placeholders={"body": text(
        "e-b2",
        {"level": 0, "runs": [{"t": "Gross margin up ", "cite": [CITE]}, {"t": "3.1 pts", "b": True, "color": "accent2", "cite": [CITE]},
                              {"t": " quarter on quarter"}]},
        {"level": 1, "runs": [{"t": "Two thirds from list-price changes in "}, {"t": "EMEA", "i": True}]},
        {"level": 2, "runs": [{"t": "See the "}, {"t": "pricing memo", "link": "https://example.com/memo", "u": True}]},
        {"level": 3, "runs": [{"t": "Level four"}]},
        {"level": 4, "runs": [{"t": "Level five"}]},
    )}, notes=T("Pause on the EMEA point.", "Second note paragraph."))  # fmt: skip
    add("Title and Content", "Numbered and plain paragraphs", placeholders={"body": text(
        "e-b3",
        {"bullet": "number", "runs": [{"t": "First step"}]}, {"bullet": "number", "runs": [{"t": "Second step"}]},
        {"bullet": "none", "runs": [{"t": "A paragraph without a bullet."}]},
        {"bullet": "none", "align": "center", "runs": [{"t": "Centered"}]},
        {"bullet": "none", "align": "right", "runs": [{"t": "Right aligned"}]},
    )})  # fmt: skip
    for ctype, more in (
        ("bar", {}), ("column", {"stacked": "stacked"}), ("column", {"stacked": "percent"}), ("line", {}),
        ("area", {"stacked": "stacked"}), ("pie", {"series": [{"name": "Revenue", "column": "Revenue"}]}),
        ("doughnut", {"series": [{"name": "Revenue", "column": "Revenue"}]}),
        ("scatter", {"category": "Revenue", "series": [{"name": "Cost", "column": "Cost"}]}),
        ("combo", {"series": [{"name": "Revenue", "column": "Revenue"}, {"name": "Margin", "column": "Margin", "type": "line"}]}),
    ):  # fmt: skip
        add("Title and Content", f"Chart: {ctype} {more.get('stacked', '')}".strip(),
            placeholders={"body": chart(f"e-ch{n + 1:03d}", ctype, **more)})  # fmt: skip
    add("Title and Content", "Table with header, banding, and a merged cell", placeholders={"body": {
        "type": "table", "id": "e-tbl1", "table": {"columns": 3, "header_row": True, "banded_rows": True, "rows": [
            {"cells": [{"text": T("Region")}, {"text": T("Revenue")}, {"text": T("Change")}]},
            {"cells": [{"text": T("EMEA")}, {"text": T({"runs": [{"t": "120.5", "cite": [CITE]}]})}, {"text": T("+3%")}]},
            {"cells": [{"text": T("AMER")}, {"text": T("200")}, {"text": T("−1%")}]},
            {"cells": [{"text": T("Total across all regions"), "span": 2}, {"text": T("455.5")}]},
        ]}}})  # fmt: skip
    add("Title and Content", "Image fitted in the body", placeholders={"body": {
        "type": "image", "id": "e-img1", "asset": "assets/wide.png", "alt": "A wide test image", "fit": "fit", "cite": [CITE]}})  # fmt: skip
    add("Two Content", "Text beside an image that fills its box", placeholders={
        "body": text("e-b15", "Left column text", (1, "Sub point")),
        "body-2": {"type": "image", "id": "e-img2", "asset": "assets/tall.png", "alt": "A tall test image", "fit": "fill"}})  # fmt: skip
    add("Picture with Caption", "Picture placeholder", placeholders={
        "picture": {"type": "image", "id": "e-img3", "asset": "assets/wide.png", "alt": "Cropped image", "fit": "crop",
                    "crop": {"l": 0.1, "t": 0.0, "r": 0.2, "b": 0.0}},
        "body": text("e-b16", "Caption text")})  # fmt: skip
    add("Two Content", "Text beside a chart", placeholders={
        "body": text("e-b17", "Revenue grew in every region", (1, "Fastest in AMER")),
        "body-2": chart("e-ch17", "column", labels=True, legend="top", number_format="0.0")})  # fmt: skip
    add("Comparison", "Comparison of two options", placeholders={
        "body": text("e-b18a", "Option A"), "body-2": text("e-b18b", "Cheaper", "Slower"),
        "body-3": text("e-b18c", "Option B"), "body-4": chart("e-ch18", "bar", legend="none")})  # fmt: skip
    add("Layout 7", "Headline metric", placeholders={"body": text("e-b19", "18%"), "body-2": text("e-b19c", "Gross margin in Q3")})
    add("Disclaimer", "Disclaimer", placeholders={"body": text("e-b20", "This document is a test fixture and contains no real data. " * 4)})
    add("Section Header", "Section two", placeholders={"body": text("e-b21", "What changed and why")})
    add("Title Only", "Free elements", elements=[
        {"id": "e-f1", "x": 600000, "y": 1800000, "cx": 2400000, "cy": 1200000,
         "content": {"type": "shape", "id": "e-f1", "shape": "rounded-rect", "fill": "accent1", "line": "dk2", "text": T("A box")}},
        {"id": "e-f2", "x": 3200000, "y": 2400000, "cx": 1500000, "cy": 0,
         "content": {"type": "shape", "id": "e-f2", "shape": "arrow", "line": "dk1"}},
        {"id": "e-f3", "x": 4900000, "y": 1800000, "cx": 3000000, "cy": 1200000,
         "content": {"type": "text", "id": "e-f3", "text": T("A free text box", (1, "with a second level"))}},
        {"id": "e-f4", "x": 600000, "y": 3400000, "cx": 1200000, "cy": 1200000,
         "content": {"type": "shape", "id": "e-f4", "shape": "ellipse", "fill": "accent3"}},
    ])  # fmt: skip
    add("Title and Content", "Slide with long notes", placeholders={"body": text("e-b23", "See notes")},
        notes=T(*[f"Talking point {i}: " + "detail " * 12 for i in range(1, 6)]))  # fmt: skip
    add("Title and Content", "Hidden backup slide", placeholders={"body": text("e-b24", "Backup")}, hidden=True)
    add("Title and Content", "Locked slide", placeholders={"body": text("e-b25", "Do not touch")}, locked=True)
    add("Title and Content", "Chart with series in rows", placeholders={"body": {"type": "chart", "id": "e-ch26", "chart": {
        "type": "line", "series_from": "rows", "source": {"kind": "inline", "cites": [CITE], "table": {
            "columns": ["Metric", "Q1", "Q2", "Q3"], "rows": [["Revenue", 300, 340, 455.5], ["Cost", 250, 270, 330]], "loc": ""}}}}})  # fmt: skip
    add("Title and Content", "Chart with titles and formats", placeholders={"body": chart(
        "e-ch27", "column", title="Revenue and cost", x_title="Region", y_title="EUR m", labels=True, legend="right",
        number_format="#,##0.0")})  # fmt: skip
    add("Title and Content", "Chart with series colors", placeholders={"body": chart("e-ch28", "column", series=[
        {"name": "Revenue", "column": "Revenue", "color": "accent2"}, {"name": "Cost", "column": "Cost", "color": "accent6"}])})  # fmt: skip
    add("Content with Caption", "Content with caption", placeholders={
        "body": chart("e-ch29", "pie", series=[{"name": "Cost", "column": "Cost"}], labels=True),
        "body-2": text("e-b29", "Cost split by region")})  # fmt: skip
    add("Layout 12", "Dense two columns", placeholders={
        "body": text("e-b30a", *[f"Left point {i}" for i in range(1, 6)]),
        "body-2": text("e-b30b", *[f"Right point {i}" for i in range(1, 6)])})  # fmt: skip
    add("Title and Content", "Unicode: Ünïcödé, 日本語のテキスト, emoji-free", placeholders={"body": text(
        "e-b31", "Größe und Maße: 3 × 4 m²", "価格は前年比で三パーセント上昇した", "Ελληνικά, кириллица, עברית")})  # fmt: skip
    add("Closing", "Thank you", placeholders={"subtitle": text("e-b32", "Questions")})
    return s


def adapt(doc: dict[str, Any], layouts: set[str]) -> dict[str, Any]:
    """Swap corp-only layouts for stand-ins and drop placeholders the stand-in lacks."""
    if doc["layout"] in layouts or doc["layout"] not in _ALT:
        return doc
    out = json.loads(json.dumps(doc))
    out["layout"] = _ALT[doc["layout"]]
    if out["layout"] in ("Section Header", "Title and Content"):
        out["placeholders"].pop("body-2", None)
    return out


def make_assets(project: Path) -> None:
    assets = project / "assets"
    assets.mkdir(parents=True, exist_ok=True)
    for name, size, color in (("wide.png", (1600, 800), (40, 90, 160)), ("tall.png", (600, 1200), (160, 90, 40))):
        im = Image.new("RGB", size, color)
        draw = ImageDraw.Draw(im)
        draw.rectangle([size[0] // 4, size[1] // 4, 3 * size[0] // 4, 3 * size[1] // 4], outline=(255, 255, 255), width=12)
        im.save(assets / name)


def make_project(root: Path, template: str = "corp", slides: list[dict[str, Any]] | None = None,
                 deck_extra: dict[str, Any] | None = None) -> Path:
    """Write a project folder with the given template and slides; returns the project path."""
    from present_sdk.template import describe_template

    src = TEMPLATES[template]
    (root / "template").mkdir(parents=True, exist_ok=True)
    shutil.copy(src, root / "template" / src.name)
    side = src.parent / "template.toml"
    if side.is_file():
        shutil.copy(side, root / "template" / "template.toml")
    make_assets(root)
    desc = describe_template(root / "template" / src.name)
    layouts = {lay["name"] for m in desc["masters"] for lay in m["layouts"]}
    docs = [adapt(d, layouts) for d in (corpus_slides() if slides is None else slides)]
    (root / "deck" / "slides").mkdir(parents=True, exist_ok=True)
    for doc in docs:
        (root / "deck" / "slides" / f"{doc['id']}.json").write_text(json.dumps(doc, indent=2, ensure_ascii=False), encoding="utf-8")
    ids = [d["id"] for d in docs]
    half = max(1, len(ids) // 2)
    deck = {
        "schema_version": 1, "mode": "structured", "title": "Corpus deck", "author": "Test",
        "template": f"template/{src.name}",
        "sections": [{"title": "Part one", "slides": ids[:half]}, {"title": "Part two", "slides": ids[half:]}]
        if len(ids) > 1 else [{"title": "", "slides": ids}],
        "footer": {"text": "Corpus", "slide_numbers": True}, "classification": "Internal",
    }  # fmt: skip
    deck.update(deck_extra or {})
    (root / "deck" / "deck.json").write_text(json.dumps(deck, indent=2), encoding="utf-8")
    (root / ".present").mkdir(exist_ok=True)
    return root
