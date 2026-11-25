"""Scene state (ARCHITECTURE 5.4).

Scene state is what is on the canvas: parameters, objects, and notes. It is versioned by a
revision counter and a content hash. The hash covers only mathematical content (expressions
by content hash, parameter values, options, styles), never handle ids, so the same notebook
replayed in two processes gives the same hash.

View hints (``cam.fit``, ``cam.view``) are kept beside the state but outside the hash: the
camera is presentation, never scene truth (UX note §1a).

The scene is also a dependency graph (ARCHITECTURE 5.6). An object depends on the parameters
its expressions mention and on the objects it refers to. The edges are derived from content,
never stored, so they cannot disagree with it; ``affected`` answers "what must be redrawn when
these parameters move", which is what lets a parameter change skip everything else.

Objects and parameters are never changed in place: an operation replaces them. That makes a
copy of the state a copy of three dictionaries, and lets a diff skip whatever is the same object.
"""

from __future__ import annotations

import copy
import hashlib
import json
from dataclasses import dataclass, field
from typing import Any

import sympy as sp

from .handles import HandleTable, content_hash

# Spec keys that name another object.
REF_KEYS = ("of", "target", "tangent_of")


@dataclass
class SceneObject:
    id: str
    kind: str
    exprs: dict[str, Any]  # name -> SymPy object
    spec: dict[str, Any]  # JSON-able options
    style: dict[str, Any] = field(default_factory=dict)
    origin: str | None = None  # cell id or other origin label
    _canonical: dict[str, Any] | None = field(default=None, repr=False, compare=False)
    _shown: dict[str, Any] | None = field(default=None, repr=False, compare=False)

    def canonical(self) -> dict[str, Any]:
        if self._canonical is None:
            self._canonical = {
                "id": self.id,
                "kind": self.kind,
                "exprs": {k: content_hash(v) for k, v in sorted(self.exprs.items())},
                "spec": self.spec,
                "style": self.style,
            }
        return self._canonical

    def symbol_names(self) -> set[str]:
        """Names of every free symbol in the object's expressions."""
        names: set[str] = set()
        for expr in self.exprs.values():
            names.update(str(s) for s in getattr(expr, "free_symbols", ()))
        return names

    def object_refs(self) -> list[str]:
        """Ids of the objects this one refers to (what it is drawn on, or was derived from)."""
        refs = [self.spec.get(key) for key in REF_KEYS]
        at = self.spec.get("at")
        if isinstance(at, dict):
            refs.append(at.get("object"))
        return list(dict.fromkeys(r for r in refs if isinstance(r, str)))

    def public(self, handles: HandleTable) -> dict[str, Any]:
        if self._shown is None:
            # Printing is the expensive part of showing an object, and its content never changes.
            self._shown = {
                name: {"hash": content_hash(obj), "latex": sp.latex(obj), "text": sp.sstr(obj)}
                for name, obj in sorted(self.exprs.items())
            }
        exprs = {
            name: {**handles.put(self.exprs[name], shown["hash"]), **shown} for name, shown in self._shown.items()
        }
        out = {"id": self.id, "kind": self.kind, "exprs": exprs, "spec": self.spec, "style": self.style}
        if self.origin:
            out["origin"] = self.origin
        return out


@dataclass(frozen=True)
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
        def with_deps(item: SceneObject) -> dict[str, Any]:
            return {**item.public(handles), "deps": self.deps(item)}

        return {
            "revision": self.revision,
            "hash": self.hash(),
            "params": {k: p.public() for k, p in self.params.items()},
            "objects": [with_deps(o) for o in self.objects.values()],
            "notes": [with_deps(n) for n in self.notes.values()],
            "view": self.view,
        }

    def copy(self) -> SceneState:
        # Objects and parameters are replaced, never changed in place, so the containers are
        # all that needs copying.
        return SceneState(
            revision=self.revision,
            params=dict(self.params),
            objects=dict(self.objects),
            notes=dict(self.notes),
            view=copy.deepcopy(self.view),
            counters=dict(self.counters),
        )

    # ---- the dependency graph ----------------------------------------------------
    def find(self, obj_id: str) -> SceneObject | None:
        return self.objects.get(obj_id) or self.notes.get(obj_id)

    def deps(self, item: SceneObject) -> dict[str, list[str]]:
        """What one object depends on directly: declared parameters and other objects."""
        return {
            "params": sorted(item.symbol_names() & self.params.keys()),
            "objects": [r for r in item.object_refs() if self.find(r) is not None],
        }

    def closure(self, obj_id: str) -> tuple[list[str], list[str]]:
        """Every parameter and object that ``obj_id`` depends on, directly or through others.

        Returns ``(parameter names, object ids)``, each sorted; the object ids include
        ``obj_id`` itself.
        """
        params: set[str] = set()
        seen: set[str] = set()
        stack = [obj_id]
        while stack:
            current = stack.pop()
            item = self.find(current)
            if item is None or current in seen:
                continue
            seen.add(current)
            direct = self.deps(item)
            params.update(direct["params"])
            stack.extend(direct["objects"])
        return sorted(params), sorted(seen)

    def affected(self, params: Any) -> list[str]:
        """Ids of the objects and notes that must be redrawn when ``params`` change, in scene order."""
        changed = set(params)
        if not changed:
            return []
        out = []
        for table in (self.objects, self.notes):
            for obj_id in table:
                if changed.intersection(self.closure(obj_id)[0]):
                    out.append(obj_id)
        return out

    def graph(self) -> dict[str, Any]:
        """The whole graph: each object's direct dependencies, and each parameter's dependents."""
        nodes = {}
        for table in (self.objects, self.notes):
            for obj_id, item in table.items():
                nodes[obj_id] = {"kind": item.kind, "origin": item.origin, **self.deps(item)}
        return {
            "revision": self.revision,
            "objects": nodes,
            "params": {name: self.affected([name]) for name in self.params},
        }

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
        old = before_items(name)
        added = [v.public(handles) for k, v in items.items() if k not in old]
        updated = [
            v.public(handles)
            for k, v in items.items()
            if k in old and old[k] is not v and old[k].canonical() != v.canonical()
        ]
        removed = [k for k in old if k not in items]
        if added or updated or removed:
            out[name] = {"added": added, "updated": updated, "removed": removed}

    def before_items(name: str) -> dict[str, SceneObject]:
        return before.objects if name == "objects" else before.notes

    by_id(after.objects, "objects")
    by_id(after.notes, "notes")
    params = {
        k: p.public()
        for k, p in after.params.items()
        if k not in before.params or (before.params[k] is not p and before.params[k] != p)
    }
    removed_params = [k for k in before.params if k not in after.params]
    if params or removed_params:
        # ``affected``: what has to be redrawn because of these parameters, and nothing else.
        moved = [k for k in params if k in before.params] + removed_params
        out["params"] = {"changed": params, "removed": removed_params, "affected": after.affected(moved)}
    if before.view != after.view:
        out["view"] = after.view
    return out
