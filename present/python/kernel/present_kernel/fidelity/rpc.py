"""RPC surface of the fidelity renderer: ``render.fidelity`` and ``template.thumbnails``."""

from __future__ import annotations

import shutil
from pathlib import Path
from typing import Any

from present_sdk import Deck
from present_sdk.template import FURNITURE_TYPES

from ..errors import ENGINE_UNAVAILABLE, NOT_FOUND, RENDER_FAILED, RpcError, invalid_params
from ..methods import group, hello_provider
from ..paths import rel_to_root
from .engines import EngineError, Engines, pdf_to_pngs, sha256_path


def engines_of(kernel) -> Engines:
    eng = getattr(kernel, "_engines", None)
    if eng is None:
        eng = Engines(kernel.home, kernel.engine_pref)
        kernel._engines = eng
        kernel.__dict__.setdefault("_closers", []).append(eng.close)
    return eng


def _wrap(exc: EngineError) -> RpcError:
    return RpcError(ENGINE_UNAVAILABLE if exc.unavailable else RENDER_FAILED, exc.detail)


def pdf_for(kernel, pptx: Path, engine: str = "auto") -> tuple[Path, str]:
    """PDF of a ``.pptx``, cached by content hash under ``.present/cache/render/``."""
    engines = engines_of(kernel)
    try:
        chosen = engines.choose(engine)
    except EngineError as exc:
        raise _wrap(exc) from exc
    digest = sha256_path(pptx)
    cache = kernel.project / ".present" / "cache" / "render" / digest[:16]
    for kind in ([chosen] if engine != "auto" else [chosen, "libreoffice"]):
        hit = cache / f"{kind}.pdf"
        if hit.is_file():
            return hit, kind
    tmp = cache / "pending.pdf"
    try:
        used = engines.to_pdf(pptx, tmp, engine)
    except EngineError as exc:
        raise _wrap(exc) from exc
    final = cache / f"{used}.pdf"
    tmp.replace(final)
    return final, used


@hello_provider
def _hello(kernel) -> dict[str, Any]:
    return {"engines": engines_of(kernel).info()}


@group
def _register(kernel) -> None:
    def render_fidelity(params: dict[str, Any]) -> dict[str, Any]:
        pptx = kernel.path(params.get("pptx") or "")
        if not pptx.is_file():
            raise RpcError(NOT_FOUND, f"{params.get('pptx')} not found")
        fmt = params.get("format", "png")
        if fmt not in ("png", "pdf"):
            raise invalid_params("format must be png or pdf")
        out_dir = kernel.path(params.get("out_dir") or ".present/cache/render/out")
        out_dir.mkdir(parents=True, exist_ok=True)
        pdf, used = pdf_for(kernel, pptx, params.get("engine", "auto"))
        if fmt == "pdf":
            target = out_dir / (pptx.stem + ".pdf")
            shutil.copyfile(pdf, target)
            return {"engine": used, "files": [rel_to_root(kernel.project, target)]}
        files = pdf_to_pngs(pdf, out_dir, int(params.get("dpi") or 96), params.get("slides"))
        return {"engine": used, "files": [rel_to_root(kernel.project, f) for f in files]}

    def template_thumbnails(params: dict[str, Any]) -> list[dict[str, str]]:
        rel = params.get("path") or ""
        template = kernel.path(rel)
        if not template.is_file():
            raise RpcError(NOT_FOUND, f"{rel} not found")
        digest = sha256_path(template)[:16]
        out_dir = kernel.project / ".present" / "cache" / "thumbs" / digest
        deck = Deck(template, project=kernel.project)
        names = []
        for master in deck.description["masters"]:
            for layout in master["layouts"]:
                slide = deck.add_slide(layout["name"])
                names.append(layout["name"])
                for ph in layout["placeholders"]:
                    if ph["type"] in FURNITURE_TYPES or "text" not in ph["accepts"]:
                        continue
                    label = layout["name"] if ph["type"] in ("title", "ctrTitle") else ph["key"]
                    slide.placeholder(ph["key"]).set_text(label)
        out_dir.mkdir(parents=True, exist_ok=True)
        pptx = out_dir / "layouts.pptx"
        deck.save(pptx)
        pdf, _ = pdf_for(kernel, pptx)
        files = pdf_to_pngs(pdf, out_dir, int(params.get("dpi") or 48), stem="layout")
        return [{"layout": n, "png": rel_to_root(kernel.project, f)} for n, f in zip(names, files)]

    kernel.dispatcher.register("render.fidelity", render_fidelity)
    kernel.dispatcher.register("template.thumbnails", template_thumbnails)
