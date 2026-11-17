from __future__ import annotations

from typing import Any

from ..methods import group
from . import notes_docx, outline_markdown


@group
def _register(kernel) -> None:
    def outline_md(params: dict[str, Any]) -> dict[str, Any]:
        out = kernel.path(params["out"])
        out.parent.mkdir(parents=True, exist_ok=True)
        text = outline_markdown(kernel.project, params.get("deck") or "deck/deck.json", bool(params.get("include_sources", True)))
        out.write_text(text, encoding="utf-8")
        return {"out": params["out"]}

    def docx(params: dict[str, Any]) -> dict[str, Any]:
        notes_docx(kernel.project, params.get("deck") or "deck/deck.json", kernel.path(params["out"]),
                   bool(params.get("include_sources", True)))  # fmt: skip
        return {"out": params["out"]}

    kernel.dispatcher.register("export.outline_md", outline_md)
    kernel.dispatcher.register("export.notes_docx", docx)
