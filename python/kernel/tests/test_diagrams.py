"""Diagram primitives (BUILD-SPEC W6): every variant at 3, 7, and 15 items on the corp template."""

from __future__ import annotations

import os
import shutil
import subprocess
from datetime import date
from pathlib import Path
from typing import Any

import pytest
from corpus import make_project, slide
from lxml import etree
from pptx import Presentation
from present_sdk import tags as T
from present_sdk.content import ContentError
from present_sdk.diagrams import dates as D
from present_sdk.diagrams.canvas import text_area
from present_sdk.measure import Measurer, Para, _tokens

from present_kernel.kernel import Kernel

A = "http://schemas.openxmlformats.org/drawingml/2006/main"
P = "http://schemas.openxmlformats.org/presentationml/2006/main"
EID = "e-d1"
SIZES = (3, 7, 15)
VARIANTS = [
    ("process", "chevrons"), ("process", "steps"), ("process", "flowchart"),
    ("timeline", "milestones"), ("timeline", "swimlanes"), ("timeline", "gantt"),
    ("matrix", "2x2"), ("matrix", "grid"), ("matrix", "kpi-tiles"),
    ("hierarchy", "org"), ("hierarchy", "tree"), ("hierarchy", "driver-tree"),
]  # fmt: skip
CASES = [(f, v, n) for f, v in VARIANTS for n in SIZES]
MIN_PT = 10.0

# ---- fixture models --------------------------------------------------------------------------

_PHASES = ["Discover", "Define", "Design", "Build", "Test", "Launch", "Measure", "Refine", "Scale", "Review",
           "Plan", "Source", "Align", "Deliver", "Close"]  # fmt: skip
_DETAILS = ["Interview ten customers", "Agree the scope", "Draft two options", "Ship the pilot", "Check the results",
            "Roll out to all regions", "Track weekly"]  # fmt: skip
_DATES = ["2026-01-15", "2026-03", "2026-Q2", "2026-08-01", "2026-10", "2027", "2027-Q2", "2026-02-10", "2026-05",
          "2026-Q3", "2026-11-20", "2027-02", "2027-04-01", "2026-06-15", "2026-12"]  # fmt: skip
_ENDS = ["2026-03-31", "2026-05", "2026-Q3", "2026-10-15", "2026-12", "2027", "2027-Q3", "2026-04-30", "2026-08",
         "2026-Q4", "2027-01-31", "2027-05", "2027-06-30", "2026-09-15", "2027-03"]  # fmt: skip
_WORK = ["Market study", "Vendor selection", "Pilot build", "Security review", "Training", "Regional rollout",
         "Benefits tracking", "Contract signed", "Data migration", "User testing", "Go-live", "Hypercare",
         "Phase two scope", "Process redesign", "Board update"]  # fmt: skip
_LANES = ["Strategy", "Technology", "People"]
_KPIS = [("€12.4m", "Revenue"), ("18%", "Gross margin"), ("42", "Net promoter score"), ("3.1 pts", "Margin gain"),
         ("96%", "On-time delivery"), ("1,240", "Active customers"), ("7 days", "Cycle time"), ("€0.9m", "Savings"),
         ("14", "Open roles"), ("2.3x", "Pipeline cover"), ("88%", "Utilisation"), ("5", "Markets live"),
         ("31%", "Digital share"), ("€4.2m", "Capex"), ("0", "Lost-time incidents")]  # fmt: skip
_OPTIONS = ["Price rise", "New channel", "Bundle offer", "Loyalty scheme", "Outsourcing", "Automation", "Self-service",
            "Partner sales", "Site merger", "Range cut", "Fleet renewal", "Shared services", "Local sourcing",
            "Shift model", "Lean office"]  # fmt: skip
_ROLES = ["Chief Executive", "Finance", "Operations", "Sales", "Control", "Treasury", "Plants", "Logistics",
          "Quality", "Key accounts", "Inside sales", "Marketing", "Tax", "Purchasing", "Service"]  # fmt: skip
