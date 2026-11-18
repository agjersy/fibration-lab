"""LaTeX to native PowerPoint equations (OMML): the converter and the SDK's equation element."""

from __future__ import annotations

import os
from pathlib import Path

import pptx
import pytest
from lxml import etree
from pptx import Presentation

REPO = Path(__file__).resolve().parents[3]
os.environ.setdefault("PRESENT_FONT_DIRS", str(REPO / "vendor" / "fonts"))

from present_sdk import Deck  # noqa: E402
from present_sdk.omml import M, OmmlError, latex_to_omath  # noqa: E402

DEFAULT = Path(pptx.__file__).parent / "templates" / "default.pptx"
GOLDEN = Path(__file__).parent / "golden"
NS = {"m": M}


def _shape(latex: str) -> str:
    """The OMML structure without run properties: element names and text, one per line."""
    out: list[str] = []

    def walk(el, depth):
        q = etree.QName(el)
        if q.namespace != M or q.localname == "rPr":
            return
        val = el.get(f"{{{M}}}val")
        text = el.text if q.localname == "t" else None
        out.append("  " * depth + q.localname + (f" val={val}" if val else "") + (f" {text!r}" if text else ""))
        for child in el:
            walk(child, depth + 1)

    walk(latex_to_omath(latex), 0)
    return "\n".join(out) + "\n"


def test_a_fraction_with_an_exponent_matches_the_golden():
    got = _shape(r"\frac{a^{2}}{b + 1} e^{- \frac{t}{\tau_{1}}}")
    golden = GOLDEN / "omml_fraction_exponent.txt"
    if os.environ.get("PRESENT_UPDATE_GOLDEN") == "1":
        golden.write_text(got, encoding="utf-8")
    assert got == golden.read_text(encoding="utf-8")


@pytest.mark.parametrize("latex,tags", [
    (r"\sqrt{x^{2} + 1}", {"rad", "sSup"}),
    (r"\sqrt[3]{x}", {"rad", "deg"}),
    (r"\sum_{i=0}^{n} i^{2}", {"nary", "sSup"}),
    (r"\prod_{k=1}^{N} k", {"nary"}),
    (r"\int\limits_{0}^{\infty} e^{- x}\, dx", {"nary", "sSup"}),
    (r"\exp{\left(- \frac{t}{\tau} \right)}", {"func", "d", "f"}),
    (r"\log{\left(x \right)} + \sin^{2}{\left(\theta \right)}", {"func", "sSup", "d"}),
    (r"A e^{- \frac{t}{\tau_{1}}} + B e^{- \frac{t}{\tau_{2}}}", {"sSup", "sSub", "f"}),
    (r"x_{i}^{2} \cdot \alpha \leq \pi", {"sSubSup"}),
    (r"\left|x\right| \geq \hat{\beta}", {"d", "acc"}),
    (r"\operatorname{erf}{\left(x \right)}", {"func"}),
])
def test_the_sympy_subset_converts(latex, tags):
    omath = latex_to_omath(latex)
    found = {etree.QName(e).localname for e in omath.iter()}
    assert tags <= found, (latex, found)


def test_greek_and_operators_are_unicode():
    texts = [t.text for t in latex_to_omath(r"\alpha - \Omega \times 2").iter(f"{{{M}}}t")]
    assert texts == ["α", "−", "Ω", "×", "2"]


@pytest.mark.parametrize("latex", [r"\begin{matrix} a & b \end{matrix}", r"\frac{a}", r"\unknowncommand x", ""])
def test_latex_outside_the_subset_is_refused(latex):
    with pytest.raises(OmmlError):
        latex_to_omath(latex)


def _equations(path: Path):
    prs = Presentation(str(path))
    return [el for s in prs.slides for sh in s.shapes for el in sh._element.iter(f"{{{M}}}oMath")]


def test_an_equation_element_exports_as_a_native_equation(tmp_path):
    deck = Deck(DEFAULT, project=tmp_path, out=tmp_path / "out.pptx", snapshot="h")
    slide = deck.add_slide("Title and Content", slide_id="s-eq")
    slide.title("The model")
    slide.fill("body", {"type": "equation", "id": "e-eq1", "eq_cite": {
        "latex": r"\frac{a^{2}}{b}", "tier": "derived", "check": {"status": "unavailable", "trials": 0},
        "qualname": "ratio", "path": "lib/m.py", "source_hash": "00"}})
    emap = deck.save()
    found = _equations(tmp_path / "out.pptx")
    assert len(found) == 1 and found[0].find(".//m:f", NS) is not None
    assert any(e["element_id"] == "e-eq1" for e in emap)
    assert not deck.warnings


def test_unconvertible_latex_falls_back_and_says_so(tmp_path):
    deck = Deck(DEFAULT, project=tmp_path, out=tmp_path / "out.pptx", snapshot="h")
    slide = deck.add_slide("Title Only", slide_id="s-eq")
    slide.equation(r"\begin{matrix} a & b \end{matrix}", box=slide.free_box(), element_id="e-eq2")
    deck.save()
    assert _equations(tmp_path / "out.pptx") == []
    assert [w["rule"] for w in deck.warnings] == ["eq.omml-fallback"]
    assert deck.warnings[0]["element_id"] == "e-eq2"
