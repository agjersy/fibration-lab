from __future__ import annotations

import io
import json
import zipfile
from pathlib import Path

import openpyxl
import pytest
from corpus import TEMPLATES, corpus_slides
from lxml import etree
from pptx import Presentation
from present_sdk import tags as T
from present_sdk.template import load_presentation

from present_kernel.errors import RpcError

GOLDEN = Path(__file__).parent / "golden"


def render(kernel_for, project, out="exports/deck.pptx", **params):
    return kernel_for(project).dispatcher.call("deck.render", {"out": out, "snapshot_id": "h-0000000001", **params})


def test_corpus_has_at_least_thirty_slides():
    assert len(corpus_slides()) >= 30


def test_validate_clean(kernel_for, corpus_project):
    diags = kernel_for(corpus_project).dispatcher.call("deck.validate", {})
    assert [d for d in diags if d["severity"] == "error"] == []


def test_render_reopens_and_tags_every_element(kernel_for, corpus_project):
    result = render(kernel_for, corpus_project)
    prs = Presentation(str(corpus_project / result["out"]))
    slides = list(prs.slides)
    deck = json.loads((corpus_project / "deck/deck.json").read_text())
    order = [sid for sec in deck["sections"] for sid in sec["slides"]]
    assert [T.read_slide_tags(s.part)["present.slide"] for s in slides] == order
    by_slide: dict[str, set[str]] = {}
    for slide in slides:
        sid = T.read_slide_tags(slide.part)["present.slide"]
        by_slide[sid] = {T.read_shape_tags(slide.part, sh._element).get("present.element", "") for sh in slide.shapes}
    for sid in order:
        doc = json.loads((corpus_project / f"deck/slides/{sid}.json").read_text())
        wanted = {item["id"] for item in doc["placeholders"].values() if item.get("type") != "empty"}
        wanted |= {el["id"] for el in doc.get("elements", [])}
        assert wanted <= by_slide[sid], f"{sid}: missing {wanted - by_slide[sid]}"
    assert {e["slide_id"] for e in result["element_map"]} == set(order)
    props = T.read_custom_properties(prs)
    assert props["present.deck_snapshot"] == "h-0000000001"
    assert props["present.mode"] == "structured"


def test_render_is_byte_deterministic(kernel_for, corpus_project):
    a = render(kernel_for, corpus_project, out="exports/a.pptx")
    b = render(kernel_for, corpus_project, out="exports/b.pptx")
    assert a["sha256"] == b["sha256"]
    assert (corpus_project / "exports/a.pptx").read_bytes() == (corpus_project / "exports/b.pptx").read_bytes()


def test_citations_are_written_as_tags(kernel_for, corp_project):
    result = render(kernel_for, corp_project)
    prs = Presentation(str(corp_project / result["out"]))
    slide = prs.slides[1]
    tags = {sh.name: T.read_shape_tags(slide.part, sh._element) for sh in slide.shapes}
    assert tags["e-b2"]["present.cite.0"].startswith("ref:r-q3rep@")


def test_chart_workbook_holds_the_values(kernel_for, corp_project):
    result = render(kernel_for, corp_project)
    prs = Presentation(str(corp_project / result["out"]))
    frame = next(sh for sh in prs.slides[3].shapes if sh.has_chart)
    wb = openpyxl.load_workbook(io.BytesIO(frame.chart.part.chart_workbook.xlsx_part.blob))
    rows = [list(r) for r in wb.active.iter_rows(values_only=True)]
    assert rows[0][1:] == ["Revenue", "Cost"]
    assert rows[1] == ["EMEA", 120.5, 80]
    assert frame.chart.plots[0].categories[0] == "EMEA"


def test_native_elements(kernel_for, corp_project):
    result = render(kernel_for, corp_project)
    prs = Presentation(str(corp_project / result["out"]))
    kinds = {e["kind"] for e in result["element_map"]}
    assert {"chart", "table", "image", "shape", "connector"} <= kinds
    hidden = [s for s in prs.slides if s._element.get("show") == "0"]
    assert len(hidden) == 1
    notes = prs.slides[1].notes_slide.notes_text_frame.text
    assert "Pause on the EMEA point." in notes
    table = next(sh for sh in prs.slides[12].shapes if sh.has_table).table
    assert table.cell(0, 0).text == "Region" and table.first_row
    assert table.cell(3, 0).is_merge_origin


def test_sections_and_furniture(kernel_for, corp_project):
    result = render(kernel_for, corp_project)
    with zipfile.ZipFile(corp_project / result["out"]) as z:
        pres = z.read("ppt/presentation.xml").decode()
        slide2 = z.read("ppt/slides/slide2.xml").decode()
    assert 'name="Part one"' in pres and 'name="Part two"' in pres
    assert "Corpus · Internal" in slide2
    assert 'type="sldNum"' in slide2


def test_partial_render(kernel_for, corp_project):
    result = render(kernel_for, corp_project, out="exports/part.pptx", slides=["s-c002", "s-c005"])
    prs = Presentation(str(corp_project / "exports/part.pptx"))
    assert len(prs.slides) == 2
    assert {e["slide_id"] for e in result["element_map"]} == {"s-c002", "s-c005"}


def test_unknown_layout_is_a_render_error(kernel_for, tmp_path):
    from corpus import make_project, slide

    project = make_project(tmp_path / "p", "plain", [slide(1, "No Such Layout", "x")])
    with pytest.raises(RpcError) as err:
        render(kernel_for, project)
    assert err.value.code == 2004 and err.value.extra["slide_id"] == "s-c001"


def test_wrong_content_for_placeholder_is_a_validation_error(kernel_for, tmp_path):
    from corpus import chart, make_project, slide

    doc = slide(1, "Section Header", "x", placeholders={"body": chart("e-x", "bar")})
    project = make_project(tmp_path / "p", "plain", [doc])
    diags = kernel_for(project).dispatcher.call("deck.validate", {})
    assert any("does not accept chart" in d["message"] for d in diags)


def _normalized_slide_xml(pptx_path: Path, index: int) -> str:
    prs = Presentation(str(pptx_path))
    root = etree.fromstring(etree.tostring(prs.slides[index]._element))
    for el in root.iter():
        for attr in list(el.attrib):
            if attr.endswith("}id") or attr.endswith("}embed") or attr.endswith("}link"):
                el.set(attr, "rId")
    return etree.tostring(root, pretty_print=True).decode()


@pytest.mark.parametrize("index", range(10))
def test_golden_slide_xml(kernel_for, corp_project, index, request):
    result = render(kernel_for, corp_project)
    xml = _normalized_slide_xml(corp_project / result["out"], index)
    path = GOLDEN / f"corp-slide-{index + 1:02d}.xml"
    if request.config.getoption("--update-golden") or not path.exists():
        path.write_text(xml, encoding="utf-8")
    assert xml == path.read_text(encoding="utf-8")


def test_potx_template_opens():
    prs = load_presentation(TEMPLATES["corp"])
    assert len(prs.slide_layouts) == 13