_DRIVERS = [("Profit", "€24m"), ("Revenue", "€180m"), ("Cost", "€156m"), ("Volume", "1.2m units"),
            ("Price", "€150"), ("Fixed cost", "€60m"), ("Variable cost", "€96m"), ("New customers", "0.3m"),
            ("Repeat customers", "0.9m"), ("List price", "€165"), ("Discount", "9%"), ("Materials", "€58m"),
            ("Labour", "€38m"), ("Rent", "€22m"), ("Overheads", "€38m")]  # fmt: skip
# Parent index per node for hierarchies of each size (None: root).
_TREES = {
    3: [None, 0, 0],
    7: [None, 0, 0, 1, 1, 2, 2],
    15: [None, 0, 0, 0, 1, 1, 2, 2, 2, 3, 3, 3, 1, 2, 3],
}
_DRIVER_TREES = {
    3: [None, 0, 0],
    7: [None, 0, 0, 1, 1, 2, 2],
    15: [None, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 6, 6, 5, 5],
}
_FLOWS: dict[int, tuple[list[tuple[str, str]], list[tuple[int, int, str | None]]]] = {
    3: ([("start", "Request"), ("step", "Review"), ("end", "Done")], [(0, 1, None), (1, 2, None)]),
    7: ([("start", "Request received"), ("step", "Check the data"), ("decision", "Complete?"), ("step", "Ask for details"),
         ("step", "Assess risk"), ("step", "Approve"), ("end", "Closed")],
        [(0, 1, None), (1, 2, None), (2, 3, "No"), (3, 1, None), (2, 4, "Yes"), (4, 5, None), (5, 6, None)]),
    15: ([("start", "Order in"), ("step", "Validate"), ("decision", "Valid?"), ("step", "Return to sales"),
          ("step", "Credit check"), ("decision", "Limit ok?"), ("step", "Escalate"), ("step", "Reserve stock"),
          ("step", "Plan route"), ("step", "Pick"), ("step", "Pack"), ("step", "Ship"), ("step", "Invoice"),
          ("step", "Collect"), ("end", "Closed")],
         [(0, 1, None), (1, 2, None), (2, 3, "No"), (3, 1, None), (2, 4, "Yes"), (4, 5, None), (5, 6, "No"),
          (5, 7, "Yes"), (6, 7, None), (7, 8, None), (7, 9, None), (8, 11, None), (9, 10, None), (10, 11, None),
          (11, 12, None), (12, 13, None), (13, 14, None), (6, 14, "Rejected")]),
}  # fmt: skip


def text(value: str) -> dict[str, Any]:
    return {"paragraphs": [{"runs": [{"t": value}]}]}


