"""The deck SDK on its own: what a code-mode build script relies on."""

from __future__ import annotations

import os
from pathlib import Path

import pptx
import pytest
from pptx import Presentation

REPO = Path(__file__).resolve().parents[3]
os.environ.setdefault("PRESENT_FONT_DIRS", str(REPO / "vendor" / "fonts"))

from present_sdk import ContentError, Deck, cite, theme  # noqa: E402
from present_sdk import tags as T  # noqa: E402
from present_sdk.measure import Measurer, Para  # noqa: E402
from present_sdk.template import describe_template  # noqa: E402

DEFAULT = Path(pptx.__file__).parent / "templates" / "default.pptx"


def build(tmp_path: Path) -> Deck:
    deck = Deck(DEFAULT, project=tmp_path, out=tmp_path / "out.pptx", snapshot="h-test")
    slide = deck.add_slide("Title and Content", slide_id="s-one")
    slide.title("A title")
    para = slide.placeholder("body").paragraph("Revenue was 120.5", level=0)
    cite(para.runs[0], "ref:r-x@000000000000#p1")
    slide.placeholder("body").paragraph("A second level", level=1)
    slide.notes("Speaker notes")
    chart = deck.add_slide("Title Only", slide_id="s-two")
    chart.title("A chart")
    chart.chart(type="column", box=chart.free_box(), category="Region",
                data={"columns": ["Region", "Revenue"], "rows": [["EMEA", 1.5], ["APAC", 2.5]], "loc": ""})
    return deck


def test_build_tags_and_reopen(tmp_path):
    emap = build(tmp_path).save()
    prs = Presentation(str(tmp_path / "out.pptx"))
    assert [T.read_slide_tags(s.part)["present.slide"] for s in prs.slides] == ["s-one", "s-two"]
    body = prs.slides[0].shapes[1]
    tags = T.read_shape_tags(prs.slides[0].part, body._element)
    assert tags["present.cite.0"] == "ref:r-x@000000000000#p1" and tags["present.element"].startswith("e-")
    assert {e["kind"] for e in emap} == {"shape", "chart"}
    assert T.read_custom_properties(prs)["present.deck_snapshot"] == "h-test"
    assert prs.slides[0].notes_slide.notes_text_frame.text == "Speaker notes"


def test_output_is_deterministic(tmp_path):
    a, _ = build(tmp_path).to_bytes()
    b, _ = build(tmp_path).to_bytes()
    assert a == b


def test_errors_name_what_is_available(tmp_path):
    deck = Deck(DEFAULT, project=tmp_path)
    with pytest.raises(ContentError, match="Title and Content"):
        deck.add_slide("No Such Layout")
    slide = deck.add_slide("Title Only")
    with pytest.raises(ContentError, match="has no placeholder 'body'"):
        slide.placeholder("body")
    with pytest.raises(ValueError):
        theme.color("#ff0000")
    assert theme.color("accent1") == "accent1" and theme.font("major") == "+mj-lt"


def test_template_description_and_measurement():
    desc = describe_template(DEFAULT)
    layout = next(lay for lay in desc["masters"][0]["layouts"] if lay["name"] == "Two Content")
    assert [p["key"] for p in layout["placeholders"]][:3] == ["title", "body", "body-2"]
    m = Measurer()
    short = m.measure([Para("Short", "Liberation Sans", 18)], 3_000_000, 400_000)
    long = m.measure([Para("Long text " * 40, "Liberation Sans", 18)], 3_000_000, 400_000)
    shrunk = m.measure([Para("Long text " * 6, "Liberation Sans", 18)], 3_000_000, 400_000, autofit="shrink")
    assert short.fits and not long.fits and shrunk.fits and shrunk.scale < 1
