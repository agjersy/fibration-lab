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
from present_sdk.omml import A14, MC, A, M, OmmlError, P, R, latex_to_omath  # noqa: E402

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
    """The equations of a file re-opened with python-pptx (they sit in mc:AlternateContent, which
    python-pptx's shape list does not show, so the slide XML is searched)."""
    prs = Presentation(str(path))
    return [el for s in prs.slides for el in s._element.iter(f"{{{M}}}oMath")]


def _skeleton(el, depth=0) -> list[str]:
    """Element names and the attributes that matter, down to the OMML (whose inside the
    fraction golden covers); the picture's relationship id and the shape ids are normalised."""
    q = etree.QName(el)
    prefix = {MC: "mc", A14: "a14", M: "m", P: "p", A: "a"}.get(q.namespace, q.namespace)
    keep = {k: v for k, v in el.attrib.items() if etree.QName(k).localname in ("Requires", "prst", "algn", "val", "rotWithShape", "txBox", "idx")}
    if any(etree.QName(k).localname == "embed" for k in el.attrib):
        keep["r:embed"] = "<image>"
    attrs = "".join(f" {etree.QName(k).localname if not k.startswith('r:') else k}={v}" for k, v in sorted(keep.items()))
    line = "  " * depth + f"{prefix}:{q.localname}{attrs}"
    if q.localname == "oMath":
        return [line + " ..."]
    if q.localname in ("custDataLst", "rPr", "endParaRPr", "lstStyle", "avLst", "off", "ext", "fillRect"):
        return [line]
    return [line] + [x for child in el for x in _skeleton(child, depth + 1)]


def _alternate(path: Path):
    prs = Presentation(str(path))
    found = [el for s in prs.slides for el in s.shapes._spTree.iterfind(f"{{{MC}}}AlternateContent")]
    return prs, found


def _equation_deck(tmp_path: Path) -> Path:
    deck = Deck(DEFAULT, project=tmp_path, out=tmp_path / "out.pptx", snapshot="h")
    slide = deck.add_slide("Title and Content", slide_id="s-eq")
    slide.title("The model")
    slide.fill("body", {"type": "equation", "id": "e-eq1", "eq_cite": {
        "latex": r"\frac{a^{2}}{b + 1} e^{- \frac{t}{\tau_{1}}}", "tier": "derived",
        "check": {"status": "agree", "trials": 32}, "qualname": "decay", "path": "lib/m.py", "source_hash": "00"}})
    free = deck.add_slide("Title Only", slide_id="s-eq2")
    free.equation(r"\sqrt{x^{2} + 1}", box=free.free_box(), element_id="e-eq2")
    deck.save()
    assert not deck.warnings
    return tmp_path / "out.pptx"


def test_an_equation_is_stored_the_way_powerpoint_stores_one(tmp_path):
    """The whole shape in mc:AlternateContent: an a14 choice (the namespace declared there) whose
    shape holds a14:m with the OMML, and a fallback copy of the shape filled with a picture of
    the equation drawn by Matplotlib's mathtext."""
    path = _equation_deck(tmp_path)
    prs, found = _alternate(path)
    assert len(found) == 2
    ac = found[0]
    choice = ac.find(f"{{{MC}}}Choice")
    assert choice.get("Requires") == "a14" and choice.nsmap.get("a14") == A14
    got = "\n".join(_skeleton(ac)) + "\n"
    golden = GOLDEN / "equation_shape.txt"
    if os.environ.get("PRESENT_UPDATE_GOLDEN") == "1":
        golden.write_text(got, encoding="utf-8")
    assert got == golden.read_text(encoding="utf-8")
    # The fallback picture is an image part of the slide, a PNG drawn by Matplotlib.
    slide = prs.slides[0]
    rid = ac.find(f".//{{{A}}}blip").get(f"{{{R}}}embed")
    part = slide.part.related_part(rid)
    assert part.content_type == "image/png" and part.blob.startswith(b"\x89PNG")
    # The fallback carries the choice's id, name, alt text and element tags.
    from present_sdk import tags as T

    shapes = [ac.find(f"{{{MC}}}{w}")[0] for w in ("Choice", "Fallback")]
    ids = {(c.get("id"), c.get("name"), c.get("descr")) for sp in shapes for c in sp.iter(f"{{{P}}}cNvPr")}
    assert len(ids) == 1 and next(iter(ids))[1] == "e-eq1"
    assert [T.read_shape_tags(slide.part, sp).get("present.element") for sp in shapes] == ["e-eq1", "e-eq1"]


def test_libreoffice_exports_an_equation_deck_to_pdf(tmp_path):
    """LibreOffice opens the file without complaint and exports every slide, drawing the
    fallback picture. Skipped where LibreOffice is not installed."""
    import shutil
    import subprocess

    soffice = shutil.which("soffice") or shutil.which("libreoffice")
    if soffice is None:
        pytest.skip("LibreOffice is not installed")
    path = _equation_deck(tmp_path)
    profile = tmp_path / "lo-profile"
    res = subprocess.run(
        [soffice, f"-env:UserInstallation=file://{profile}", "--headless", "--convert-to", "pdf",
         "--outdir", str(tmp_path / "pdf"), str(path)],
        capture_output=True, text=True, timeout=240,
    )
    pdf = tmp_path / "pdf" / "out.pdf"
    assert res.returncode == 0 and pdf.is_file(), res.stdout + res.stderr
    out = (res.stdout + res.stderr).lower()
    assert "error" not in out and "corrupt" not in out and "damaged" not in out, out
    data = pdf.read_bytes()
    assert data.count(b"/Type/Page") - data.count(b"/Type/Pages") == 2 or data.count(b"/Type /Page") - data.count(b"/Type /Pages") == 2
    assert b"/Subtype/Image" in data or b"/Subtype /Image" in data


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
