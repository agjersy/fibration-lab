"""Display: how an object is shown, and its shape in an engine-neutral vocabulary (MQ-10)."""

from __future__ import annotations

import sympy as sp

from ..handles import NumericFn
from ..registry import op
from ..schema import handle

ANY = handle("expr", "relation", "boolean", "set")


def head(obj) -> str:
    """The top-level shape of an object, without SymPy class names."""
    if isinstance(obj, NumericFn):
        return "numeric_fn"
    if isinstance(obj, sp.Symbol):
        return "symbol"
    if isinstance(obj, sp.Integer):
        return "integer"
    if isinstance(obj, sp.Rational):
        return "rational"
    if isinstance(obj, sp.Float):
        return "decimal"
    if isinstance(obj, sp.NumberSymbol) or obj in (sp.I, sp.oo, -sp.oo, sp.zoo, sp.nan):
        return "constant"
    if isinstance(obj, sp.Add):
        return "sum"
    if isinstance(obj, sp.Mul):
        return "product"
    if isinstance(obj, sp.Pow):
        return "power"
    if isinstance(obj, sp.Piecewise):
        return "piecewise"
    if isinstance(obj, sp.core.relational.Relational):
        return {"==": "equation", "!=": "inequation"}.get(obj.rel_op, "inequality")
    if isinstance(obj, sp.Interval):
        return "interval"
    if isinstance(obj, sp.FiniteSet):
        return "finite_set"
    if isinstance(obj, sp.Union):
        return "union"
    if isinstance(obj, sp.Set):
        return "set"
    if isinstance(obj, sp.And | sp.Or | sp.Not):
        return {"And": "and", "Or": "or", "Not": "not"}[type(obj).__name__]
    if isinstance(obj, sp.Integral):
        return "integral"
    if isinstance(obj, sp.Derivative):
        return "derivative"
    if isinstance(obj, sp.Function):
        return "function:" + type(obj).__name__.lower()
    return "other"


@op("to_latex", "display", "LaTeX for an object", args={"of": ANY}, required=["of"], result="string", pure=True)
def to_latex(engine, of):
    return sp.latex(engine.value(of))


@op("to_text", "display", "Plain text for an object", args={"of": ANY}, required=["of"], result="string", pure=True)
def to_text(engine, of):
    return sp.sstr(engine.value(of))


@op(
    "to_srepr",
    "display",
    "SymPy srepr text for an object (for users' own SymPy code)",
    args={"of": ANY},
    required=["of"],
    result="string",
    pure=True,
)
def to_srepr(engine, of):
    return sp.srepr(engine.value(of))


@op(
    "info",
    "display",
    "Kind, shape, LaTeX, text, and free symbols of an object",
    args={"of": handle("expr", "relation", "boolean", "set", "numeric_fn")},
    required=["of"],
    result="object",
    pure=True,
)
def info(engine, of):
    entry = engine.handles.entry(of)
    obj = entry.obj
    if isinstance(obj, NumericFn):
        return {"kind": entry.kind, "head": "numeric_fn", "hash": entry.hash}
    return {
        "kind": entry.kind,
        "head": head(obj),
        "latex": sp.latex(obj),
        "text": sp.sstr(obj),
        "free_symbols": sorted(str(s) for s in obj.free_symbols),
        "hash": entry.hash,
    }
