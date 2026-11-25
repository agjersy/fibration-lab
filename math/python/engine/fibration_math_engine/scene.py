"""Scene state (ARCHITECTURE 5.4).

Scene state is what is on the canvas: parameters, objects, and notes. It is versioned by a
revision counter and a content hash. The hash covers only mathematical content (expressions
by content hash, parameter values, options, styles), never handle ids, so the same notebook
replayed in two processes gives the same hash.

View hints (``cam.fit``, ``cam.view``) are kept beside the state but outside the hash: the
camera is presentation, never scene truth (UX note §1a).
"""

from __future__ import annotations

import copy
import hashlib
import json
from dataclasses import dataclass, field
from typing import Any

import sympy as sp

from .handles import HandleTable, content_hash


@dataclass
class SceneObject:
    id: str
    kind: str
    exprs: dict[str, Any]  # name -> SymPy object
    spec: dict[str, Any]  # JSON-able options
    style: dict[str, Any] = field(default_factory=dict)
    origin: str | None = None  # cell id or other origin label

    def canonical(self) -> dict[str, Any]:
        return {
            "id": self.id,
            "kind": self.kind,
            "exprs": {k: content_hash(v) for k, v in sorted(self.exprs.items())},
            "spec": self.spec,
            "style": self.style,
        }

    def public(self, handles: HandleTable) -> dict[str, Any]:
        exprs = {}
        for name, obj in sorted(self.exprs.items()):
            ref = handles.put(obj)
            exprs[name] = {**ref, "hash": content_hash(obj), "latex": sp.latex(obj), "text": sp.sstr(obj)}
        out = {"id": self.id, "kind": self.kind, "exprs": exprs, "spec": self.spec, "style": self.style}
        if self.origin:
            out["origin"] = self.origin
        return out


@dataclass
class Param:
    name: str
    value: float
    min: float
    max: float
    step: float | None = None

    def public(self) -> dict[str, Any]:
        out: dict[str, Any] = {"value": self.value, "min": self.min, "max": self.max}
        if self.step is not None:
            out["step"] = self.step
        return out


@dataclass
class SceneState:
    revision: int = 0
    params: dict[str, Param] = field(default_factory=dict)
    objects: dict[str, SceneObject] = field(default_factory=dict)
    notes: dict[str, SceneObject] = field(default_factory=dict)
    view: dict[str, Any] = field(default_factory=dict)
    counters: dict[str, int] = field(default_factory=dict)

    # ---- identity ----------------------------------------------------------------
    def next_id(self, prefix: str) -> str:
        n = self.counters.get(prefix, 0) + 1
        self.counters[prefix] = n
        return f"{prefix}-{n}"

    def canonical(self) -> dict[str, Any]:
        return {
            "params": {k: p.public() for k, p in sorted(self.params.items())},
            "objects": [o.canonical() for o in self.objects.values()],
            "notes": [n.canonical() for n in self.notes.values()],
        }

    def hash(self) -> str:
        text = json.dumps(self.canonical(), sort_keys=True, separators=(",", ":"))
        return hashlib.sha256(text.encode("utf-8")).hexdigest()

    def public(self, handles: HandleTable) -> dict[str, Any]:
        return {
            "revision": self.revision,
            "hash": self.hash(),
            "params": {k: p.public() for k, p in self.params.items()},
            "objects": [o.public(handles) for o in self.objects.values()],
            "notes": [n.public(handles) for n in self.notes.values()],
            "view": self.view,
        }

    def copy(self) -> SceneState:
        # SymPy objects are immutable, so a structural copy of the containers is enough.
        return SceneState(
            revision=self.revision,
            params={k: copy.copy(p) for k, p in self.params.items()},
            objects={k: copy.copy(o) for k, o in self.objects.items()},
            notes={k: copy.copy(n) for k, n in self.notes.items()},
            view=copy.deepcopy(self.view),
            counters=dict(self.counters),
        )

    # ---- queries used by masks -------------------------------------------------
    def has_params(self) -> bool:
        return bool(self.params)

    def has_objects(self) -> bool:
        return bool(self.objects) or bool(self.notes)

    def has_kind(self, *kinds: str) -> bool:
        return any(o.kind in kinds for o in self.objects.values())

    def param_symbols(self) -> set[sp.Symbol]:
        return {sp.Symbol(name, real=True) for name in self.params}

    def param_values(self) -> dict[sp.Symbol, float]:
        return {sp.Symbol(name, real=True): p.value for name, p in self.params.items()}


def diff(before: SceneState, after: SceneState, handles: HandleTable) -> dict[str, Any]:
    """What changed, in the shape the renderer applies: added, updated, removed by id."""
    out: dict[str, Any] = {}

    def by_id(items: dict[str, SceneObject], name: str) -> None:
        old = {k: v.canonical() for k, v in before_items(name).items()}
        new = {k: v.canonical() for k, v in items.items()}
        added = [items[k].public(handles) for k in new if k not in old]
        updated = [items[k].public(handles) for k in new if k in old and old[k] != new[k]]
        removed = [k for k in old if k not in new]
        if added or updated or removed:
            out[name] = {"added": added, "updated": updated, "removed": removed}

    def before_items(name: str) -> dict[str, SceneObject]:
        return before.objects if name == "objects" else before.notes

    by_id(after.objects, "objects")
    by_id(after.notes, "notes")
    params = {
        k: p.public()
        for k, p in after.params.items()
        if k not in before.params or before.params[k].public() != p.public()
    }
    removed_params = [k for k in before.params if k not in after.params]
    if params or removed_params:
        out["params"] = {"changed": params, "removed": removed_params}
    if before.view != after.view:
        out["view"] = after.view
    return out
