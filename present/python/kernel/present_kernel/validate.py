"""``deck.validate``: JSON Schema plus cross-references to layouts, assets, and slides."""

from __future__ import annotations

import functools
import json
from pathlib import Path
from typing import Any

import jsonschema
from present_sdk.template import FURNITURE_TYPES, describe_template, layout_info, resolve_placeholder_info

from .paths import resolve_in_root
from .render import load_deck, load_sidecar


def _schema_dir() -> Path:
    here = Path(__file__).resolve()
    packaged = here.parent / "schemas"
    if packaged.is_dir():
        return packaged
    return here.parents[3] / "schemas"  # development checkout: <repo>/schemas


@functools.lru_cache(maxsize=4)
def _validator(name: str):
    path = _schema_dir() / name
    if not path.is_file():
        return None
    schema = json.loads(path.read_text(encoding="utf-8"))
    return jsonschema.Draft7Validator(schema)


def _schema_diags(name: str, doc: Any, slide_id: str | None) -> list[dict[str, Any]]:
    validator = _validator(name)
    if validator is None:
        return []
    out = []
    for err in sorted(validator.iter_errors(doc), key=lambda e: list(e.absolute_path)):
        pointer = "/" + "/".join(str(p) for p in err.absolute_path)
        diag = {"rule": "schema", "severity": "error", "message": err.message[:300], "pointer": pointer}
        if slide_id:
            diag["slide_id"] = slide_id
        out.append(diag)
    return out


def validate_deck(project: Path, deck_rel: str = "deck/deck.json") -> list[dict[str, Any]]:
    deck, slides = load_deck(project, deck_rel)
    diags = _schema_diags("deck.schema.json", deck, None)
    template = resolve_in_root(project, deck.get("template", ""))
    description = None
    if template.is_file():
        description = describe_template(template, deck["template"], load_sidecar(project, deck["template"]))
    else:
        diags.append({"rule": "schema", "severity": "error", "message": f"template {deck.get('template')!r} not found"})
    seen: set[str] = set()
    for sid, doc in slides.items():
        diags += _schema_diags("slide.schema.json", doc, sid)
        if doc.get("id") != sid:
            diags.append({"rule": "schema", "severity": "error", "slide_id": sid,
                          "message": f"file is named {sid}.json but the slide id is {doc.get('id')!r}"})  # fmt: skip
        if description is None:
            continue
        try:
            layout = layout_info(description, doc.get("layout", ""))
        except KeyError:
            diags.append({"rule": "tpl.layout-only", "severity": "error", "slide_id": sid, "pointer": "/layout",
                          "message": f"layout {doc.get('layout')!r} is not in the template",
                          "fix_hint": "choose a layout from template.layouts"})  # fmt: skip
            continue
        for key, item in (doc.get("placeholders") or {}).items():
            try:
                info = resolve_placeholder_info(layout, key)
            except KeyError:
                keys = [p["key"] for p in layout["placeholders"] if p["type"] not in FURNITURE_TYPES]
                diags.append({"rule": "schema", "severity": "error", "slide_id": sid, "pointer": f"/placeholders/{key}",
                              "message": f"layout {layout['name']!r} has no placeholder {key!r}",
                              "fix_hint": f"available: {', '.join(keys)}"})  # fmt: skip
                continue
            kind = item.get("type")
            # An equation is a text box with a native equation (or an image when it falls back).
            fits = kind == "equation" and ("text" in info["accepts"] or "image" in info["accepts"])
            if kind not in (None, "empty") and kind not in info["accepts"] and not fits:
                diags.append({"rule": "schema", "severity": "error", "slide_id": sid, "element_id": item.get("id"),
                              "pointer": f"/placeholders/{key}",
                              "message": f"placeholder {key!r} ({info['type']}) does not accept {kind}",
                              "fix_hint": f"accepts: {', '.join(info['accepts']) or 'nothing'}"})  # fmt: skip
        for item in _all_items(doc):
            eid = item.get("id")
            if eid:
                if eid in seen:
                    diags.append({"rule": "schema", "severity": "error", "slide_id": sid, "element_id": eid,
                                  "message": f"element id {eid} is used more than once"})  # fmt: skip
                seen.add(eid)
            if item.get("type") == "image":
                asset = item.get("asset", "")
                try:
                    ok = resolve_in_root(project, asset).is_file()
                except Exception:
                    ok = False
                if not ok:
                    diags.append({"rule": "schema", "severity": "error", "slide_id": sid, "element_id": eid,
                                  "message": f"asset {asset!r} not found"})  # fmt: skip
    return diags


def _all_items(doc: dict[str, Any]):
    yield from (doc.get("placeholders") or {}).values()
    for el in doc.get("elements") or []:
        content = dict(el.get("content") or {})
        content.setdefault("id", el.get("id"))
        yield content
