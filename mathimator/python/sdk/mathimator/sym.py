"""Construct expressions: symbols, math text, piecewise definitions, SymPy objects."""

from __future__ import annotations

from typing import Any

from ._objects import Expr, Handle, Relation, _scalar, call


def symbols(names: str) -> Any:
    """Real symbols: ``x = symbols("x")`` or ``x, y = symbols("x y")``."""
    out = call("sym.symbols", names=names)
    for s, n in zip(out, names.replace(",", " ").split()):
        s.name = n
    return out[0] if len(out) == 1 else tuple(out)


def define(text: str, name: str | None = None) -> Any:
    """Read math text: ``define("a*sin(b*x)")``, ``define("x**2 + y**2 = 1")``.

    ``name`` labels the object in this script's output only; it is not sent to the engine.
    """
    out = call("sym.define", text=text)
    out.name = name
    return out


def piecewise(*pieces: tuple[Any, Any]) -> Expr:
    """``piecewise((x**2, "x < 0"), (x, "otherwise"))``; conditions may be relations."""
    return call("sym.piecewise", pieces=[{"expr": _scalar(e), "when": c} for e, c in pieces])


def relation(lhs: Any, rel: str, rhs: Any) -> Relation:
    return call("expr.relation", lhs=_scalar(lhs), rel=rel, rhs=_scalar(rhs))


def from_sympy(obj: Any) -> Handle:
    """Hand a SymPy object built by your own code to the engine (sent as srepr, re-read there)."""
    import sympy

    return call("sym.from_srepr", srepr=sympy.srepr(obj))


def from_srepr(text: str) -> Handle:
    return call("sym.from_srepr", srepr=text)
