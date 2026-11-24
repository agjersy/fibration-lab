"""The operation registry: the single source of the taxonomy (ARCHITECTURE 5.3).

Every operation is declared once with ``@op``: its name, category, purity, argument schema,
result kind, masking predicate, budget, and one-line summary. ``describe_api`` is generated
from this table, and the engine dispatches from it, so the manifest cannot drift from what
the engine does.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

API_VERSION = 1

CATEGORIES = ("construct", "derive", "introspect", "display", "object", "parameter", "annotate", "camera")

DEFAULT_BUDGET_S = 5.0
HEAVY_BUDGET_S = 20.0


@dataclass(frozen=True)
class Mask:
    """A named predicate over scene state, with the reason shown when it is false."""

    name: str
    test: Callable[[Any], bool]
    reason: str


@dataclass
class Op:
    name: str
    category: str
    pure: bool
    summary: str
    args: dict[str, Any]
    required: list[str]
    result: str
    impl: Callable[..., Any]
    mask: Mask | None = None
    budget_s: float = DEFAULT_BUDGET_S
    changes_view: bool = False
    tags: list[str] = field(default_factory=list)

    def schema(self) -> dict[str, Any]:
        return {"type": "object", "properties": self.args, "required": self.required}

    def manifest(self) -> dict[str, Any]:
        out: dict[str, Any] = {
            "name": self.name,
            "category": self.category,
            "pure": self.pure,
            "summary": self.summary,
            "args": self.schema(),
            "result": self.result,
            "budget_s": self.budget_s,
        }
        if self.mask is not None:
            out["mask"] = {"name": self.mask.name, "reason": self.mask.reason}
        if self.tags:
            out["tags"] = list(self.tags)
        return out


REGISTRY: dict[str, Op] = {}


def op(
    name: str,
    category: str,
    summary: str,
    args: dict[str, Any] | None = None,
    required: list[str] | None = None,
    result: str = "none",
    pure: bool = False,
    mask: Mask | None = None,
    budget_s: float = DEFAULT_BUDGET_S,
    changes_view: bool = False,
    tags: list[str] | None = None,
) -> Callable[[Callable[..., Any]], Callable[..., Any]]:
    assert category in CATEGORIES, category

    def register(fn: Callable[..., Any]) -> Callable[..., Any]:
        assert name not in REGISTRY, f"duplicate operation {name}"
        REGISTRY[name] = Op(
            name=name,
            category=category,
            pure=pure,
            summary=summary,
            args=args or {},
            required=list(required or []),
            result=result,
            impl=fn,
            mask=mask,
            budget_s=budget_s,
            changes_view=changes_view,
            tags=list(tags or []),
        )
        return fn

    return register


def manifest(state: Any | None = None) -> dict[str, Any]:
    """The machine-readable API. With a scene state, each operation says whether it is available."""
    ops = []
    for name in sorted(REGISTRY):
        entry = REGISTRY[name].manifest()
        if state is not None:
            mask = REGISTRY[name].mask
            entry["available"] = mask is None or bool(mask.test(state))
        ops.append(entry)
    return {"api_version": API_VERSION, "operations": ops}
