from __future__ import annotations

import json
import shutil
from pathlib import Path

import pytest
from corpus import CITE, SALES, T, chart, make_project, slide, text
from PIL import Image

from present_kernel.checker.numbers import check_run, extract_numbers

HAVE_LO = shutil.which("soffice") is not None


def check(kernel_for, project, **params):
    k = kernel_for(project)
    k.dispatcher.call("deck.render", {"out": "exports/deck.pptx", "snapshot_id": "h-1"})
    return k.dispatcher.call("check.run", {"pptx": "exports/deck.pptx", "engine": False, **params})


def rules_of(diags, rule):
    return [d for d in diags if d["rule"] == rule]


def good_slide(n=1, **extra):
    return slide(n, "Title and Content", f"A clear title {n}", placeholders={"body": text(
        f"e-b{n}", {"runs": [{"t": "Revenue grew to 120.5", "cite": [CITE]}]}, "A second point")}, **extra)


@pytest.fixture()
def project(tmp_path):
    def make(slides, template="plain", **deck_extra):
        return make_project(tmp_path / "p", template, slides, deck_extra or None)

    return make


def test_clean_deck_has_no_findings(kernel_for, project):
    diags = check(kernel_for, project([good_slide(1), good_slide(2)], classification=None))
    # The corpus citation points at a reference that is not in this project; that finding is expected.
    dangling = rules_of(diags, "src.dangling")
    assert len(dangling) == 2 and all(d["severity"] == "error" for d in dangling)
    assert [d for d in diags if d["severity"] != "info" and d["rule"] != "src.dangling"] == []


def test_overflow_fails_and_short_text_passes(kernel_for, project):
    long = slide(1, "Title and Content", "Too much text", placeholders={"body": text("e-long", *["A long supporting point that goes on and on. " * 3] * 30)})
    diags = check(kernel_for, project([long, good_slide(2)], "wide-dense"))
    hits = rules_of(diags, "fit.overflow")
    assert [d["element_id"] for d in hits] == ["e-long"] and hits[0]["slide_id"] == "s-c001"
    assert hits[0]["severity"] == "error" and "fix_hint" in hits[0]


def test_shrink_boxes_fit_until_the_minimum(kernel_for, project):
    some = slide(1, "Title and Content", "Shrinks", placeholders={"body": text("e-some", *[f"Supporting point number {i}" for i in range(12)])})
    lots = slide(2, "Title and Content", "Cannot shrink enough", placeholders={"body": text("e-lots", *["A long supporting point that goes on and on. " * 3] * 40)})
    p = project([some, lots])
    diags = check(kernel_for, p)
    assert [d["element_id"] for d in rules_of(diags, "fit.overflow")] == ["e-lots"]
    from pptx import Presentation

    xml = Presentation(str(p / "exports/deck.pptx")).slides[0].shapes[1]._element.xml
    assert "fontScale" in xml  # the renderer wrote PowerPoint's shrink for the box that needed it


def test_theme_fonts_and_colors(kernel_for, project):
    bad = slide(1, "Title Only", "Off brand", elements=[{"id": "e-x", "x": 500000, "y": 2000000, "cx": 3000000, "cy": 800000,
        "content": {"type": "text", "id": "e-x", "text": T({"runs": [{"t": "Pink text", "color": "#FF00AA"}]})}}])  # fmt: skip
    p = project([bad])
    (p / ".present" / "project.toml").write_text('[contract]\nallow_free_elements = true\n')
    diags = check(kernel_for, p)
    assert rules_of(diags, "tpl.theme-colors")[0]["element_id"] == "e-x"
    assert rules_of(diags, "tpl.layout-only") == []
    # A font override written by code is caught on the built file.
    from pptx import Presentation

    prs = Presentation(str(p / "exports/deck.pptx"))
    next(s for s in prs.slides[0].shapes if s.name == "e-x").text_frame.paragraphs[0].runs[0].font.name = "Comic Sans MS"
    prs.save(str(p / "exports/deck.pptx"))
    diags = kernel_for(p).dispatcher.call("check.run", {"pptx": "exports/deck.pptx", "engine": False})
    assert "Comic Sans MS" in rules_of(diags, "tpl.theme-fonts")[0]["message"]


def test_free_elements_need_permission(kernel_for, project):
    doc = slide(1, "Title Only", "Free", elements=[{"id": "e-f", "x": 0, "y": 2000000, "cx": 1000000, "cy": 500000,
        "content": {"type": "shape", "id": "e-f", "shape": "rect", "fill": "accent1"}}])  # fmt: skip
    assert rules_of(check(kernel_for, project([doc])), "tpl.layout-only")


def test_density(kernel_for, project):
    many = slide(1, "Title and Content", "Dense", placeholders={"body": text("e-m", *[f"Point {i}" for i in range(9)])})
    wordy = slide(2, "Title and Content", "Wordy", placeholders={"body": text("e-w", "word " * 90)})
    diags = check(kernel_for, project([many, wordy]))
    assert rules_of(diags, "density.bullets")[0]["slide_id"] == "s-c001"
    assert rules_of(diags, "density.words")[0]["slide_id"] == "s-c002"