def model(family: str, variant: str, n: int) -> dict[str, Any]:
    out: dict[str, Any] = {"family": family, "variant": variant}
    if family == "process" and variant == "flowchart":
        nodes, edges = _FLOWS[n]
        out["nodes"] = [{"id": f"n{i + 1}", "label": text(t), "kind": k} for i, (k, t) in enumerate(nodes)]
        out["edges"] = [{"from": f"n{a + 1}", "to": f"n{b + 1}", **({"label": lab} if lab else {})} for a, b, lab in edges]
    elif family == "process":
        out["nodes"] = [{"id": f"n{i + 1}", "label": text(_PHASES[i])} for i in range(n)]
        if n <= 7:
            for i, node in enumerate(out["nodes"]):
                node["detail"] = text(_DETAILS[i])
    elif family == "timeline":
        items = []
        for i in range(n):
            item: dict[str, Any] = {"id": f"i{i + 1}", "label": text(_WORK[i]), "start": _DATES[i]}
            if variant != "milestones" and i % 4 != 3:
                item["end"] = _ENDS[i]
            if variant != "milestones" and n > 3:
                item["lane"] = _LANES[i % 3]
            items.append(item)
        out["items"] = items
        if variant != "milestones" and n > 3:
            out["lanes"] = list(_LANES)
        if n == 7:
            out["axis"] = {"from": "2026", "to": "2027-Q3", "unit": "quarter"}
    elif family == "matrix" and variant == "2x2":
        out["axes"] = {"x": "Effort", "y": "Impact", "x_low": "Low", "x_high": "High", "y_low": "Low", "y_high": "High"}
        out["cells"] = [{"x": round(0.08 + 0.84 * ((i * 0.618034 + 0.3) % 1), 3),
                         "y": round(0.08 + 0.84 * ((i * 0.377 + 0.55) % 1), 3), "label": text(_OPTIONS[i])} for i in range(n)]  # fmt: skip
    elif family == "matrix" and variant == "grid":
        cols = {3: 3, 7: 4, 15: 3}[n]
        rows = -(-n // cols)
        out["columns"] = ["Cost", "Speed", "Risk", "Fit"][:cols]
        out["rows"] = [f"Option {chr(65 + r)}" for r in range(rows)]
        out["cells"] = [{"row": i // cols, "col": i % cols, "score": i % 5, "label": text(["", "Good", "Mixed view"][i % 3])}
                        for i in range(n)]  # fmt: skip
    elif family == "matrix":
        out["cells"] = [{"value": v, "label": text(lab)} for v, lab in _KPIS[:n]]
    elif variant == "driver-tree":
        out["nodes"] = [{"id": f"n{i + 1}", "label": text(_DRIVERS[i][0]), "value": _DRIVERS[i][1],
                         **({"parent": f"n{p + 1}"} if p is not None else {})} for i, p in enumerate(_DRIVER_TREES[n])]  # fmt: skip
    else:
        out["nodes"] = [{"id": f"n{i + 1}", "label": text(_ROLES[i]), **({"parent": f"n{p + 1}"} if p is not None else {}),
                         **({"detail": text("12 people")} if variant == "org" and i < 4 else {})}
                        for i, p in enumerate(_TREES[n])]  # fmt: skip
    return out


_LONG = "An extraordinarily long label that keeps going well past anything a small shape could ever hold " * 6


def long_model(family: str) -> dict[str, Any]:
    variant = {"process": "chevrons", "timeline": "gantt", "matrix": "grid", "hierarchy": "tree"}[family]
    out = model(family, variant, 15)
    target = (out.get("nodes") or out.get("items") or out.get("cells"))[4]
    target["label"] = text(_LONG)
    return out


def diagram_slide(n: int, title: str, diagram: dict[str, Any]) -> dict[str, Any]:
    return slide(n, "Title and Content", title, placeholders={"body": {"type": "diagram", "id": EID, "diagram": diagram}})


# ---- rendered deck ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def rendered(tmp_path_factory):
    root = tmp_path_factory.mktemp("diagrams")
    slides = [diagram_slide(i + 1, f"{f} / {v} / {n}", model(f, v, n)) for i, (f, v, n) in enumerate(CASES)]
    project = make_project(root / "project", "corp", slides)
    kernel = Kernel(project=project, home=root / "home")
    try:
        result = kernel.dispatcher.call("deck.render", {"out": "exports/d.pptx", "snapshot_id": "h-1"})
        again = kernel.dispatcher.call("deck.render", {"out": "exports/d2.pptx", "snapshot_id": "h-1"})
    finally:
        kernel.close()
    prs = Presentation(str(project / result["out"]))
    return {"project": project, "result": result, "again": again, "prs": prs, "slides": list(prs.slides)}


def _group(sl):
    groups = [sh for sh in sl.shapes if sh._element.tag == f"{{{P}}}grpSp"]
    assert len(groups) == 1
    return groups[0]


def _body_box(sl) -> tuple[int, int, int, int]:
    for ph in sl.slide_layout.placeholders:
        if ph.placeholder_format.idx == 1:
            return int(ph.left), int(ph.top), int(ph.width), int(ph.height)
    raise AssertionError("layout has no body placeholder")


def _bbox(el: etree._Element) -> tuple[float, float, float, float]:
    xfrm = el.find(f".//{{{A}}}xfrm")
    off, ext = xfrm.find(f"{{{A}}}off"), xfrm.find(f"{{{A}}}ext")
    x, y, w, h = (int(off.get("x")), int(off.get("y")), int(ext.get("cx")), int(ext.get("cy")))
    if int(xfrm.get("rot", "0")) % 10800000 == 5400000:  # a quarter turn swaps the extents about the center
        cx, cy = x + w / 2, y + h / 2
        x, y, w, h = cx - h / 2, cy - w / 2, h, w
    return x, y, w, h


def _name(el: etree._Element) -> str:
    return el.find(f".//{{{P}}}cNvPr").get("name", "")


def _node_ids(diagram: dict[str, Any]) -> set[str]:
    entries = diagram.get("nodes") or diagram.get("items") or diagram.get("cells") or []
    return {e.get("id") or f"cell-{i + 1}" for i, e in enumerate(entries)}


def _overlaps(a, b, tol: float = 1.0) -> bool:
    return (min(a[0] + a[2], b[0] + b[2]) - max(a[0], b[0]) > tol) and (min(a[1] + a[3], b[1] + b[3]) - max(a[1], b[1]) > tol)


def _text_problems(el: etree._Element, measurer: Measurer, font: str) -> list[str]:
    """Measure a shape's text at the size written into its runs; return what does not fit."""
    body = el.find(f"{{{P}}}txBody")
    if body is None or not "".join(body.itertext()).strip():
        return []
    _, _, w, h = _bbox(el)
    geom = el.find(f".//{{{A}}}prstGeom")
    prst = geom.get("prst") if geom is not None else "rect"
    adj = None
    if geom is not None:
        for gd in geom.iter(f"{{{A}}}gd"):
            if gd.get("name") == "adj":
                adj = int(gd.get("fmla").split()[1])
    _, _, tw, th = text_area(prst, int(w), int(h), adj)
    pr = body.find(f"{{{A}}}bodyPr")
    if pr.get("vert") in ("vert", "vert270"):
        tw, th = th, tw
    insets = {k: int(pr.get(a, d)) for k, a, d in (("l", "lIns", 91440), ("t", "tIns", 45720), ("r", "rIns", 91440), ("b", "bIns", 45720))}
    paras, problems = [], []
    for p in body.findall(f"{{{A}}}p"):
        runs = p.findall(f"{{{A}}}r")
        if not runs:
            continue
        rpr = runs[0].find(f"{{{A}}}rPr")
        assert rpr is not None and rpr.get("sz"), f"{_name(el)}: run without an explicit size"
        size = int(rpr.get("sz")) / 100
        if size < MIN_PT:
            problems.append(f"{_name(el)}: {size} pt is below the minimum")
        assert rpr.find(f"{{{A}}}solidFill/{{{A}}}srgbClr") is None, f"{_name(el)}: literal color"
        paras.append(Para("".join(r.findtext(f"{{{A}}}t") or "" for r in runs), font, size, rpr.get("b") == "1"))
    avail = (tw - insets["l"] - insets["r"]) / 12700
    for para in paras:
        for tok in _tokens(para.text):
            if measurer.advance_pt(tok.rstrip(" "), para.font, para.size_pt, para.bold) > avail + 0.01:
                problems.append(f"{_name(el)}: word {tok!r} is wider than the shape")
    if not measurer.measure(paras, int(tw), int(th), insets).fits:
        problems.append(f"{_name(el)}: text is taller than the shape")
    return problems


@pytest.mark.parametrize("index", range(len(CASES)), ids=[f"{v}-{n}" for _, v, n in CASES])
def test_one_tagged_group(rendered, index):
    sl = rendered["slides"][index]
    group = _group(sl)
    assert T.read_shape_tags(sl.part, group._element).get("present.element") == EID
    assert group.name == EID
    assert len(group.shapes) >= CASES[index][2]
    assert group._element.find(f".//{{{P}}}grpSp") is None  # flat: native shapes and connectors only
    assert not group._element.xpath(".//*[local-name()='srgbClr']")  # theme colors only
    entry = next(e for e in rendered["result"]["element_map"] if e["slide_id"] == f"s-c{index + 1:03d}" and e["element_id"] == EID)
    assert entry["kind"] == "group"


@pytest.mark.parametrize("index", range(len(CASES)), ids=[f"{v}-{n}" for _, v, n in CASES])
def test_geometry(rendered, index):
    family, variant, n = CASES[index]
    sl = rendered["slides"][index]
    group = _group(sl)
    bx, by, bw, bh = _body_box(sl)
    tol = 12700
    children = [(el, _name(el), _bbox(el)) for el in group._element if el.tag in (f"{{{P}}}sp", f"{{{P}}}cxnSp")]
    for _, name, (x, y, w, h) in children:
        assert x >= bx - tol and y >= by - tol and x + w <= bx + bw + tol and y + h <= by + bh + tol, f"{name} leaves the box"
    ids = _node_ids(model(family, variant, n))
    nodes = [(name, box) for _, name, box in children if name in ids]
    assert {name for name, _ in nodes} == ids
    labels = [(name, box) for _, name, box in children if name.endswith(("-label", "-detail"))]
    for group_ in (nodes, labels):
        for i, (na, a) in enumerate(group_):
            for nb, b in group_[i + 1:]:
                assert not _overlaps(a, b), f"{na} overlaps {nb}"
    for na, a in labels:  # a label may touch only its own node
        for nb, b in nodes:
            if not na.startswith(nb + "-") and variant != "flowchart":
                assert not _overlaps(a, b), f"label {na} overlaps node {nb}"


@pytest.mark.parametrize("index", range(len(CASES)), ids=[f"{v}-{n}" for _, v, n in CASES])
def test_text_fits(rendered, index):
    sl = rendered["slides"][index]
    measurer = Measurer()
    problems = []
    for el in _group(sl)._element.iter(f"{{{P}}}sp"):
        problems += _text_problems(el, measurer, "Carlito")
    assert problems == []


def test_fixtures_render_without_warnings_and_deterministically(rendered):
    assert rendered["result"]["warnings"] == []
    assert rendered["result"]["sha256"] == rendered["again"]["sha256"]


@pytest.mark.parametrize("family", ["process", "timeline", "matrix", "hierarchy"])
def test_deliberately_long_text_reports_overflow(kernel_for, tmp_path, family):
    project = make_project(tmp_path / "p", "corp", [diagram_slide(1, "Too long", long_model(family))])
    result = kernel_for(project).dispatcher.call("deck.render", {"out": "exports/d.pptx", "snapshot_id": "h-1"})
    hits = [w for w in result["warnings"] if w["rule"] == "fit.overflow" and w["element_id"] == EID]
    assert len(hits) == 1 and hits[0]["slide_id"] == "s-c001"
    assert len(Presentation(str(project / result["out"])).slides) == 1


def test_flowchart_with_decision_and_back_edge(rendered):
    sl = rendered["slides"][CASES.index(("process", "flowchart", 7))]
    group = _group(sl)
    by_name = {sh.name: sh for sh in group.shapes}
    assert by_name["n3"]._element.find(f".//{{{A}}}prstGeom").get("prst") == "diamond"
    assert by_name["n1"]._element.find(f".//{{{A}}}prstGeom").get("prst") == "flowChartTerminator"
    ids = {sh.name: sh.shape_id for sh in group.shapes}
    cxns = {_name(el): el for el in group._element.iter(f"{{{P}}}cxnSp")}
    assert set(cxns) == {"n1-n2", "n2-n3", "n3-n4", "n4-n2", "n3-n5", "n5-n6", "n6-n7"}
    for name, el in cxns.items():
        a, b = name.split("-")
        st, end = el.find(f".//{{{A}}}stCxn"), el.find(f".//{{{A}}}endCxn")
        assert int(st.get("id")) == ids[a] and int(end.get("id")) == ids[b]
        assert el.find(f".//{{{A}}}tailEnd").get("type") == "triangle"
        assert el.find(f".//{{{A}}}prstGeom").get("prst") in ("bentConnector3", "line")
    back = cxns["n4-n2"]  # leaves and enters on the same side, routed around the nodes
    assert back.find(f".//{{{A}}}stCxn").get("idx") == back.find(f".//{{{A}}}endCxn").get("idx")
    assert {"n3-n4-label", "n3-n5-label"} <= set(by_name)


def test_flowchart_rejects_unknown_edge_ends(kernel_for, tmp_path):
    from present_kernel.errors import RpcError

    bad = model("process", "flowchart", 3)
    bad["edges"].append({"from": "n1", "to": "nope"})
    project = make_project(tmp_path / "p", "corp", [diagram_slide(1, "Bad", bad)])
    with pytest.raises(RpcError):
        kernel_for(project).dispatcher.call("deck.render", {"out": "exports/d.pptx", "snapshot_id": "h-1"})


def test_grid_scores_are_harvey_balls(rendered):
    sl = rendered["slides"][CASES.index(("matrix", "grid", 7))]
    shapes = {sh.name: sh for sh in _group(sl).shapes}
    for i in range(7):
        score = i % 5
        assert f"cell-{i + 1}-ball" in shapes
        assert (f"cell-{i + 1}-score" in shapes) == (score in (1, 2, 3))
    pie = shapes["cell-3-score"]._element
    assert pie.find(f".//{{{A}}}prstGeom").get("prst") == "pie"
    assert {gd.get("name"): gd.get("fmla") for gd in pie.iter(f"{{{A}}}gd")} == {"adj1": "val 16200000", "adj2": "val 5400000"}


@pytest.mark.parametrize(("value", "start", "end"), [
    ("2026", date(2026, 1, 1), date(2027, 1, 1)),
    ("2026-03", date(2026, 3, 1), date(2026, 4, 1)),
    ("2026-12", date(2026, 12, 1), date(2027, 1, 1)),
    ("2026-03-15", date(2026, 3, 15), date(2026, 3, 16)),
    ("2026-Q1", date(2026, 1, 1), date(2026, 4, 1)),
    ("2026-Q4", date(2026, 10, 1), date(2027, 1, 1)),
])  # fmt: skip
def test_date_parsing(value, start, end):
    assert D.parse_period(value) == (start, end)
    assert D.parse_start(value) == start and D.parse_end(value) == end


@pytest.mark.parametrize("value", ["2026-Q5", "2026-13", "2026-02-30", "March 2026", "26-03", ""])
def test_bad_dates_are_content_errors(value):
    with pytest.raises(ContentError):
        D.parse_period(value)


def test_axis_units_and_ticks():
    assert D.floor_unit(date(2026, 5, 20), "quarter") == date(2026, 4, 1)
    assert D.ceil_unit(date(2026, 5, 20), "month") == date(2026, 6, 1)
    assert D.floor_unit(date(2026, 5, 20), "week") == date(2026, 5, 18)
    ticks, step = D.ticks(date(2026, 1, 1), date(2027, 1, 1), "month", 12)
    assert step == 1 and len(ticks) == 13 and ticks[0] == date(2026, 1, 1)
    ticks, step = D.ticks(date(2026, 1, 1), date(2029, 1, 1), "month", 12)
    assert step == 3 and ticks[1] == date(2026, 4, 1)
    assert D.auto_unit(date(2026, 1, 1), date(2026, 1, 10)) == "day"
    assert D.auto_unit(date(2020, 1, 1), date(2026, 1, 1)) == "year"


# ---- contact sheets for visual review --------------------------------------------------------


def test_contact_sheets(rendered, tmp_path):
    """Fidelity renders of the fixtures, written to ``$PRESENT_TEST_ARTIFACTS`` for review."""
    target = os.environ.get("PRESENT_TEST_ARTIFACTS")
    soffice = shutil.which("soffice") or shutil.which("libreoffice")
    if not target or not soffice:
        return
    import pypdfium2
    from PIL import Image

    out = Path(target) / "diagrams"
    out.mkdir(parents=True, exist_ok=True)
    env = {**os.environ, "HOME": str(tmp_path)}
    done = subprocess.run(
        [soffice, f"-env:UserInstallation=file://{tmp_path / 'lo'}", "--headless", "--convert-to", "pdf", "--outdir",
         str(tmp_path), str(rendered["project"] / "exports/d.pptx")],
        env=env, capture_output=True, timeout=300, check=False)  # fmt: skip
    pdf_path = tmp_path / "d.pdf"
    if done.returncode != 0 or not pdf_path.is_file():
        return
    pdf = pypdfium2.PdfDocument(str(pdf_path))
    try:
        pages = [pdf[i].render(scale=1.6).to_pil().convert("RGB") for i in range(len(pdf))]
    finally:
        pdf.close()
    assert len(pages) == len(CASES)
    for (_, variant, n), page in zip(CASES, pages):
        page.save(out / f"{variant}-{n:02d}.png")
    w, h = pages[0].size
    for k, (family, variant) in enumerate(VARIANTS):
        sheet = Image.new("RGB", (w * len(SIZES), h), "white")
        for j in range(len(SIZES)):
            sheet.paste(pages[k * len(SIZES) + j], (j * w, 0))
        sheet.save(out / f"sheet-{family}-{variant}.png")
