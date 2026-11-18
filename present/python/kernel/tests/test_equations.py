"""Equation elements: native OMML in the exported file, the image fallback, and the checker rules."""

from __future__ import annotations

from pathlib import Path

import pytest
from corpus import make_project, slide
from pptx import Presentation

M = "http://schemas.openxmlformats.org/officeDocument/2006/math"


def eq(eid, latex, tier="declared", status="agree", **extra):
    check = {"status": status, "trials": 32, "tol": 1e-6}
    if status == "mismatch":
        check["counterexample"] = {"inputs": {"t": 1.0}, "expected": 2.0, "actual": 3.0}
    return {"type": "equation", "id": eid, "eq_cite": {
        "latex": latex, "tier": tier, "check": check, "qualname": "decay", "path": "lib/model.py",
        "source_hash": "0" * 64}, **extra}  # fmt: skip


@pytest.fixture()
def render(kernel_for, tmp_path):
    def go(slides):
        p = make_project(tmp_path / "p", "plain", slides, None)
        k = kernel_for(p)
        out = k.dispatcher.call("deck.render", {"out": "exports/deck.pptx", "snapshot_id": "h-1"})
        diags = k.dispatcher.call("check.run", {"pptx": "exports/deck.pptx", "engine": False})
        return p / "exports" / "deck.pptx", out, diags, k

    return go


def test_an_equation_exports_as_an_editable_powerpoint_equation(render):
    path, out, diags, k = render([slide(1, "Title and Content", "The decay model",
                                        placeholders={"body": eq("e-eq1", r"A e^{- \frac{t}{\tau}}")})])
    prs = Presentation(str(path))
    maths = [m for s in prs.slides for sh in s.shapes for m in sh._element.iter(f"{{{M}}}oMath")]
    assert len(maths) == 1
    assert maths[0].find(f".//{{{M}}}f") is not None and maths[0].find(f".//{{{M}}}sSup") is not None
    assert not [w for w in out["warnings"] if w["rule"] == "eq.omml-fallback"]
    assert not [d for d in diags if d["rule"].startswith("eq.")]
    assert k.dispatcher.call("deck.validate", {}) == [] or all(
        d["rule"] != "schema" for d in k.dispatcher.call("deck.validate", {}))


def test_unconvertible_latex_falls_back_to_an_image_with_alt_text(render):
    # `\mathcal` is outside the converter's subset; Matplotlib's mathtext draws it.
    path, out, diags, _ = render([slide(1, "Title and Content", "A loss",
                                        placeholders={"body": eq("e-eq2", r"\mathcal{L} = x^{2}")})])
    prs = Presentation(str(path))
    pics = [sh for s in prs.slides for sh in s.shapes if sh._element.tag.endswith("}pic")]
    assert len(pics) == 1 and "mathcal" in pics[0]._element.xpath(".//p:cNvPr/@descr")[0]
    assert [w["element_id"] for w in out["warnings"] if w["rule"] == "eq.omml-fallback"] == ["e-eq2"]
    assert [d["element_id"] for d in diags if d["rule"] == "eq.omml-fallback"] == ["e-eq2"]


def test_a_mismatch_or_latex_only_equation_is_an_error_until_acknowledged(render):
    _, _, diags, _ = render([
        slide(1, "Title and Content", "Mismatch", placeholders={"body": eq("e-m", "x^{2}", status="mismatch")}),
        slide(2, "Title and Content", "Display only", placeholders={"body": eq("e-l", "x", tier="latex", status="unavailable")}),
        slide(3, "Title and Content", "Acknowledged", placeholders={"body": eq("e-a", "x^{2}", status="mismatch", acknowledged=True)}),
    ])
    found = {(d["rule"], d["element_id"]): d["severity"] for d in diags if d["rule"] in ("eq.mismatch", "eq.latex-only")}
    assert found == {("eq.mismatch", "e-m"): "error", ("eq.latex-only", "e-l"): "error", ("eq.mismatch", "e-a"): "info"}
    mismatch = next(d for d in diags if d["rule"] == "eq.mismatch" and d["element_id"] == "e-m")
    assert "t=1" in mismatch["message"] and mismatch["pointer"] == "/placeholders/body"


def test_the_outline_export_lists_the_equation(render, tmp_path):
    path, _, _, _k = render([slide(1, "Title and Content", "Model", placeholders={"body": eq("e-eq1", r"\frac{a}{b}")})])
    from present_kernel.exports import _slide_parts

    _, _, extras, _ = _slide_parts({"placeholders": {"body": eq("e-eq1", r"\frac{a}{b}")}})
    assert extras == [r"Equation: $\frac{a}{b}$ (decay in lib/model.py)"]
    assert Path(path).is_file()


def bound(eid, text, run_id="r1", name="margin"):
    return {"type": "text", "id": eid, "text": {"paragraphs": [{"runs": [
        {"t": "Margin was "}, {"t": text, "run_cite": {"run_id": run_id, "result_name": name, "script": "scripts/m.py", "args": []}},
    ]}]}}  # fmt: skip


def test_a_bound_number_must_show_the_recorded_value(kernel_for, tmp_path):
    slides = [
        slide(1, "Title and Content", "Percent", placeholders={"body": bound("e-p", "18.3%")}),
        slide(2, "Title and Content", "Rounded", placeholders={"body": bound("e-r", "0.18")}),
        slide(3, "Title and Content", "Wrong", placeholders={"body": bound("e-w", "21%")}),
        slide(4, "Title and Content", "Missing", placeholders={"body": bound("e-m", "5", name="gone")}),
    ]
    p = make_project(tmp_path / "p", "plain", slides, None)
    k = kernel_for(p)
    k.dispatcher.call("deck.render", {"out": "exports/deck.pptx", "snapshot_id": "h-1"})
    results = {"r1/margin": {"name": "margin", "value": 0.18312}, "r1/gone": None}
    diags = k.dispatcher.call("check.run", {"pptx": "exports/deck.pptx", "engine": False, "results": results})
    found = {d["element_id"]: d["severity"] for d in diags if d["rule"] == "num.bound"}
    assert found == {"e-w": "error", "e-m": "warn"}, diags
    wrong = next(d for d in diags if d["rule"] == "num.bound" and d["element_id"] == "e-w")
    assert wrong["pointer"] == "/placeholders/body/text/paragraphs/0/runs/1"
    # A bound number counts as cited; without results (no session) nothing is compared.
    assert not [d for d in diags if d["rule"] == "src.coverage"]
    plain = k.dispatcher.call("check.run", {"pptx": "exports/deck.pptx", "engine": False})
    assert not [d for d in plain if d["rule"] == "num.bound"]
