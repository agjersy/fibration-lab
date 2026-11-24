"""Introspection: read-only questions about the mathematics, answered by the engine.

These are what make a claim checkable. Each answer says how it was reached: ``symbolic``
answers can support a **proven** claim; ``numeric`` answers (a root found by bisection, a
sign read from samples) support only **checked** (ARCHITECTURE 10.4).
"""

from __future__ import annotations

import itertools
import math

import mpmath
import sympy as sp
from sympy.calculus.util import continuous_domain as _continuous_domain

from ..errors import CallError
from ..memo import Out
from ..registry import HEAVY_BUDGET_S, op
from ..schema import handle, interval, scalar, string
from ..truth import from_fuzzy, truth
from .common import LIMIT_DIRS, to_float

EXPR = handle("expr", description="An expression")
VAR = scalar("The variable, a symbol such as x")
NUMERIC_GRID = 2000
MAX_POINTS = 100


def _domain(engine, within):
    return sp.S.Reals if within is None else sp.Interval(*engine.interval(within))


@op(
    "continuous_domain",
    "introspect",
    "The real set on which the expression is defined and continuous",
    args={"expr": EXPR, "var": VAR, "within": interval()},
    required=["expr", "var"],
    result="handle:set",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def continuous_domain(engine, expr, var, within=None):
    return Out(_continuous_domain(engine.expr(expr), engine.symbol(var), _domain(engine, within)))


@op(
    "singularities",
    "introspect",
    "Real points where the expression is singular",
    args={"expr": EXPR, "var": VAR, "within": interval()},
    required=["expr", "var"],
    result="{set: handle, points: handle[] | null}",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def singularities(engine, expr, var, within=None):
    result = sp.singularities(engine.expr(expr), engine.symbol(var), _domain(engine, within))
    points = None
    if isinstance(result, sp.FiniteSet) or result is sp.S.EmptySet:
        points = [Out(p) for p in sorted(result, key=sp.default_sort_key)][:MAX_POINTS]
    return {"set": Out(result), "points": points}


@op(
    "is_continuous_on",
    "introspect",
    "Whether the expression is continuous on the whole interval",
    args={"expr": EXPR, "var": VAR, "over": interval()},
    required=["expr", "var", "over"],
    result="truth",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def is_continuous_on(engine, expr, var, over):
    f, x = engine.expr(expr), engine.symbol(var)
    region = sp.Interval(*engine.interval(over))
    try:
        cont = _continuous_domain(f, x, region)
    except NotImplementedError:
        return truth(None, reason="SymPy cannot determine where this expression is continuous")
    return _subset(region, cont)


def _subset(region, superset):
    """Whether region is inside superset, decided from the set difference when SymPy's
    is_subset cannot tell (it often cannot for unions of intervals)."""
    decided = region.is_subset(superset)
    if decided is not None:
        return from_fuzzy(decided)
    missing = sp.Complement(region, superset)
    if missing is sp.S.EmptySet:
        return truth(True)
    if isinstance(missing, sp.FiniteSet | sp.Interval) or missing.is_empty is False:
        return truth(False, reason=f"not continuous at {missing}")
    return truth(None, reason="SymPy cannot compare these sets")


@op(
    "is_finite_limit",
    "introspect",
    "Whether the limit at a point exists and is finite",
    args={"expr": EXPR, "var": VAR, "at": scalar(), "dir": string(enum=LIMIT_DIRS)},
    required=["expr", "var", "at"],
    result="truth",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def is_finite_limit(engine, expr, var, at, dir="+-"):
    from .derive import limit

    out = limit(engine, expr, var, at, dir)
    if out["exists"]["truth"] != "true":
        return out["exists"] if out["exists"]["truth"] == "unknown" else truth(False, reason=out["exists"].get("reason"))
    value = out["value"].obj
    return from_fuzzy(value.is_finite, reason=None if value.is_finite else f"the limit is {value}")


def _numeric_fn(f, x):
    fn = sp.lambdify(x, f, modules="mpmath")

    def call(v: float) -> float | None:
        try:
            y = fn(mpmath.mpf(v))
            if isinstance(y, mpmath.mpc):
                return None if abs(y.imag) > 1e-12 * max(1, abs(y.real)) else float(y.real)
            y = float(y)
        except (ZeroDivisionError, ValueError, TypeError, OverflowError):
            return None
        return y if math.isfinite(y) else None

    return call


def _need_numeric_interval(engine, f, x, over):
    if over is None:
        raise CallError(
            "undecided",
            "the engine could not solve this exactly, and a numeric search needs a finite interval",
            hint="pass within=[lo, hi]",
        )
    lo, hi = (to_float(v) for v in engine.interval(over))
    if lo is None or hi is None or not (math.isfinite(lo) and math.isfinite(hi)):
        raise CallError("undecided", "a numeric search needs a finite interval")
    if f.free_symbols - {x}:
        raise CallError(
            "undecided",
            "the expression has other free symbols, so it cannot be searched numerically",
            hint="substitute parameter values first (subs)",
        )
    return lo, hi


def _numeric_roots(f, x, lo: float, hi: float) -> list[float]:
    fn = _numeric_fn(f, x)
    xs = [lo + (hi - lo) * i / NUMERIC_GRID for i in range(NUMERIC_GRID + 1)]
    ys = [fn(v) for v in xs]
    roots: list[float] = []
    for (a, ya), (b, yb) in zip(zip(xs, ys), zip(xs[1:], ys[1:])):
        if ya is None or yb is None:
            continue
        if ya == 0:
            roots.append(a)
        elif ya * yb < 0:
            try:
                r = float(mpmath.findroot(lambda t: fn(float(t)) or 0.0, (a, b), solver="bisect"))
            except (ValueError, ZeroDivisionError):
                continue
            # A sign change across a pole is not a root.
            val = fn(r)
            if val is not None and abs(val) < 1e-8 * max(1.0, abs(ya), abs(yb)):
                roots.append(r)
    if ys[-1] == 0:
        roots.append(hi)
    return roots[:MAX_POINTS]


@op(
    "roots_in",
    "introspect",
    "Real roots of the expression, exactly when possible, else numerically within an interval",
    args={"expr": EXPR, "var": VAR, "within": interval()},
    required=["expr", "var"],
    result="{roots: [{x: handle, approx: number}], complete: truth, method: string}",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def roots_in(engine, expr, var, within=None):
    f, x = engine.expr(expr), engine.symbol(var)
    domain = _domain(engine, within)
    result = sp.solveset(f, x, domain=domain)
    if isinstance(result, sp.FiniteSet) or result is sp.S.EmptySet:
        roots = sorted(result, key=lambda r: (to_float(r) is None, to_float(r) or 0.0))
        if len(roots) > MAX_POINTS:
            raise CallError("budget", f"more than {MAX_POINTS} roots; narrow the interval")
        return {
            "roots": [{"x": Out(r), "approx": to_float(r)} for r in roots],
            "complete": truth(True),
            "method": "symbolic",
        }
    lo, hi = _need_numeric_interval(engine, f, x, within)
    found = _numeric_roots(f, x, lo, hi)
    return {
        "roots": [{"x": Out(sp.Float(r, 15)), "approx": r} for r in found],
        "complete": truth(None, reason="roots found numerically by sign changes; roots of even multiplicity can be missed"),
        "method": "numeric",
    }


def _classify(f, x, c) -> str:
    second = sp.diff(f, x, 2).subs(x, c)
    second = sp.simplify(second) if not second.is_number else second
    if second.is_positive:
        return "local_min"
    if second.is_negative:
        return "local_max"
    return "undetermined"


@op(
    "extrema",
    "introspect",
    "Critical points in an interval, classified by the second derivative, with the extreme values over it",
    args={"expr": EXPR, "var": VAR, "over": interval()},
    required=["expr", "var", "over"],
    result="{points: [...], complete: truth, global_min?: handle, global_max?: handle}",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def extrema(engine, expr, var, over):
    f, x = engine.expr(expr), engine.symbol(var)
    lo, hi = engine.interval(over)
    region = sp.Interval(lo, hi)
    crit = sp.solveset(sp.diff(f, x), x, domain=region)
    if not (isinstance(crit, sp.FiniteSet) or crit is sp.S.EmptySet):
        return {
            "points": [],
            "complete": truth(None, reason=f"the critical points are not a finite list: {crit}"),
        }
    if len(crit) > MAX_POINTS:
        raise CallError("budget", f"more than {MAX_POINTS} critical points; narrow the interval")
    points = []
    for c in sorted(crit, key=lambda v: to_float(v) or 0.0):
        value = f.subs(x, c)
        points.append(
            {"x": Out(c), "approx": to_float(c), "value": Out(value), "kind": _classify(f, x, c)}
        )
    out = {"points": points, "complete": truth(True)}
    # Extreme values over the closed interval, when f is continuous there and all values are numbers.
    if not (f.free_symbols - {x}) and all(v.is_finite for v in (lo, hi)):
        cont = _continuous_domain(f, x, region)
        if _subset(region, cont)["truth"] == "true":
            candidates = [*crit, lo, hi]
            values = [(f.subs(x, c), c) for c in candidates]
            numeric = [(to_float(v), v, c) for v, c in values]
            if all(n is not None for n, _, _ in numeric):
                lo_val = min(numeric, key=lambda t: t[0])
                hi_val = max(numeric, key=lambda t: t[0])
                out["global_min"] = {"x": Out(lo_val[2]), "value": Out(lo_val[1])}
                out["global_max"] = {"x": Out(hi_val[2]), "value": Out(hi_val[1])}
    return out


@op(
    "sign_on",
    "introspect",
    "The sign of the expression over an interval: positive, negative, nonnegative, nonpositive, zero, or mixed",
    args={"expr": EXPR, "var": VAR, "over": interval()},
    required=["expr", "var", "over"],
    result="{sign: string, method: string}",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def sign_on(engine, expr, var, over):
    f, x = engine.expr(expr), engine.symbol(var)
    lo, hi = engine.interval(over)
    region = sp.Interval(lo, hi)
    if f.free_symbols - {x}:
        # Decide from assumptions alone (e.g. exp(a*x) is positive for any real a).
        if f.is_positive:
            return {"sign": "positive", "method": "symbolic"}
        if f.is_negative:
            return {"sign": "negative", "method": "symbolic"}
        if f.is_zero:
            return {"sign": "zero", "method": "symbolic"}
        return {"sign": "unknown", "method": "none"}
    cont = _continuous_domain(f, x, region)
    zeros = sp.solveset(f, x, domain=region)
    if _subset(region, cont)["truth"] != "true" or not (isinstance(zeros, sp.FiniteSet) or zeros is sp.S.EmptySet):
        lo_f, hi_f = to_float(lo), to_float(hi)
        if lo_f is None or hi_f is None or not (math.isfinite(lo_f) and math.isfinite(hi_f)):
            return {"sign": "unknown", "method": "none"}
        fn = _numeric_fn(f, x)
        ys = [fn(lo_f + (hi_f - lo_f) * i / NUMERIC_GRID) for i in range(NUMERIC_GRID + 1)]
        ys = [y for y in ys if y is not None]
        return {"sign": _sign_word(ys), "method": "numeric"}
    # Continuous with finitely many zeros: the sign is constant between consecutive zeros.
    cuts = sorted({lo, hi, *zeros}, key=lambda v: to_float(v) or 0.0)
    signs = []
    for a, b in itertools.pairwise(cuts):
        if a.is_infinite or b.is_infinite:
            mid = (b - 1) if a.is_infinite and not b.is_infinite else (a + 1 if not a.is_infinite else sp.Integer(0))
        else:
            mid = (a + b) / 2
        value = f.subs(x, mid)
        signs.append(1 if value.is_positive else -1 if value.is_negative else None)
    if None in signs:
        return {"sign": "unknown", "method": "none"}
    has_zero = len(zeros) > 0
    word = _sign_word(signs, has_zero)
    return {"sign": word, "method": "symbolic"}


def _sign_word(values, has_zero: bool | None = None) -> str:
    if not values:
        return "unknown"
    pos = any(v > 0 for v in values)
    neg = any(v < 0 for v in values)
    zero = has_zero if has_zero is not None else any(v == 0 for v in values)
    if pos and neg:
        return "mixed"
    if pos:
        return "nonnegative" if zero else "positive"
    if neg:
        return "nonpositive" if zero else "negative"
    return "zero"


@op(
    "degree",
    "introspect",
    "Polynomial degree in a variable; null when the expression is not a polynomial in it",
    args={"expr": EXPR, "var": VAR},
    required=["expr", "var"],
    result="integer | null",
    pure=True,
)
def degree(engine, expr, var):
    f, x = engine.expr(expr), engine.symbol(var)
    if not f.is_polynomial(x):
        return None
    return int(sp.degree(f, x))


@op(
    "free_symbols",
    "introspect",
    "Names of the free symbols, sorted",
    args={"expr": handle("expr", "relation", "boolean", "set")},
    required=["expr"],
    result="string[]",
    pure=True,
)
def free_symbols(engine, expr):
    return sorted(str(s) for s in engine.value(expr).free_symbols)


@op(
    "equals",
    "introspect",
    "Whether two expressions are equal for all values of their symbols",
    args={"a": scalar(), "b": scalar()},
    required=["a", "b"],
    result="truth",
    pure=True,
    budget_s=HEAVY_BUDGET_S,
)
def equals(engine, a, b):
    x, y = engine.expr(a), engine.expr(b)
    if sp.simplify(x - y) == 0:
        return truth(True)
    result = x.equals(y)
    if result is None:
        return truth(None, reason="SymPy could neither prove nor refute equality")
    return truth(bool(result))


@op(
    "set.is_empty",
    "introspect",
    "Whether a set is empty",
    args={"set": handle("set")},
    required=["set"],
    result="truth",
    pure=True,
)
def is_empty(engine, set):
    return from_fuzzy(engine.handles.get(set, ("set",)).is_empty)


@op(
    "set.contains",
    "introspect",
    "Whether a set contains a value",
    args={"set": handle("set"), "value": scalar()},
    required=["set", "value"],
    result="truth",
    pure=True,
)
def contains(engine, set, value):
    result = engine.handles.get(set, ("set",)).contains(engine.expr(value))
    if result in (sp.true, sp.false):
        return truth(bool(result))
    return truth(None, reason=f"undecided: {result}")


@op(
    "set.as_intervals",
    "introspect",
    "A real set as a list of intervals and isolated points, when it has that form",
    args={"set": handle("set")},
    required=["set"],
    result="{intervals: [...], exact: bool}",
    pure=True,
)
def as_intervals(engine, set):
    s = engine.handles.get(set, ("set",))
    parts = list(s.args) if isinstance(s, sp.Union) else [s]
    out = []
    for p in parts:
        if isinstance(p, sp.Interval):
            out.append(
                {
                    "lo": Out(p.start),
                    "hi": Out(p.end),
                    "lo_open": bool(p.left_open),
                    "hi_open": bool(p.right_open),
                    "approx": [to_float(p.start), to_float(p.end)],
                }
            )
        elif isinstance(p, sp.FiniteSet):
            for v in p:
                out.append({"point": Out(v), "approx": to_float(v)})
        elif p is sp.S.EmptySet:
            continue
        else:
            return {"intervals": [], "exact": False}
    return {"intervals": out, "exact": True}


@op(
    "evaluate",
    "introspect",
    "Exact value of an expression at given symbol values, by name",
    args={
        "expr": handle("expr", "relation", "boolean"),
        "at": {"type": "object", "properties": {}, "additionalProperties": scalar()},
    },
    required=["expr", "at"],
    result="handle",
    pure=True,
)
def evaluate(engine, expr, at):
    from .derive import subs

    return subs(engine, expr, at)


@op(
    "numeric",
    "introspect",
    "A decimal approximation (method: numeric); fails if free symbols remain",
    args={"expr": handle("expr"), "digits": {"type": "integer", "minimum": 1, "maximum": 100}},
    required=["expr"],
    result="{value: number, text: string}",
    pure=True,
)
def numeric(engine, expr, digits=15):
    f = engine.expr(expr)
    if f.free_symbols:
        raise CallError(
            "bad_args",
            f"the expression still has free symbols ({', '.join(sorted(map(str, f.free_symbols)))})",
            hint="substitute values with subs or evaluate first",
        )
    n = sp.N(f, int(digits))
    if n.has(sp.I):
        re_, im_ = n.as_real_imag()
        if abs(float(im_)) > 0:
            return {"value": None, "real": float(re_), "imag": float(im_), "text": sp.sstr(n), "method": "numeric"}
        n = re_
    try:
        value = float(n)
    except (TypeError, ValueError):
        raise CallError("math", f"{n} is not a real number") from None
    return {"value": value if math.isfinite(value) else None, "text": sp.sstr(n), "method": "numeric"}
