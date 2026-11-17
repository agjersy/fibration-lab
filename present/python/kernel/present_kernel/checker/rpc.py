"""RPC surface of the checker: ``check.run`` and ``text.measure``."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from present_sdk.measure import Measurer, Para

from ..errors import NOT_FOUND, RpcError, invalid_params
from ..methods import group
from ..render import load_deck, load_sidecar
from .opens import validate_parts
from .rules import check_deck


def _resolver(kernel):
    try:
        from ..refs.reader import ProjectRefs
    except ImportError:
        return None, None
    refs = ProjectRefs(kernel.project)

    def resolve(cite: str) -> str | None:
        try:
            return refs.text(cite)
        except Exception:
            try:
                table = refs.table(cite)
            except Exception:
                return None
            cells = [str(c) for c in table.get("columns", [])]
            cells += [str(v) for row in table.get("rows", []) for v in row if v is not None]
            return " | ".join(cells)

    def chart_values(chart: dict[str, Any]) -> dict[str, Any] | None:
        try:
            from ..transforms import apply_steps

            table = refs.table(chart["source"]["cite"])
            return apply_steps(table, chart.get("steps") or [])
        except Exception:
            return None

    return resolve, chart_values


def run_check(kernel, pptx_rel: str, deck_rel: str | None, slides: list[str] | None, rules: list[str] | None,
              with_engine: bool = True) -> list[dict[str, Any]]:
    pptx = kernel.path(pptx_rel)
    if not pptx.is_file():
        raise RpcError(NOT_FOUND, f"{pptx_rel} not found")
    deck = docs = sidecar = None
    deck_path = kernel.path(deck_rel or "deck/deck.json")
    if deck_path.is_file():
        deck, docs = load_deck(kernel.project, deck_rel or "deck/deck.json")
        sidecar = load_sidecar(kernel.project, deck["template"])
        if deck.get("mode") == "code":
            docs = None
    resolve, chart_values = _resolver(kernel)

    def opens_clean(path: Path, visible: int) -> list[str]:
        problems = validate_parts(path)
        if with_engine and (rules is None or "out.opens-clean" in rules):
            try:
                from ..fidelity.rpc import pdf_for

                pdf, _ = pdf_for(kernel, path)
                import pypdfium2 as pdfium

                doc = pdfium.PdfDocument(str(pdf))
                pages = len(doc)
                doc.close()
                if pages != visible:
                    problems.append(f"the fidelity engine produced {pages} pages for {visible} visible slides")
            except RpcError as exc:
                if exc.code != 2008:  # no engine installed is not a defect of the file
                    problems.append(f"the fidelity engine could not convert the file: {exc.detail}")
        return problems

    return check_deck(kernel.project, pptx, deck, docs, sidecar, slides, rules, resolve, chart_values, opens_clean)


@group
def _register(kernel) -> None:
    def check_run(params: dict[str, Any]) -> list[dict[str, Any]]:
        pptx = params.get("pptx")
        if not isinstance(pptx, str):
            raise invalid_params("pptx is required")
        return run_check(kernel, pptx, params.get("deck"), params.get("slides"), params.get("rules"),
                         bool(params.get("engine", True)))  # fmt: skip

    def text_measure(params: dict[str, Any]) -> dict[str, Any]:
        try:
            family, size = params["font_family"], float(params["size_pt"])
            box = params["box"]
            paragraphs = params["paragraphs"]
        except (KeyError, TypeError, ValueError) as exc:
            raise invalid_params(f"text.measure: {exc}") from exc
        paras = [Para(str(t), family, size, bool(params.get("bold")), bool(params.get("italic")),
                      line_spacing=float(params.get("line_spacing") or 1.0)) for t in paragraphs]  # fmt: skip
        m = Measurer().measure(paras, int(box["cx"]), int(box["cy"]), params.get("insets"), params.get("autofit", "none"))
        out = {"fits": m.fits, "lines": m.lines, "height": m.height, "font_found": m.font_found}
        if m.shrink_to_pt is not None:
            out["shrink_to_pt"] = m.shrink_to_pt
        return out

    kernel.dispatcher.register("check.run", check_run)
    kernel.dispatcher.register("text.measure", text_measure)
