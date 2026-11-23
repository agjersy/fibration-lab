"""Masks and helpers shared by the operation modules."""

from __future__ import annotations

from typing import Any

import sympy as sp

from ..errors import CallError
from ..registry import Mask

HAS_PARAMS = Mask("has_params", lambda s: s.has_params(), "declare a parameter first (param.declare)")
HAS_OBJECTS = Mask("has_objects", lambda s: s.has_objects(), "the scene has no objects yet")
HAS_EXPLICIT = Mask(
    "has_explicit_curve", lambda s: s.has_kind("explicit"), "the scene needs a curve y = f(x) (plot.explicit)"
)

LIMIT_DIRS = ["+", "-", "+-"]


def check_bound(engine, expr: Any, allowed: set[sp.Symbol], what: str) -> None:
    """Every free symbol of a plotted object must be a plot variable or a declared parameter."""
    free = getattr(expr, "free_symbols", set())
    unbound = sorted(str(s) for s in free - allowed - engine.scene.param_symbols())
    if unbound:
        raise CallError(
            "bad_args",
            f"the {what} uses {', '.join(unbound)}, which {'is' if len(unbound) == 1 else 'are'} neither "
            "a plot variable nor a declared parameter",
            hint=f"declare {'it' if len(unbound) == 1 else 'them'} with param.declare, or substitute values with subs",
        )


def exact_text(value: sp.Expr) -> dict[str, Any]:
    """A scalar stored in scene state: its exact text and its float value."""
    out: dict[str, Any] = {"text": sp.sstr(value)}
    try:
        out["value"] = float(value)
    except (TypeError, ValueError):
        out["value"] = None
    return out


def to_float(value: sp.Expr) -> float | None:
    try:
        f = float(sp.N(value, 17))
    except (TypeError, ValueError):
        return None
    return f