def test_sidecar_bullet_limit_and_mandatory_layouts(kernel_for, project):
    six = slide(1, "Title and Content", "Six points", placeholders={"body": text("e-6", *[f"Point {i}" for i in range(6)])})
    diags = check(kernel_for, project([six], "corp"))
    assert "limit is 5" in rules_of(diags, "density.bullets")[0]["message"]
    messages = [d["message"] for d in rules_of(diags, "tpl.mandatory")]
    assert any("Disclaimer" in m for m in messages) and any("Closing" in m for m in messages)


def test_coverage_and_explicit_non_factual(kernel_for, project):
    uncited = slide(1, "Title and Content", "Uncited", placeholders={"body": text("e-u", "Revenue grew 12% to 4.5m")})
    waived = slide(2, "Title and Content", "Waived", placeholders={"body": text("e-v", {"runs": [{"t": "Step 2 of 5", "cite": []}]})})
    nochart = slide(3, "Title and Content", "Chart", placeholders={"body": {"type": "chart", "id": "e-c", "chart": {
        "type": "bar", "series_from": "columns", "source": {"kind": "inline", "table": SALES, "cites": []}}}})  # fmt: skip
    diags = rules_of(check(kernel_for, project([uncited, waived, nochart])), "src.coverage")
    assert [(d["slide_id"], d["element_id"]) for d in diags] == [("s-c001", "e-u"), ("s-c003", "e-c")]
    assert diags[0]["pointer"] == "/placeholders/body/text/paragraphs/0"


def test_alt_text_and_resolution(kernel_for, project, tmp_path):
    doc = slide(1, "Title and Content", "Image", placeholders={"body": {"type": "image", "id": "e-i", "asset": "assets/tiny.png", "alt": ""}})
    p = project([doc])
    Image.new("RGB", (120, 60), (10, 20, 30)).save(p / "assets" / "tiny.png")
    diags = check(kernel_for, p)
    assert rules_of(diags, "a11y.alt-text")[0]["element_id"] == "e-i"
    assert "dpi" in rules_of(diags, "img.resolution")[0]["message"]


def test_contrast(kernel_for, project):
    doc = slide(1, "Title and Content", "Contrast", placeholders={"body": text("e-c", {"runs": [{"t": "Pale text", "color": "lt2"}]}, "Normal text")})
    diags = rules_of(check(kernel_for, project([doc])), "a11y.contrast")
    assert len(diags) == 1 and diags[0]["element_id"] == "e-c"


def test_titles_must_exist_and_be_unique(kernel_for, project):
    a = good_slide(1)
    b = good_slide(2)
    b["placeholders"]["title"] = text("e-t2", "A clear title 1")
    c = slide(3, "Title and Content", None, placeholders={"body": text("e-b3", "No title here")})
    diags = rules_of(check(kernel_for, project([a, b, c])), "struct.titles")
    assert sorted(d["slide_id"] for d in diags) == ["s-c001", "s-c002", "s-c003"]


def test_slide_and_rule_filters(kernel_for, project):
    many = slide(1, "Title and Content", "Dense", placeholders={"body": text("e-m", *[f"Point {i}" for i in range(9)])})
    other = slide(2, "Title and Content", "Dense too", placeholders={"body": text("e-n", *[f"Point {i}" for i in range(9)])})
    p = project([many, other])
    assert {d["slide_id"] for d in check(kernel_for, p, slides=["s-c002"])} == {"s-c002"}
    assert {d["rule"] for d in check(kernel_for, p, rules=["density.bullets"])} == {"density.bullets"}


def test_severity_override(kernel_for, project):
    many = slide(1, "Title and Content", "Dense", placeholders={"body": text("e-m", *[f"Point {i}" for i in range(9)])})
    p = project([many])
    (p / ".present" / "project.toml").write_text('[contract]\n"density.bullets" = "error"\nmax_bullets = 20\n"struct.titles" = "off"\n')
    assert rules_of(check(kernel_for, p), "density.bullets") == []
    (p / ".present" / "project.toml").write_text('[contract]\n"density.bullets" = "error"\n')
    assert rules_of(check(kernel_for, p), "density.bullets")[0]["severity"] == "error"


def test_broken_file_is_reported(kernel_for, project):
    p = project([good_slide(1)])
    (p / "exports").mkdir(exist_ok=True)
    (p / "exports" / "bad.pptx").write_bytes(b"not a zip")
    diags = kernel_for(p).dispatcher.call("check.run", {"pptx": "exports/bad.pptx"})
    assert diags[0]["rule"] == "out.opens-clean"


@pytest.mark.skipif(not HAVE_LO, reason="LibreOffice is not installed")
def test_opens_clean_with_the_engine(kernel_for, corp_project):
    k = kernel_for(corp_project)
    k.dispatcher.call("deck.render", {"out": "exports/deck.pptx", "snapshot_id": "h-1"})
    diags = k.dispatcher.call("check.run", {"pptx": "exports/deck.pptx", "rules": ["out.opens-clean"]})
    assert diags == []


