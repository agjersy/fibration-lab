"""Structured-mode rendering: deck document -> ``.pptx`` through the deck SDK (PROTOCOL.md 1.3, 1.4)."""

from __future__ import annotations

import json
import tomllib
from pathlib import Path
from typing import Any

from present_sdk import ContentError, Deck

from .errors import NOT_FOUND, RENDER_FAILED, RpcError
from .paths import resolve_in_root


def load_deck(project: Path, deck_rel: str = "deck/deck.json") -> tuple[dict[str, Any], dict[str, dict[str, Any]]]:
    deck_path = resolve_in_root(project, deck_rel)
    if not deck_path.is_file():
        raise RpcError(NOT_FOUND, f"{deck_rel} not found")
    deck = json.loads(deck_path.read_text(encoding="utf-8"))
    slides: dict[str, dict[str, Any]] = {}
    for section in deck.get("sections", []):
        for sid in section.get("slides", []):
            path = deck_path.parent / "slides" / f"{sid}.json"
            if not path.is_file():
                raise RpcError(NOT_FOUND, f"slide {sid} is listed in deck.json but deck/slides/{sid}.json is missing")
            slides[sid] = json.loads(path.read_text(encoding="utf-8"))
    return deck, slides


def load_sidecar(project: Path, template_rel: str) -> dict[str, Any] | None:
    side = resolve_in_root(project, template_rel).parent / "template.toml"
    return tomllib.loads(side.read_text(encoding="utf-8")) if side.is_file() else None


def footer_text(deck: dict[str, Any]) -> str | None:
    parts = [p for p in ((deck.get("footer") or {}).get("text"), deck.get("classification")) if p]
    return " · ".join(parts) if parts else None


def build_slide(sdk_deck: Deck, deck: dict[str, Any], doc: dict[str, Any], section: str | None,
                generated: Any = None) -> None:
    slide = sdk_deck.add_slide(doc["layout"], doc["id"], section)
    if doc.get("kind") == "generated":
        if generated is None:
            raise ContentError("generated slides are run by the sandboxed runner, not the renderer")
        generated(slide, doc)
    else:
        for key, item in (doc.get("placeholders") or {}).items():
            slide.fill(key, item)
        for element in doc.get("elements") or []:
            slide.free(element)
    if doc.get("notes"):
        slide.notes(doc["notes"])
    if doc.get("hidden"):
        slide.raw._element.set("show", "0")
    footer = deck.get("footer") or {}
    text = footer_text(deck)
    if text or footer.get("slide_numbers") or footer.get("date"):
        slide.furniture(text, bool(footer.get("slide_numbers")), bool(footer.get("date")))


def open_sdk_deck(project: Path, deck: dict[str, Any], out: Path | None, snapshot_id: str) -> Deck:
    template = resolve_in_root(project, deck["template"])
    if not template.is_file():
        raise RpcError(NOT_FOUND, f"template {deck['template']} not found")
    sdk_deck = Deck(template, project=project, out=out, snapshot=snapshot_id, sidecar=load_sidecar(project, deck["template"]))
    sdk_deck.mode = "structured"
    sdk_deck.properties["Title"] = deck.get("title", "")
    if deck.get("author"):
        sdk_deck.properties["Author"] = deck["author"]
    for k, v in (deck.get("properties") or {}).items():
        sdk_deck.properties[k] = v
    return sdk_deck


def render_deck(project: Path, out_rel: str, snapshot_id: str, only: list[str] | None = None,
                deck_rel: str = "deck/deck.json", generated: Any = None) -> dict[str, Any]:
    """Render the deck document. ``only`` renders a subset of slides (for partial thumbnails)."""
    deck, slides = load_deck(project, deck_rel)
    out = resolve_in_root(project, out_rel)
    sdk_deck = open_sdk_deck(project, deck, out, snapshot_id)
    multi = len([s for s in deck.get("sections", []) if s.get("title")]) > 0
    for section in deck.get("sections", []):
        for sid in section.get("slides", []):
            if only is not None and sid not in only:
                continue
            doc = slides[sid]
            try:
                build_slide(sdk_deck, deck, doc, section.get("title", "") if multi else None, generated)
            except ContentError as exc:
                raise RpcError(RENDER_FAILED, f"slide {sid}: {exc}", slide_id=sid, element_id=exc.element_id) from exc
            except (KeyError, TypeError, ValueError) as exc:
                raise RpcError(RENDER_FAILED, f"slide {sid}: {type(exc).__name__}: {exc}", slide_id=sid) from exc
    data, emap = sdk_deck.to_bytes()
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(data)
    import hashlib

    return {
        "out": out_rel, "sha256": hashlib.sha256(data).hexdigest(), "element_map": emap,
        "warnings": sdk_deck.warnings,
    }  # fmt: skip
