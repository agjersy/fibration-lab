"""W7 acceptance: the checker's fits/overflow verdict against LibreOffice's actual layout."""

from __future__ import annotations

import json
import shutil
from pathlib import Path

import pypdfium2 as pdfium
import pytest
from corpus import make_project, slide, text
from present_sdk.template import describe_template, layout_info, resolve_placeholder_info

pytestmark = pytest.mark.skipif(shutil.which("soffice") is None, reason="LibreOffice is not installed")

WORDS = ["revenue", "margin", "growth", "across", "regions", "improved", "steadily", "while", "costs", "remained", "under", "control", "and", "the", "pipeline", "expanded", "with", "several", "large", "enterprise", "accounts", "closing", "late", "in", "the", "quarter"]  # fmt: skip


def _cases():
    cases = []
    # Short single-line paragraphs: 12..31 lines around the box capacity.
    for n in range(12, 32):
        cases.append([f"Point {i + 1} about {WORDS[i % len(WORDS)]}" for i in range(n)])
    # Wrapping paragraphs of varying length.
    for n in range(2, 22):
        cases.append([" ".join(WORDS[(i + j) % len(WORDS)] for j in range(18 + (i * 7) % 23)) for i in range(n)])
    return cases


def test_overflow_verdicts_agree_with_libreoffice(kernel_for, tmp_path):
    cases = _cases()
    assert len(cases) == 40
    docs = [slide(i + 1, "Title and Content", f"Case {i + 1}", placeholders={"body": text(f"e-case{i + 1:02d}", *paras)})
            for i, paras in enumerate(cases)]  # fmt: skip
    project = make_project(tmp_path / "p", "wide-dense", docs, {"footer": None, "classification": None})
    (project / ".present" / "project.toml").write_text('[contract]\nmax_bullets = 999\nmax_words = 99999\n')
    k = kernel_for(project)
    k.dispatcher.call("deck.render", {"out": "exports/deck.pptx", "snapshot_id": "h-1"})
    diags = k.dispatcher.call("check.run", {"pptx": "exports/deck.pptx", "rules": ["fit.overflow"], "engine": False})
    ours = {d["element_id"] for d in diags}
    pdf_rel = k.dispatcher.call("render.fidelity", {"pptx": "exports/deck.pptx", "format": "pdf", "out_dir": "exports", "engine": "libreoffice"})["files"][0]

    desc = describe_template(project / "template" / "wide-dense.pptx")
    body = resolve_placeholder_info(layout_info(desc, "Title and Content"), "body")
    slide_h = desc["slide_size"]["cy"]
    doc = pdfium.PdfDocument(str(project / pdf_rel))
    assert len(doc) == 40
    scale = 2.0
    misses = []
    rows = []
    for i in range(40):
        image = doc[i].render(scale=scale).to_pil().convert("L")
        w, h = image.size
        bottom = (body["y"] + body["cy"]) / slide_h * h
        x0, x1 = int(body["x"] / desc["slide_size"]["cx"] * w), int((body["x"] + body["cx"]) / desc["slide_size"]["cx"] * w)
        px = image.load()
        lowest = 0
        for y in range(h - 1, int(body["y"] / slide_h * h), -1):
            if any(px[x, y] < 128 for x in range(x0, x1, 2)):
                lowest = y
                break
        theirs = lowest > bottom + 2
        mine = f"e-case{i + 1:02d}" in ours
        rows.append({"case": i + 1, "checker_overflow": mine, "libreoffice_overflow": theirs, "ink_px_past_bottom": round(lowest - bottom, 1)})
        if mine != theirs:
            misses.append(rows[-1])
    doc.close()
    report = Path(__file__).resolve().parents[3] / "docs" / "notes" / "overflow-accuracy.json"
    if report.parent.is_dir():
        report.write_text(json.dumps({"agree": 40 - len(misses), "of": 40, "misses": misses}, indent=1), encoding="utf-8")
    assert len(misses) <= 2, misses
