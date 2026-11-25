"""Derived operations: real SymPy computation, each returning new immutable objects."""

from __future__ import annotations

import sympy as sp
from sympy.calculus.accumulationbounds import AccumulationBounds

from ..errors import CallError
from ..memo import Out
from ..registry import HEAVY_BUDGET_S, op
from ..schema import handle, integer, interval, scalar, string
from ..truth import truth
from .common import LIMIT_DIRS

EXPR = handle("expr", description="An expression")
VAR = scalar("The variable, a symbol such as x")


@op(
    "diff",
    "derive",
    "Derivative with respect to a variable, n times",
    args={"expr": EXPR, "var": VAR, "n": integer(minimum=1, maximum=20)},
    required=["expr", "var"],
    result="handle:expr",
    pure=True,
)
def diff(engine, expr, var, n=1):
    return Out(sp.diff(engine.expr(expr), engine.symbol(var), int(n)))


@op(
    "integrate",
    "derive",
    "Antiderivative, or a definite integral over bounds [lo, hi] (may be infinite: 'oo')",
    args={"expr": EXPR, "var": VAR, "bounds": interval()},
    required=["expr", "var"],
    result="{value: handle, evaluated: bool}",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def integrate(engine, expr, var, bounds=None):
    f, x = engine.expr(expr), engine.symbol(var)
    if bounds is None:
        result = sp.integrate(f, x)
    else:
        lo, hi = engine.interval(bounds, "integration bounds")
        result = sp.integrate(f, (x, lo, hi))
    return {"value": Out(result), "evaluated": not result.has(sp.Integral)}


@op(
    "limit",
    "derive",
    "Limit as the variable approaches a point ('oo' for infinity); dir is '+', '-' or '+-'",
    args={"expr": EXPR, "var": VAR, "at": scalar(), "dir": string(enum=LIMIT_DIRS)},
    required=["expr", "var", "at"],
    result="{value: handle | null, exists: truth, finite?: truth, left?: handle, right?: handle, bounds?: [handle, handle]}",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def limit(engine, expr, var, at, dir="+-"):
    """The variable is real, so a two-sided limit is computed as both one-sided limits and
    exists only when they agree. (SymPy's own two-sided limit answers ``zoo``, complex
    infinity, for 1/x at 0, which is not a real limit.) An infinite limit exists in the
    extended reals: ``exists`` is true and ``finite`` is false."""
    f, x, a = engine.expr(expr), engine.symbol(var), engine.expr(at, "limit point")
    if a.is_infinite:
        dir = "-" if a == sp.oo else "+"
    if dir == "+-":
        left, right = _one_sided(f, x, a, "-"), _one_sided(f, x, a, "+")
        for side in (left, right):
            if side["value"] is None:
                return side
        lv, rv = left["value"].obj, right["value"].obj
        if lv != rv:
            same = sp.simplify(lv - rv) == 0 if not (lv.is_infinite or rv.is_infinite) else False
            if not same:
                return {
                    "value": None,
                    "exists": truth(False, reason=f"the left limit is {lv} and the right limit is {rv}"),
                    "left": Out(lv),
                    "right": Out(rv),
                }
        return right
    return _one_sided(f, x, a, dir)


def _one_sided(f, x, a, dir: str) -> dict:
    try:
        value = sp.limit(f, x, a, dir)
    except ValueError as exc:
        return {"value": None, "exists": truth(None, reason=str(exc) or "SymPy could not evaluate this limit")}
    if isinstance(value, AccumulationBounds):
        return {
            "value": None,
            "exists": truth(False, reason=f"the values oscillate within [{value.min}, {value.max}]"),
            "bounds": [Out(value.min), Out(value.max)],
        }
    if value.has(sp.Limit) or value is sp.nan or value.has(sp.zoo):
        return {"value": None, "exists": truth(None, reason=f"SymPy could not evaluate this limit (got {value})")}
    return {"value": Out(value), "exists": truth(True), "finite": truth(bool(value.is_finite))}


@op(
    "series",
    "derive",
    "Taylor or Laurent polynomial of order n about a point (the O() term removed)",
    args={"expr": EXPR, "var": VAR, "at": scalar(), "n": integer(minimum=1, maximum=30)},
    required=["expr", "var"],
    result="handle:expr",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def series(engine, expr, var, at=0, n=6):
    f, x, a = engine.expr(expr), engine.symbol(var), engine.expr(at, "expansion point")
    return Out(sp.series(f, x, a, int(n)).removeO())


def _transform(name: str, fn, summary: str, heavy: bool = False):
    @op(
        name,
        "derive",
        summary,
        args={"expr": EXPR},
        required=["expr"],
        result="handle:expr",
        pure=True,
        budget_s=HEAVY_BUDGET_S if heavy else 5.0,
    )
    def run(engine, expr):
        return Out(fn(engine.value(expr)))

    return run


_transform("simplify", sp.simplify, "Simplify an expression", heavy=True)
_transform("expand", sp.expand, "Expand products and powers")
_transform("factor", sp.factor, "Factor over the rationals")
_transform("cancel", sp.cancel, "Put a rational expression in canonical p/q form")
_transform("trigsimp", sp.trigsimp, "Simplify trigonometric expressions", heavy=True)


@op(
    "subs",
    "derive",
    "Substitute values for symbols, by name: {'a': 2, 'x': 'pi/2'}",
    args={
        "expr": handle("expr", "relation", "boolean"),
        "values": {"type": "object", "properties": {}, "additionalProperties": scalar()},
    },
    required=["expr", "values"],
    result="handle",
    pure=True,
)
def subs(engine, expr, values):
    obj = engine.value(expr)
    if len(values) > 50:
        raise CallError("budget", "at most 50 substitutions per call")
    mapping = {engine.symbol(name, "substitution target"): engine.expr(v) for name, v in values.items()}
    return Out(obj.subs(mapping))


@op(
    "solve",
    "derive",
    "Real solutions of an equation (or expr = 0) for a variable, optionally within an interval",
    args={"equation": handle("expr", "relation"), "var": VAR, "within": interval()},
    required=["equation", "var"],
    result="{solutions: handle[], complete: truth, set: handle}",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def solve(engine, equation, var, within=None):
    eq, x = engine.value(equation), engine.symbol(var)
    domain = sp.S.Reals if within is None else sp.Interval(*engine.interval(within))
    result = sp.solveset(eq, x, domain=domain)
    if isinstance(result, sp.FiniteSet):
        sols = sorted(result, key=sp.default_sort_key)
        if len(sols) > 100:
            raise CallError("budget", "more than 100 solutions; narrow the interval")
        return {"solutions": [Out(s) for s in sols], "complete": truth(True), "set": Out(result)}
    if result is sp.S.EmptySet:
        return {"solutions": [], "complete": truth(True), "set": Out(result)}
    return {
        "solutions": [],
        "complete": truth(None, reason=f"the solution set is not a finite list: {result}"),
        "set": Out(result),
    }