def test_text_measure_rpc(kernel_for, tmp_path):
    k = kernel_for(tmp_path)
    box = {"cx": 3000000, "cy": 400000}
    short = k.dispatcher.call("text.measure", {"font_family": "Liberation Sans", "size_pt": 18, "paragraphs": ["Short"], "box": box})
    long = k.dispatcher.call("text.measure", {"font_family": "Liberation Sans", "size_pt": 18, "paragraphs": ["Long text " * 30], "box": box})
    shrunk = k.dispatcher.call("text.measure", {"font_family": "Liberation Sans", "size_pt": 18, "paragraphs": ["Long text " * 6], "box": box, "autofit": "shrink"})
    assert short == {"fits": True, "lines": 1, "height": short["height"], "font_found": True}
    assert long["fits"] is False and long["lines"] > 3
    assert shrunk["fits"] is True and shrunk["shrink_to_pt"] < 18


# ---- the number check (PROTOCOL 8.3) ---------------------------------------------------------

SOURCE = ("Gross margin rose to 18.2% from 15.1% a year earlier. Revenue was €1,234.5 million, of which EMEA "
          "contributed 412 million. Headcount fell by 3 percent to 12,480. Operating cash flow: $0.9bn. Share 0.336.")  # fmt: skip


@pytest.mark.parametrize(("run", "expected"), [
    ("Margin reached 18.2%", ["green"]),
    ("Margin up from 15.1% to 18.2%", ["green", "green"]),
    ("Margin of about 18%", ["amber"]),
    ("Revenue of €1,234.5 million", ["green"]),
    ("Revenue of €1.2bn", ["amber"]),
    ("Revenue of 1234.5m", ["green"]),
    ("EMEA 412m", ["green"]),
    ("EMEA share was 33.6%", ["green"]),
    ("Headcount down 3%", ["green"]),
    ("Headcount now 12,480", ["green"]),
    ("Headcount now 12.5k", ["amber"]),
    ("Cash flow $0.9bn", ["green"]),
    ("Cash flow of $900m", ["green"]),
    ("Margin reached 19.4%", ["red"]),
    ("Revenue of €2.1bn", ["red"]),
    ("Headcount now 14,000", ["red"]),
    ("Margin fell 7 pts", ["red"]),
    ("Results for Q3 2026", []),
    ("FY26 outlook unchanged", []),
    ("No figures at all", []),
])  # fmt: skip
def test_number_check(run, expected):
    assert [v for _, v in check_run(run, [SOURCE])] == expected


def test_extract_numbers_normalizes():
    values = {n.text: n.value for n in extract_numbers("€1.2bn, 18%, 1,234 and −3.5 plus 2 thousand")}
    assert values["€1.2bn"] == 1.2e9 and values["18%"] == 18 and values["1,234"] == 1234
    assert values["−3.5"] == -3.5 and values["2 thousand"] == 2000


def test_numbers_rule_uses_cited_passage(kernel_for, project):
    ok = slide(1, "Title and Content", "Numbers ok", placeholders={"body": text("e-ok", {"runs": [{"t": "Margin reached 18.2%", "cite": [CITE]}]})})
    bad = slide(2, "Title and Content", "Numbers bad", placeholders={"body": text("e-bad", {"runs": [{"t": "Margin reached 21%", "cite": [CITE]}]})})
    gone = slide(3, "Title and Content", "Dangling", placeholders={"body": text("e-gone", {"runs": [{"t": "Up 4%", "cite": ["ref:r-none@000000000000#p1"]}]})})
    p = project([ok, bad, gone])
    k = kernel_for(p)
    k.dispatcher.call("deck.render", {"out": "exports/deck.pptx", "snapshot_id": "h-1"})
    from present_kernel.checker import check_deck
    from present_kernel.render import load_deck

    deck, docs = load_deck(p)
    diags = check_deck(p, p / "exports/deck.pptx", deck, docs, resolve_cite=lambda c: SOURCE if c == CITE else None)
    assert [(d["rule"], d["element_id"]) for d in diags if d["rule"].startswith("src.")] == [
        ("src.numbers", "e-bad"), ("src.dangling", "e-gone")]  # fmt: skip
    stale = json.loads(json.dumps(chart("e-ch", "bar")))
    stale["chart"]["source"] = {"kind": "ref", "cite": CITE}
    stale["chart"]["computed"] = SALES
    doc = slide(4, "Title and Content", "Stale chart", placeholders={"body": stale})
    p2 = make_project(Path(str(p) + "2"), "plain", [doc])
    kernel_for(p2).dispatcher.call("deck.render", {"out": "exports/deck.pptx", "snapshot_id": "h-1"})
    deck, docs = load_deck(p2)
    fresh = {"columns": SALES["columns"], "rows": [[*SALES["rows"][0][:1], 999, 80, 0.3], *SALES["rows"][1:]]}
    diags = check_deck(p2, p2 / "exports/deck.pptx", deck, docs, resolve_cite=lambda c: SOURCE, chart_values=lambda ch: fresh)
    assert [d["rule"] for d in diags if d["rule"] == "src.chart-stale"] == ["src.chart-stale"]
