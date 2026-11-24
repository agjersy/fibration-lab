"""Certified sampling: plots that can't lie (ARCHITECTURE 9.1, WP-0.5).

``sample`` turns one scene object into drawable geometry for a viewport. For curves it does
more than sample: every segment of the polyline it returns is *checked* with interval
arithmetic (``mpmath.iv``) to lie within half a pixel of the true graph, and the places where
the mathematics is special (poles, removable holes, jumps, domain edges) come from SymPy, not
from guessing at samples. When anything cannot be checked, the result says ``sampled`` and
gives the reason. ``certified`` is never returned on a guess.

Contract
--------

JSON-RPC method ``sample``::

    params: {object: id, viewport: {x: [lo, hi], y: [lo, hi]}, pixels?: {w, h}, params?: {name: number}}
    result: {object, kind, status: "certified" | "sampled", reason?: string,
             segments: [[[x, y], ...], ...],
             markers: [{kind: "asymptote" | "hole" | "jump_end" | "point", x, y?, closed?: bool}],
             fill?: {polygons: [[[x, y], ...], ...]} | {rects: [[x0, y0, x1, y1], ...]},
             points?: [[x, y], ...],        # data series
             anchor?: {object: id},         # notes attached to an object
             total?: number,                # riemann sums
             stats: {evals, points, ms}}
    error:  {status: "error", error: {kind, detail, hint?}} (the engine's CallError shape)

* ``pixels`` defaults to 800 x 600. ``params`` overrides the scene's parameter values for this
  call only (a slider being dragged); every override must name a declared parameter.
* Parameters are substituted exactly (a value is read through its shortest decimal form, as
  ``Engine.value`` does), so the result is deterministic for the same inputs: work is bounded
  by evaluation counts, not by time. (A wall-clock backstop exists; if it fires, the result is
  ``sampled`` and says so.)
* Segments are polylines in data coordinates. They break at discontinuities and undefined
  parts, never connecting across a pole. Points far outside the viewport are thinned but kept
  finite, so the renderer clips.

What ``certified`` means
------------------------

* **Explicit curves y = f(x):** over the part of the viewport where f is defined, every point
  of the true graph lies within 0.5 px of the drawn polyline, except within 0.5 px (in x) of a
  marked breakpoint, where a marker shows what happens (asymptote, hole, jump). Each segment
  [a, b] is accepted only when one of these holds for the interval enclosure F of f over
  [a, b]:

  1. F and the segment are both entirely above (or below) the viewport: nothing visible.
  2. Every corner of the box [a, b] x F is within 0.5 px of the segment. Distance to a segment
     is convex, so the farthest point of the box is a corner, and the graph lies in the box.
  3. The centered form f(m) - c(m) + (f'([a, b]) - s)([a, b] - m) bounds the vertical gap
     between f and the chord c by 0.5 px.

  Otherwise the segment is split. A segment that is still unchecked at 1/20 px, or once the
  evaluation budget is spent, is kept as sampled and the whole result becomes ``sampled``.
  Breakpoints come from ``continuous_domain`` and ``singularities``, from where the arguments
  of floor, ceiling, sign, and Heaviside cross their jump values, and from the boundaries of
  Piecewise conditions. Each is classified with one-sided limits.

  The tests above bound the graph's distance *from* the polyline. The other direction (no
  drawn segment strays from the graph, such as a vertical connector across a jump) needs f
  to be continuous on the segment: then the graph passes through every value between the
  segment's ends. Continuity holds by construction. Sampling never crosses a breakpoint; the
  only discontinuous primitives (floor, ceiling, sign, Heaviside, Piecewise) are split at
  their jumps; and every other primitive is continuous wherever the interval evaluation shows
  it is defined and bounded. When a breakpoint cannot be found or classified (SymPy cannot
  solve for it, or the analysis runs out of time), the result is ``sampled``.
* **Parametric and polar curves:** each segment's parameter interval is enclosed in an x-y box
  whose corners must be within 0.5 px of the segment (test 2), or the box must be off-screen.
* **Regions:** a quadtree whose cells are proven inside or outside by evaluating the condition
  with interval arithmetic; boundary cells are resolved down to 0.35 px (cell diagonal under
  0.5 px). ``certified`` when every boundary cell reached that size.
* **Implicit curves:** traced by marching squares on a 1 px grid, after interval arithmetic
  rules out every cell the curve cannot pass through. Always ``sampled``: a cell may hold a
  piece of curve that does not change sign at its corners. The reason says how many such cells
  there were.
* **Points, vectors, data, Riemann sums, notes:** exact coordinates, ``certified``. Areas take
  the status of their curve.
"""

from __future__ import annotations

import itertools
import math
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

import mpmath
import numpy as np
import sympy as sp
from sympy.calculus.util import continuous_domain

from .budget import budget
from .errors import CallError
from .mathparse import read_math

iv = mpmath.iv

TOL_PX = 0.5  # the certified error bound, in pixels
MIN_PX = 0.05  # stop splitting a segment that is still unchecked at this width
REGION_LEAF_PX = 0.35  # region boundary cells; diagonal under TOL_PX
IMPLICIT_LEAF_PX = 1.0
QUAD_BASE_PX = 16.0
MAX_POINTS = 20_000  # per object
MAX_EVALS = 60_000  # interval evaluations per object
MAX_BREAKPOINTS = 400
ANALYSIS_BUDGET_S = 2.5
WALL_CLOCK_S = 5.0
FAR = 1e300  # infinite sample values are clamped here (off-screen; keeps JSON finite)


# ---- interval arithmetic over SymPy expressions -------------------------------------


class Unsupported(Exception):
    """The expression uses something interval arithmetic is not implemented for."""


class Undefined(Exception):
    """The function is undefined on part of the box (e.g. log of an interval reaching 0)."""


def _ends(X) -> tuple[mpmath.mpf, mpmath.mpf]:
    a, b = X._mpi_
    return mpmath.mpf(a), mpmath.mpf(b)


def _hull(values: list) -> Any:
    lo = min(_ends(v)[0] for v in values)
    hi = max(_ends(v)[1] for v in values)
    return iv.mpf([lo, hi])


def _point(v) -> Any:
    return iv.mpf([v, v])


def _widen(lo: mpmath.mpf, hi: mpmath.mpf) -> Any:
    tiny = mpmath.mpf(2) ** -1070
    return iv.mpf([lo - abs(lo) * mpmath.mpf(2) ** -80 - tiny, hi + abs(hi) * mpmath.mpf(2) ** -80 + tiny])


def _monotone(g: Callable, increasing: bool = True, domain: tuple[float, float] | None = None):
    """Enclosure of a monotone function from its endpoint values, computed at 90 bits and
    widened outward; for functions mpmath.iv does not provide."""

    def run(X):
        a, b = _ends(X)
        if domain is not None and (a < domain[0] or b > domain[1]):
            raise Undefined()
        with mpmath.workprec(90):
            ga, gb = g(a), g(b)
        return _widen(ga, gb) if increasing else _widen(gb, ga)

    return run


def _cosh(X):
    a, b = _ends(X)
    with mpmath.workprec(90):
        ca, cb = mpmath.cosh(a), mpmath.cosh(b)
    if a <= 0 <= b:
        return _widen(mpmath.mpf(1), max(ca, cb))
    return _widen(min(ca, cb), max(ca, cb))


def _abs(X):
    a, b = _ends(X)
    if a >= 0:
        return X
    if b <= 0:
        return -X
    return iv.mpf([0, max(-a, b)])


def _floor(X):
    a, b = _ends(X)
    return iv.mpf([mpmath.floor(a), mpmath.floor(b)])


def _ceiling(X):
    a, b = _ends(X)
    return iv.mpf([mpmath.ceil(a), mpmath.ceil(b)])


def _step(X, neg, zero, pos):
    a, b = _ends(X)
    vals = []
    if a < 0:
        vals.append(neg)
    if a <= 0 <= b:
        vals.append(zero)
    if b > 0:
        vals.append(pos)
    return _hull([iv.mpf(v) for v in vals])


def _log(X):
    a, _b = _ends(X)
    if a <= 0:
        raise Undefined()
    return iv.log(X)


def _sqrt(X):
    a, _ = _ends(X)
    if a < 0:
        raise Undefined()
    return iv.sqrt(X)


def _finite(X) -> bool:
    a, b = _ends(X)
    return mpmath.isfinite(a) and mpmath.isfinite(b)


_UNARY: dict[type, Callable] = {
    sp.sin: iv.sin,
    sp.cos: iv.cos,
    sp.tan: iv.tan,
    sp.exp: iv.exp,
    sp.log: _log,
    sp.erf: _monotone(mpmath.erf),  # mpmath.iv has no erf
    sp.Abs: _abs,
    sp.floor: _floor,
    sp.ceiling: _ceiling,
    sp.sign: lambda X: _step(X, -1, 0, 1),
    sp.atan: _monotone(mpmath.atan),
    sp.asin: _monotone(mpmath.asin, domain=(-1, 1)),
    sp.acos: _monotone(mpmath.acos, increasing=False, domain=(-1, 1)),
    sp.sinh: _monotone(mpmath.sinh),
    sp.tanh: _monotone(mpmath.tanh),
    sp.asinh: _monotone(mpmath.asinh),
    sp.acosh: _monotone(mpmath.acosh, domain=(1, math.inf)),
    sp.cosh: _cosh,
    sp.re: lambda X: X,
    sp.im: lambda X: iv.mpf(0),
    sp.cot: lambda X: 1 / iv.tan(X),
    sp.sec: lambda X: 1 / iv.cos(X),
    sp.csc: lambda X: 1 / iv.sin(X),
}


def _const(e) -> Any:
    if e.is_Integer:
        return iv.mpf(int(e))
    if e.is_Rational:
        return iv.mpf(int(e.p)) / iv.mpf(int(e.q))
    if e.is_Float:
        return iv.mpf(str(e))
    if e is sp.pi:
        return iv.pi
    if e is sp.E:
        return iv.e
    if isinstance(e, sp.NumberSymbol):
        with mpmath.workprec(90):
            v = mpmath.mpf(sp.N(e, 30))
        return _widen(v, v)
    raise Unsupported(f"the constant {e}")


def compile_iv(expr, variables: tuple) -> Callable:
    """A function of interval arguments that encloses ``expr`` over a box."""
    index = {v: i for i, v in enumerate(variables)}

    def c(e) -> Callable:
        if e in index:
            i = index[e]
            return lambda xs: xs[i]
        if e.is_Number or isinstance(e, sp.NumberSymbol):
            if e.is_infinite or e is sp.nan or not e.is_real:
                raise Unsupported(f"the value {e}")
            k = _const(e)
            return lambda xs: k
        if e.is_Symbol:
            raise Unsupported(f"the free symbol {e}")
        if isinstance(e, sp.Add):
            terms = [c(a) for a in e.args]

            def add(xs):
                total = terms[0](xs)
                for t in terms[1:]:
                    total = total + t(xs)
                return total

            return add
        if isinstance(e, sp.Mul):
            factors = [c(a) for a in e.args]

            def mul(xs):
                total = factors[0](xs)
                for f in factors[1:]:
                    total = total * f(xs)
                return total

            return mul
        if isinstance(e, sp.Pow):
            return _compile_pow(e, c)
        if isinstance(e, sp.Heaviside):
            arg = c(e.args[0])
            h0 = sp.Rational(1, 2) if len(e.args) < 2 else e.args[1]
            h0v = _const(sp.nsimplify(h0))
            return lambda xs: _step(arg(xs), iv.mpf(0), h0v, iv.mpf(1))
        if isinstance(e, sp.Max | sp.Min):
            args = [c(a) for a in e.args]
            pick = max if isinstance(e, sp.Max) else min

            def extreme(xs):
                vals = [_ends(a(xs)) for a in args]
                return iv.mpf([pick(v[0] for v in vals), pick(v[1] for v in vals)])

            return extreme
        if isinstance(e, sp.Piecewise):
            return _compile_piecewise(e, c)
        fn = _UNARY.get(type(e))
        if fn is not None and len(e.args) == 1:
            arg = c(e.args[0])
            return lambda xs: fn(arg(xs))
        raise Unsupported(type(e).__name__)

    body = c(sp.sympify(expr))

    def run(*xs):
        try:
            out = body(xs)
        except (ZeroDivisionError, mpmath.libmp.ComplexResult):
            raise Undefined() from None
        return out if hasattr(out, "_mpi_") else iv.mpf(out)

    return run


def _compile_pow(e, c) -> Callable:
    base, exp = e.args
    b = c(base)
    if exp.is_Integer:
        n = int(exp)
        if n >= 0:
            return lambda xs: b(xs) ** n

        def recip(xs):
            B = b(xs)
            lo, hi = _ends(B)
            if lo <= 0 <= hi:
                raise Undefined()
            return 1 / (B ** (-n))

        return recip
    if exp == sp.Rational(1, 2):
        return lambda xs: _sqrt(b(xs))
    if exp == sp.Rational(-1, 2):

        def rsqrt(xs):
            B = b(xs)
            if _ends(B)[0] <= 0:
                raise Undefined()
            return 1 / iv.sqrt(B)

        return rsqrt
    # Any other exponent: real only for a positive base (SymPy's principal branch).
    ex = c(exp)

    def power(xs):
        B = b(xs)
        lo, _ = _ends(B)
        if lo < 0 or (lo == 0 and not (exp.is_number and exp.is_positive)):
            raise Undefined()
        if lo == 0:
            return B ** ex(xs)
        return iv.exp(ex(xs) * iv.log(B))

    return power


def _compile_cond(cond, c) -> Callable:
    """Three-valued condition over a box: True (everywhere), False (nowhere), None (unknown)."""
    if cond is sp.true or cond is True:
        return lambda xs: True
    if cond is sp.false or cond is False:
        return lambda xs: False
    if isinstance(cond, sp.core.relational.Relational):
        diff = c(cond.lhs - cond.rhs)
        op = cond.rel_op

        def rel(xs):
            lo, hi = _ends(diff(xs))
            if op == "<":
                return True if hi < 0 else False if lo >= 0 else None
            if op == "<=":
                return True if hi <= 0 else False if lo > 0 else None
            if op == ">":
                return True if lo > 0 else False if hi <= 0 else None
            if op == ">=":
                return True if lo >= 0 else False if hi < 0 else None
            if op == "==":
                return True if lo == hi == 0 else False if (lo > 0 or hi < 0) else None
            if op == "!=":
                return False if lo == hi == 0 else True if (lo > 0 or hi < 0) else None
            raise Unsupported(f"the relation {op}")

        return rel
    if isinstance(cond, sp.And | sp.Or):
        parts = [_compile_cond(a, c) for a in cond.args]
        is_and = isinstance(cond, sp.And)

        def combine(xs):
            vals = [p(xs) for p in parts]
            if is_and:
                return False if False in vals else True if all(v is True for v in vals) else None
            return True if True in vals else False if all(v is False for v in vals) else None

        return combine
    if isinstance(cond, sp.Not):
        inner = _compile_cond(cond.args[0], c)

        def negate(xs):
            v = inner(xs)
            return None if v is None else not v

        return negate
    raise Unsupported(f"the condition {cond}")


def _compile_piecewise(e, c) -> Callable:
    branches = [(c(expr), _compile_cond(cond, c)) for expr, cond in e.args]

    def run(xs):
        values = []
        for value, cond in branches:
            t = cond(xs)
            if t is False:
                continue
            values.append(value(xs))
            if t is True:
                return _hull(values)
        raise Undefined()  # part of the box may be covered by no branch

    return run


def compile_cond(cond, variables: tuple) -> Callable:
    """A three-valued function of interval arguments for a condition over a box."""

    def c(e):
        fn = compile_iv(e, variables)
        return lambda xs: fn(*xs)

    return _compile_cond(cond, c)


# ---- numeric evaluation ---------------------------------------------------------------


def _numeric(expr, variables: tuple) -> Callable[..., float]:
    """A float evaluator; NaN where the expression is undefined, +-FAR where it overflows."""
    try:
        fn = sp.lambdify(variables, expr, modules="numpy")
        mp_fn = None
    except Exception:  # an expression numpy cannot print
        fn = None
        mp_fn = sp.lambdify(variables, expr, modules="mpmath")

    def call(*vals: float) -> float:
        try:
            with np.errstate(all="ignore"):
                y = fn(*vals) if fn is not None else mp_fn(*(mpmath.mpf(v) for v in vals))
            if isinstance(y, complex | mpmath.mpc) or np.iscomplexobj(y):
                y = complex(y)
                if abs(y.imag) > 1e-12 * max(1.0, abs(y.real)):
                    return math.nan
                y = y.real
            y = float(y)
        except (ZeroDivisionError, ValueError, TypeError, OverflowError, FloatingPointError):
            return math.nan
        if math.isinf(y):
            return math.copysign(FAR, y)
        return y

    return call


# ---- views and bookkeeping ------------------------------------------------------------


@dataclass
class View:
    xlo: float
    xhi: float
    ylo: float
    yhi: float
    w: float
    h: float

    @property
    def sx(self) -> float:
        return self.w / (self.xhi - self.xlo)

    @property
    def sy(self) -> float:
        return self.h / (self.yhi - self.ylo)

    def px(self, x: float, y: float) -> tuple[float, float]:
        return (x - self.xlo) * self.sx, (self.yhi - y) * self.sy


@dataclass
class Work:
    deadline: float
    evals: int = 0
    points: int = 0
    exhausted: str | None = None
    uncertified: list[float] = field(default_factory=list)
    reasons: list[str] = field(default_factory=list)

    def spend(self, n: int = 1) -> bool:
        """Count interval evaluations; False once the budget is spent."""
        self.evals += n
        if self.exhausted is None:
            if self.evals > MAX_EVALS or self.points > MAX_POINTS:
                self.exhausted = "the sampling budget was spent before every segment was checked"
            elif time.perf_counter() > self.deadline:
                self.exhausted = "the time limit was reached before every segment was checked"
        return self.exhausted is None

    def miss(self, x: float) -> None:
        self.uncertified.append(x)

    def status(self) -> tuple[str, str | None]:
        reasons = list(self.reasons)
        if self.uncertified:
            xs = sorted(set(round(v, 6) for v in self.uncertified))
            shown = ", ".join(f"{v:g}" for v in xs[:3]) + (" ..." if len(xs) > 3 else "")
            reasons.append(f"could not check the curve to {TOL_PX:g} px near x ≈ {shown}")
        if self.exhausted:
            reasons.append(self.exhausted)
        if reasons:
            return "sampled", "; ".join(dict.fromkeys(reasons))
        return "certified", None


def _seg_dist(p: tuple[float, float], a: tuple[float, float], b: tuple[float, float]) -> float:
    dx, dy = b[0] - a[0], b[1] - a[1]
    length = dx * dx + dy * dy
    if length == 0 or not math.isfinite(length):
        return math.hypot(p[0] - a[0], p[1] - a[1])
    t = max(0.0, min(1.0, ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / length))
    return math.hypot(p[0] - (a[0] + t * dx), p[1] - (a[1] + t * dy))


def _box_near_segment(view: View, box: tuple[float, float, float, float], a, b) -> bool:
    """Every point of the data box [x0, x1] x [y0, y1] is within TOL_PX of segment a-b."""
    x0, x1, y0, y1 = box
    if not all(math.isfinite(v) for v in box):
        return False
    pa, pb = view.px(*a), view.px(*b)
    return all(_seg_dist(view.px(x, y), pa, pb) <= TOL_PX for x in (x0, x1) for y in (y0, y1))


def _off_screen(view: View, box: tuple[float, float, float, float], a, b) -> bool:
    """The box and the segment are both beyond the same edge of the viewport."""
    x0, x1, y0, y1 = box
    tx, ty = TOL_PX / view.sx, TOL_PX / view.sy
    return (
        (y0 > view.yhi + ty and min(a[1], b[1]) > view.yhi + ty)
        or (y1 < view.ylo - ty and max(a[1], b[1]) < view.ylo - ty)
        or (x0 > view.xhi + tx and min(a[0], b[0]) > view.xhi + tx)
        or (x1 < view.xlo - tx and max(a[0], b[0]) < view.xlo - tx)
    )


def _thin(polyline: list[list[float]], view: View) -> list[list[float]]:
    """Drop interior points of runs that are all above (or all below) the viewport."""
    if len(polyline) < 3:
        return polyline

    def side(p):
        return 1 if p[1] > view.yhi else -1 if p[1] < view.ylo else 0

    out = [polyline[0]]
    for i in range(1, len(polyline) - 1):
        s = side(polyline[i])
        if s != 0 and side(polyline[i - 1]) == s and side(polyline[i + 1]) == s:
            continue
        out.append(polyline[i])
    out.append(polyline[-1])
    return out


# ---- explicit curves: symbolic analysis --------------------------------------------------


@dataclass
class Piece:
    a: sp.Expr
    b: sp.Expr
    a_closed: bool
    b_closed: bool
    join_next: bool = False  # continuous through b into the next piece


def _real_value(v) -> sp.Expr | None:
    if v is None or v.has(sp.nan, sp.zoo) or not v.is_number:
        return None
    if v.is_extended_real is False or v.is_finite is False:
        return None
    if v.is_extended_real is None:
        n = sp.N(v, 30)
        if not n.is_real:
            return None
    return v


def _same(a: sp.Expr, b: sp.Expr) -> bool:
    if a == b:
        return True
    try:
        if sp.simplify(a - b) == 0:
            return True
        return abs(float(sp.N(a - b, 30))) < 1e-15
    except (TypeError, ValueError):
        return False


def _jump_points(f, x, region: sp.Interval, work: Work) -> set:
    """Points where floor, ceiling, sign, Heaviside, or a Piecewise condition can jump."""
    points: set = set()
    targets: list[tuple[sp.Expr, list[int] | None]] = []
    for atom in f.atoms(sp.floor, sp.ceiling):
        targets.append((atom.args[0], None))
    for atom in f.atoms(sp.sign, sp.Heaviside, sp.Abs):
        targets.append((atom.args[0], [0]))
    for pw in f.atoms(sp.Piecewise):
        for _, cond in pw.args:
            for rel in cond.atoms(sp.core.relational.Relational):
                targets.append((rel.lhs - rel.rhs, [0]))
    for g, values in targets:
        if x not in g.free_symbols:
            continue
        if values is None:
            try:
                G = compile_iv(g, (x,))(iv.mpf([float(region.start), float(region.end)]))
            except (Unsupported, Undefined):
                work.reasons.append(f"could not bound {g} to find where it jumps")
                continue
            lo, hi = _ends(G)
            if not (mpmath.isfinite(lo) and mpmath.isfinite(hi)):
                work.reasons.append(f"{g} is unbounded in view, so its jumps cannot all be found")
                continue
            values = list(range(int(mpmath.ceil(lo)), int(mpmath.floor(hi)) + 1))
            if len(values) > MAX_BREAKPOINTS:
                work.reasons.append(f"more than {MAX_BREAKPOINTS} jumps in view")
                continue
        for k in values:
            sols = sp.solveset(sp.Eq(g, k), x, domain=region)
            if isinstance(sols, sp.FiniteSet):
                points.update(sols)
            elif sols is not sp.S.EmptySet:
                work.reasons.append(f"could not find where {g} = {k}")
    return points


def _components(cd) -> list | None:
    """A real set as intervals (and isolated points), or None when it has another form."""
    if cd is sp.S.EmptySet:
        return []
    parts = list(cd.args) if isinstance(cd, sp.Union) else [cd]
    out = []
    for p in parts:
        if isinstance(p, sp.Interval | sp.FiniteSet):
            out.append(p)
        else:
            return None
    return out


def _analyze(
    f, x, region: sp.Interval, work: Work, fnum=None, view: View | None = None
) -> tuple[list[Piece], list[dict], list[sp.Expr]]:
    """Pieces of the domain to sample, markers at breakpoints, and isolated defined points."""
    markers: list[dict] = []
    try:
        comps = _components(continuous_domain(f, x, region))
    except NotImplementedError:
        comps = None
    if comps is None:
        try:
            sing = sp.singularities(f, x, region)
        except NotImplementedError:
            sing = None
        if isinstance(sing, sp.FiniteSet) or sing is sp.S.EmptySet:
            comps = _components(sp.Complement(region, sing)) or [region]
        else:
            work.reasons.append("could not establish where the function is continuous")
            comps = [region]
    intervals = [c for c in comps if isinstance(c, sp.Interval)]
    isolated = [p for c in comps if isinstance(c, sp.FiniteSet) for p in c]
    splits = _jump_points(f, x, region, work)

    pieces: list[Piece] = []
    for comp in sorted(intervals, key=lambda i: float(i.start)):
        inner = sorted((p for p in splits if comp.start < p < comp.end), key=float)
        cuts = [comp.start, *inner, comp.end]
        for i, (a, b) in enumerate(itertools.pairwise(cuts)):
            # SymPy reports openness as sympy.true/false, so test truthiness, never identity.
            # An outer end that is itself a jump point is classified like any other: the
            # value there may not be the limit from inside (floor(x) at the right edge).
            a_closed = i == 0 and not comp.left_open and comp.start not in splits
            b_closed = i == len(cuts) - 2 and not comp.right_open and comp.end not in splits
            pieces.append(Piece(a, b, a_closed, b_closed))

    # Classify every open end and every split point.
    ends: dict[sp.Expr, dict[str, int]] = {}
    for i, p in enumerate(pieces):
        if not p.a_closed:
            ends.setdefault(p.a, {})["right"] = i
        if not p.b_closed:
            ends.setdefault(p.b, {})["left"] = i
    if len(ends) > MAX_BREAKPOINTS:
        work.reasons.append(f"more than {MAX_BREAKPOINTS} breakpoints in view")
        return pieces, markers, isolated
    for point in sorted(ends, key=float):
        sides = ends[point]
        _classify(f, x, point, sides, pieces, markers, work, fnum, view)
    for p in isolated:
        v = _real_value(f.subs(x, p))
        if v is not None:
            markers.append({"kind": "point", "x": float(p), "y": float(v), "closed": True})
    return pieces, markers, isolated


_STEP_ATOMS = (sp.floor, sp.ceiling, sp.sign, sp.Heaviside)


def _settle_steps(f, x, p, side: str):
    """f with every step function replaced by the constant it equals just beside p.

    floor(g), ceiling(g), sign(g) and Heaviside(g) are constant on a one-sided neighbourhood
    of p whenever g is continuous and strictly monotone there (g'(p) != 0), or g(p) is not a
    step point at all. Under those conditions the replacement is exact, and the one-sided
    limit of f is the limit of the settled expression, which no longer jumps at p. Returns
    None when a step cannot be settled this way; the caller then asks SymPy.
    """
    replacements = {}
    for atom in f.atoms(*_STEP_ATOMS):
        g = atom.args[0]
        if x not in g.free_symbols:
            continue
        gp = g.subs(x, p)
        if not (gp.is_real and gp.is_finite):
            return None
        at_step = gp.is_integer if isinstance(atom, sp.floor | sp.ceiling) else gp.is_zero
        if at_step is None:
            return None
        if not at_step:
            # g is away from a step point; the atom takes its value at p on both sides,
            # provided g is continuous at p (it is real and finite there and has no steps).
            if g.atoms(*_STEP_ATOMS, sp.Piecewise):
                return None
            replacements[atom] = atom.subs(x, p)
            continue
        slope = sp.diff(g, x).subs(x, p)
        if g.atoms(*_STEP_ATOMS, sp.Piecewise) or not (slope.is_real and slope.is_finite):
            return None
        if slope.is_positive:
            rising = True
        elif slope.is_negative:
            rising = False
        else:
            return None  # a stationary point of g: the side cannot be read off the slope
        above = rising == (side == "right")  # is g just above its value at p on this side?
        if isinstance(atom, sp.floor):
            replacements[atom] = gp if above else gp - 1
        elif isinstance(atom, sp.ceiling):
            replacements[atom] = gp + 1 if above else gp
        else:  # sign and Heaviside step at g = 0
            replacements[atom] = (
                sp.Integer(1) if above else (sp.Integer(-1) if isinstance(atom, sp.sign) else sp.Integer(0))
            )
    return f.xreplace(replacements) if replacements else None


def one_sided_limit(f, x, p, side: str):
    """lim f(x) as x -> p from one side.

    Step functions are settled first (see ``_settle_steps``): SymPy's limit of an expression
    with ``floor`` can take minutes. If the settled expression has a finite real value at p
    and no steps or pieces left, that value is the limit, since the expression is then a
    composition of functions continuous where they are finite. Otherwise the limit is
    computed as h -> 0+ of f(p -+ h) with h > 0: SymPy's own ``limit(f, x, p, '-')`` is wrong
    for some Piecewise expressions (it returns the right-hand value); the substitution form
    is not. Every result is cross-checked numerically by the caller.
    """
    settled = _settle_steps(f, x, p, side)
    if settled is not None and not settled.atoms(*_STEP_ATOMS, sp.Piecewise):
        value = settled.subs(x, p)
        if value.is_real and value.is_finite:
            return sp.nsimplify(value) if value.is_Float else value
        f = settled
    h = sp.Dummy("h", positive=True)
    shifted = f.subs(x, p - h if side == "left" else p + h)
    return sp.limit(shifted, h, 0, "+")


def _consistent(fnum, view: View | None, p, side: str, expected: sp.Expr) -> bool:
    """Numeric cross-check of a symbolic limit: do the values beside p approach it?

    The function is evaluated at three distances from p, each a hundred times closer. The
    limit is accepted when the closest value is within TOL_PX of it, or when the gap shrinks by
    a fifth or more at every step (a steep but convergent approach such as x**(1/9) at 0). A wrong limit
    across a jump leaves a gap that does not shrink, and is rejected.
    """
    if fnum is None or view is None:
        return True
    pf = float(p)
    target = float(expected)
    span = view.xhi - view.xlo
    floor = abs(pf) * 1e-13
    gaps = []
    for scale in (1e-6, 1e-8, 1e-10):
        eps = max(span * scale, floor)
        y = fnum(pf - eps if side == "left" else pf + eps)
        if math.isnan(y):
            return False
        gaps.append(abs(y - target) * view.sy)
    if gaps[-1] <= TOL_PX:
        return True
    return all(b <= 0.8 * a for a, b in itertools.pairwise(gaps))


def _classify(
    f, x, p, sides: dict[str, int], pieces: list[Piece], markers: list[dict], work: Work, fnum=None, view=None
) -> None:
    limits: dict[str, Any] = {}
    for side in ("left", "right"):
        if side not in sides:
            continue
        try:
            lim = one_sided_limit(f, x, p, side)
        except (NotImplementedError, ValueError):
            lim = None
        limits[side] = lim
    value = _real_value(f.subs(x, p))
    px = float(p)

    for lim in limits.values():
        if isinstance(lim, sp.AccumBounds):
            work.reasons.append(f"the function oscillates without a limit near x = {p}")
            return
        if lim is None or lim.has(sp.Limit):
            work.reasons.append(f"could not decide what happens at x = {p}")
            return
    if any(lim.is_infinite or lim is sp.zoo for lim in limits.values()):
        markers.append({"kind": "asymptote", "x": px})
        return
    finite = {side: _real_value(lim) for side, lim in limits.items()}
    if any(v is None for v in finite.values()):
        work.reasons.append(f"could not decide what happens at x = {p}")
        return
    if not all(_consistent(fnum, view, p, side, lim) for side, lim in finite.items()):
        work.reasons.append(f"the symbolic limits at x = {p} disagree with evaluation beside it")
        return

    if len(finite) == 2:
        left, right = finite["left"], finite["right"]
        if _same(left, right):
            if value is not None and _same(value, left):
                # Continuous after all (a kink, or a split that did not matter): join the pieces.
                pieces[sides["left"]].b_closed = True
                pieces[sides["right"]].a_closed = True
                pieces[sides["left"]].join_next = True
                return
            markers.append({"kind": "hole", "x": px, "y": float(left), "closed": False})
            if value is not None:
                markers.append({"kind": "point", "x": px, "y": float(value), "closed": True})
            return
        for side, lim in (("left", left), ("right", right)):
            closed = value is not None and _same(value, lim)
            markers.append({"kind": "jump_end", "x": px, "y": float(lim), "closed": closed})
            if closed:
                if side == "left":
                    pieces[sides["left"]].b_closed = True
                else:
                    pieces[sides["right"]].a_closed = True
        if value is not None and not (_same(value, left) or _same(value, right)):
            markers.append({"kind": "point", "x": px, "y": float(value), "closed": True})
        return

    # One-sided: the edge of the domain (or of the viewport region).
    ((side, lim),) = finite.items()
    if value is not None and _same(value, lim):
        if side == "left":
            pieces[sides["left"]].b_closed = True
        else:
            pieces[sides["right"]].a_closed = True
        return
    markers.append({"kind": "hole", "x": px, "y": float(lim), "closed": False})
    if value is not None:
        markers.append({"kind": "point", "x": px, "y": float(value), "closed": True})


# ---- explicit curves: certified adaptive sampling --------------------------------------


def _certify_segment(fiv, dfiv, view: View, a: float, b: float, ya: float, yb: float, work: Work) -> bool:
    if not work.spend():
        return False
    try:
        F = fiv(iv.mpf([a, b]))
    except (Undefined, Unsupported):
        return False
    lo, hi = (float(v) for v in _ends(F))
    box = (a, b, lo, hi)
    if _off_screen(view, box, (a, ya), (b, yb)):
        return True
    if not (math.isfinite(lo) and math.isfinite(hi)):
        return False
    if _box_near_segment(view, box, (a, ya), (b, yb)):
        return True
    if dfiv is None or not work.spend(2):
        return False
    # Centered form: f(x) - c(x) in f(m) - c(m) + (f'(X) - s)(X - m).
    try:
        m = (a + b) / 2
        s = (yb - ya) / (b - a)
        X = iv.mpf([a, b])
        cm = iv.mpf(ya) + iv.mpf(s) * (iv.mpf(m) - iv.mpf(a))
        D = (fiv(iv.mpf(m)) - cm) + (dfiv(X) - iv.mpf(s)) * (X - iv.mpf(m))
    except (Undefined, Unsupported):
        return False
    dlo, dhi = (float(v) for v in _ends(D))
    return max(abs(dlo), abs(dhi)) * view.sy <= TOL_PX


def _looks_like_a_jump(fnum, view: View, a: float, ya: float, b: float, yb: float) -> bool:
    """Whether f jumps between a and b, judged numerically.

    Used only for a segment that could not be checked. Bisect towards the half with the larger
    change: across a jump the change stays the same however narrow the interval gets, while a
    continuous function, however steep, changes less and less.
    """
    gap = abs(yb - ya) * view.sy
    if gap <= 1.0:
        return False
    for _ in range(40):
        m = (a + b) / 2
        if m in (a, b):
            break
        ym = fnum(m)
        if math.isnan(ym):
            return False
        if abs(ym - ya) >= abs(yb - ym):
            b, yb = m, ym
        else:
            a, ya = m, ym
    return abs(yb - ya) * view.sy >= 0.9 * gap


def _sample_piece(fnum, fiv, dfiv, view: View, a: float, b: float, work: Work) -> list[list[list[float]]]:
    """Adaptive sampling of one continuous piece; each accepted segment is certified when possible."""
    n0 = max(8, min(400, int((b - a) * view.sx / 4) + 1))
    xs = [a + (b - a) * i / n0 for i in range(n0)] + [b]
    ys = [fnum(v) for v in xs]
    work.points += len(xs)
    polylines: list[list[list[float]]] = [[]]

    def emit(xv: float, yv: float) -> None:
        if math.isnan(yv):
            if polylines[-1]:
                polylines.append([])
            return
        polylines[-1].append([xv, yv])

    emit(xs[0], ys[0])
    for i in range(n0):
        stack = [(xs[i], ys[i], xs[i + 1], ys[i + 1])]
        while stack:
            sa, sya, sb, syb = stack.pop()
            width_px = (sb - sa) * view.sx
            finite = not (math.isnan(sya) or math.isnan(syb))
            if fiv is not None and finite:
                ok = work.exhausted is None and _certify_segment(fiv, dfiv, view, sa, sb, sya, syb, work)
            else:
                ok = False
                if finite and fiv is None:
                    # No interval arithmetic: split on visible curvature only (sampled anyway).
                    sm = (sa + sb) / 2
                    ym = fnum(sm)
                    ok = width_px <= 2 or (
                        not math.isnan(ym)
                        and _seg_dist(view.px(sm, ym), view.px(sa, sya), view.px(sb, syb)) <= TOL_PX / 2
                    )
            if ok and width_px <= 1.0 and _looks_like_a_jump(fnum, view, sa, sya, sb, syb):
                # A narrow, steep segment can pass the distance check across a jump (the
                # near-vertical line stays close to the graph's enclosure). Where breakpoint
                # analysis ran, pieces never span a jump, so this only fires without it.
                work.miss(sa)
                if polylines[-1]:
                    polylines.append([])
                emit(sb, syb)
                continue
            if ok:
                emit(sb, syb)
                continue
            if width_px <= MIN_PX or work.exhausted is not None or work.points >= MAX_POINTS:
                if (fiv is not None and finite) or not finite:
                    work.miss(sa)
                if finite and _looks_like_a_jump(fnum, view, sa, sya, sb, syb):
                    # An unanalysed jump: end the line here and start a new one, so the
                    # drawing has no riser. The miss above already makes the result sampled.
                    work.miss(sa)
                    if polylines[-1]:
                        polylines.append([])
                emit(sb, syb)
                continue
            sm = (sa + sb) / 2
            ym = fnum(sm)
            work.points += 1
            stack.append((sm, ym, sb, syb))
            stack.append((sa, sya, sm, ym))
    return [_thin(_clamp(p, view), view) for p in polylines if len(p) >= 2 and not _all_far(p)]


def _all_far(polyline: list[list[float]]) -> bool:
    """A run of samples that all overflowed: a division by zero over a whole stretch (1/floor(x)
    on [0, 1)), which is an undefined stretch, not a curve."""
    return all(abs(y) >= FAR for _, y in polyline)


def _clamp(polyline: list[list[float]], view: View) -> list[list[float]]:
    """Bring overflowed samples to a million view-heights off screen. The on-screen part of a
    segment that leaves the view moves by less than a millionth of a pixel column, and the
    coordinates stay representable in a renderer's 32-bit floats."""
    reach = 1e6 * (view.yhi - view.ylo)
    lo, hi = view.ylo - reach, view.yhi + reach
    return [[x, min(max(y, lo), hi)] for x, y in polyline]


def _explicit(f, x, lo_exact, hi_exact, view: View, work: Work) -> tuple[list, list]:
    """Segments and markers of y = f(x) for x in [lo, hi] (already clipped to the viewport)."""
    region = sp.Interval(lo_exact, hi_exact)
    if not (lo_exact < hi_exact):
        return [], []
    try:
        with budget(ANALYSIS_BUDGET_S):
            pieces, markers, _ = _analyze(f, x, region, work, _numeric(f, (x,)), view)
    except CallError:
        work.reasons.append("the symbolic analysis of breakpoints ran out of time")
        pieces, markers = [Piece(lo_exact, hi_exact, True, True)], []
    try:
        fiv = compile_iv(f, (x,))
        fiv1 = lambda X: fiv(X)  # noqa: E731
    except Unsupported as exc:
        work.reasons.append(f"interval arithmetic is not available for {exc}")
        fiv1 = None
    dfiv = None
    if fiv1 is not None:
        try:
            df = sp.diff(f, x)
            if not df.has(sp.Derivative, sp.DiracDelta):
                dfiv = compile_iv(df, (x,))
        except Unsupported:
            dfiv = None
    fnum = _numeric(f, (x,))
    excl = TOL_PX / view.sx
    segments: list = []
    previous_joined = False
    for piece in pieces:
        a = float(piece.a) if piece.a_closed else float(piece.a) + excl
        b = float(piece.b) if piece.b_closed else float(piece.b) - excl
        if b - a <= 0:
            previous_joined = piece.join_next
            continue
        polys = _sample_piece(fnum, fiv1, dfiv, view, a, b, work)
        if previous_joined and segments and polys and segments[-1][-1][0] == polys[0][0][0]:
            segments[-1].extend(polys[0][1:])
            polys = polys[1:]
        segments.extend(polys)
        previous_joined = piece.join_next
    return segments, markers


# ---- parametric curves -------------------------------------------------------------


def _parametric(fx, fy, t, t0, t1, view: View, work: Work) -> list:
    region = sp.Interval(t0, t1)
    pieces = [(t0, t1, True, True)]
    try:
        with budget(ANALYSIS_BUDGET_S):
            cd = continuous_domain(fx, t, region).intersect(continuous_domain(fy, t, region))
            comps = _components(cd)
        if comps is None:
            work.reasons.append("could not find where the curve is continuous")
        else:
            pieces = [
                (c.start, c.end, not c.left_open, not c.right_open)
                for c in comps
                if isinstance(c, sp.Interval)
            ]
            gaps = [c.start for c in comps if isinstance(c, sp.Interval) and c.left_open and c.start > t0]
            if gaps:
                work.reasons.append(
                    "the curve is discontinuous at t = " + ", ".join(str(g) for g in gaps[:3])
                )
    except (NotImplementedError, CallError):
        work.reasons.append("could not find where the curve is continuous")
    try:
        xiv, yiv = compile_iv(fx, (t,)), compile_iv(fy, (t,))
    except Unsupported as exc:
        work.reasons.append(f"interval arithmetic is not available for {exc}")
        xiv = yiv = None
    xnum, ynum = _numeric(fx, (t,)), _numeric(fy, (t,))
    span = float(t1 - t0)
    excl = span * 1e-9
    segments = []
    for a_e, b_e, a_closed, b_closed in pieces:
        a = float(a_e) if a_closed else float(a_e) + excl
        b = float(b_e) if b_closed else float(b_e) - excl
        if b <= a:
            continue
        n0 = 256
        ts = [a + (b - a) * i / n0 for i in range(n0)] + [b]
        pts = [(xnum(v), ynum(v)) for v in ts]
        work.points += len(ts)
        poly: list[list[float]] = []

        def emit(p, poly_ref):
            if math.isnan(p[0]) or math.isnan(p[1]):
                if len(poly_ref) >= 2:
                    segments.append(poly_ref[:])
                poly_ref.clear()
                return
            poly_ref.append([p[0], p[1]])

        emit(pts[0], poly)
        for i in range(n0):
            stack = [(ts[i], pts[i], ts[i + 1], pts[i + 1])]
            while stack:
                ta, pa, tb, pb = stack.pop()
                finite = not any(math.isnan(v) for v in (*pa, *pb))
                ok = False
                if finite and xiv is not None and work.exhausted is None and work.spend(2):
                    try:
                        T = iv.mpf([ta, tb])
                        X, Y = xiv(T), yiv(T)
                        box = (*(float(v) for v in _ends(X)), *(float(v) for v in _ends(Y)))
                        ok = _off_screen(view, box, pa, pb) or _box_near_segment(view, box, pa, pb)
                    except (Undefined, Unsupported):
                        ok = False
                seg_px = (
                    math.hypot(*(u - v for u, v in zip(view.px(*pa), view.px(*pb)))) if finite else math.inf
                )
                tiny = (tb - ta) <= span * 1e-7 or (finite and seg_px <= MIN_PX)
                if ok:
                    emit(pb, poly)
                    continue
                if tiny or work.exhausted is not None or work.points >= MAX_POINTS:
                    work.miss(float(ta))
                    emit(pb, poly)
                    continue
                tm = (ta + tb) / 2
                pm = (xnum(tm), ynum(tm))
                work.points += 1
                stack.append((tm, pm, tb, pb))
                stack.append((ta, pa, tm, pm))
        if len(poly) >= 2:
            segments.append(poly)
    return [_thin(p, view) for p in segments]


# ---- implicit curves and regions (quadtrees) ---------------------------------------


def _cells(view: View, base_px: float):
    nx = max(1, math.ceil(view.w / base_px))
    ny = max(1, math.ceil(view.h / base_px))
    cw = (view.xhi - view.xlo) / nx
    ch = (view.yhi - view.ylo) / ny
    return nx, ny, cw, ch


def _region(cond, vx, vy, view: View, work: Work) -> dict:
    try:
        civ = compile_cond(cond, (vx, vy))
    except Unsupported as exc:
        work.reasons.append(f"interval arithmetic is not available for {exc}")
        civ = None
    num = _numeric(cond, (vx, vy))
    nx, ny, cw, ch = _cells(view, QUAD_BASE_PX)
    leaf_w = REGION_LEAF_PX / view.sx
    leaf_h = REGION_LEAF_PX / view.sy
    rects: list[list[float]] = []

    def center_inside(x0, x1, y0, y1) -> bool:
        v = num((x0 + x1) / 2, (y0 + y1) / 2)
        return not math.isnan(v) and v != 0

    stack = [
        (view.xlo + i * cw, view.xlo + (i + 1) * cw, view.ylo + j * ch, view.ylo + (j + 1) * ch)
        for j in range(ny)
        for i in range(nx)
    ]
    while stack:
        x0, x1, y0, y1 = stack.pop()
        truth: bool | None = None
        if civ is not None and work.exhausted is None and work.spend():
            try:
                truth = civ((iv.mpf([x0, x1]), iv.mpf([y0, y1])))
            except Undefined:
                truth = None
        if truth is True:
            rects.append([x0, y0, x1, y1])
            work.points += 1
            continue
        if truth is False:
            continue
        leaf = (x1 - x0) <= leaf_w * 1.0001 and (y1 - y0) <= leaf_h * 1.0001
        if civ is not None:
            can_split = not leaf and work.exhausted is None
        else:
            can_split = (x1 - x0) * view.sx > 2 and work.points < MAX_POINTS
        if can_split:
            xm, ym = (x0 + x1) / 2, (y0 + y1) / 2
            stack.extend([(x0, xm, y0, ym), (xm, x1, y0, ym), (x0, xm, ym, y1), (xm, x1, ym, y1)])
            continue
        # A boundary cell: resolved by its centre. Larger than a leaf means the budget ran out.
        if civ is not None and not leaf:
            work.miss((x0 + x1) / 2)
        if center_inside(x0, x1, y0, y1):
            rects.append([x0, y0, x1, y1])
            work.points += 1
    if civ is None and not work.reasons:
        work.reasons.append("the region was sampled on a 2 px grid")
    return {"rects": sorted(rects)}


def _march(corners: list[float], xs: tuple[float, float], ys: tuple[float, float]) -> list[tuple]:
    """Marching squares on one cell. Corners: (x0,y0), (x1,y0), (x1,y1), (x0,y1)."""
    (x0, x1), (y0, y1) = xs, ys
    pts = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
    crossings = []
    for i in range(4):
        j = (i + 1) % 4
        vi, vj = corners[i], corners[j]
        if (vi < 0) != (vj < 0):
            # Interpolate from the lower-indexed corner by position so neighbours agree exactly.
            (pa, va), (pb, vb) = sorted([(pts[i], vi), (pts[j], vj)])
            t = va / (va - vb)
            crossings.append((pa[0] + t * (pb[0] - pa[0]), pa[1] + t * (pb[1] - pa[1])))
    if len(crossings) == 2:
        return [(crossings[0], crossings[1])]
    if len(crossings) == 4:
        return [(crossings[0], crossings[1]), (crossings[2], crossings[3])]
    return []


def _join(pieces: list[tuple]) -> list[list[list[float]]]:
    """Join two-point segments that share endpoints into polylines."""
    ends: dict[tuple, list[int]] = {}
    for i, (a, b) in enumerate(pieces):
        ends.setdefault(a, []).append(i)
        ends.setdefault(b, []).append(i)
    used = [False] * len(pieces)
    out = []
    for i in range(len(pieces)):
        if used[i]:
            continue
        used[i] = True
        line = [pieces[i][0], pieces[i][1]]
        for direction in (1, 0):
            while True:
                tip = line[-1] if direction else line[0]
                nxt = next((k for k in ends.get(tip, []) if not used[k]), None)
                if nxt is None:
                    break
                used[nxt] = True
                a, b = pieces[nxt]
                other = b if a == tip else a
                if direction:
                    line.append(other)
                else:
                    line.insert(0, other)
        out.append([[p[0], p[1]] for p in line])
    return out


def _implicit(rel, vx, vy, view: View, work: Work) -> list:
    g = rel.lhs - rel.rhs
    try:
        giv = compile_iv(g, (vx, vy))
    except Unsupported as exc:
        work.reasons.append(f"interval arithmetic is not available for {exc}")
        giv = None
    num = _numeric(g, (vx, vy))
    leaf_px = IMPLICIT_LEAF_PX if giv is not None else 2.0
    depth = max(0, math.ceil(math.log2(QUAD_BASE_PX / leaf_px)))
    nx, ny, cw, ch = _cells(view, QUAD_BASE_PX)
    scale = 2**depth
    lw, lh = cw / scale, ch / scale
    pieces: list[tuple] = []
    ambiguous = 0
    cache: dict[tuple[int, int], float] = {}

    def corner(ix: int, iy: int) -> float:
        key = (ix, iy)
        if key not in cache:
            cache[key] = num(view.xlo + ix * lw, view.ylo + iy * lh)
        return cache[key]

    stack = [(i * scale, j * scale, scale) for j in range(ny) for i in range(nx)]
    while stack:
        ix, iy, size = stack.pop()
        x0, x1 = view.xlo + ix * lw, view.xlo + (ix + size) * lw
        y0, y1 = view.ylo + iy * lh, view.ylo + (iy + size) * lh
        maybe = True
        if giv is not None and work.exhausted is None and work.spend():
            try:
                lo, hi = _ends(giv(iv.mpf([x0, x1]), iv.mpf([y0, y1])))
                maybe = lo <= 0 <= hi
            except Undefined:
                maybe = True
        if not maybe:
            continue
        if size > 1 and work.exhausted is None and len(cache) < MAX_POINTS * 4:
            half = size // 2
            stack.extend(
                [(ix, iy, half), (ix + half, iy, half), (ix, iy + half, half), (ix + half, iy + half, half)]
            )
            continue
        vals = [corner(ix, iy), corner(ix + size, iy), corner(ix + size, iy + size), corner(ix, iy + size)]
        if any(math.isnan(v) for v in vals):
            continue
        segs = _march(vals, (x0, x1), (y0, y1))
        if not segs and giv is not None:
            ambiguous += 1
        pieces.extend(segs)
    reason = f"implicit curves are traced by marching squares on a {leaf_px:g} px grid"
    if giv is not None:
        reason += "; interval arithmetic rules the curve out of every other cell"
        if ambiguous:
            reason += f"; {ambiguous} cell(s) may hold curve that does not cross their corners"
    work.reasons.append(reason)
    return [_thin(p, view) for p in _join(pieces)]


# ---- entry points ------------------------------------------------------------------


def _exact(value: float) -> sp.Rational:
    return sp.Rational(repr(float(value)))


def _spec_value(item: dict) -> sp.Expr:
    return read_math(item["text"]) if item.get("text") else _exact(item["value"])


def _num(v) -> float:
    return float(sp.N(v, 17))


def _check_viewport(viewport: Any) -> tuple[float, float, float, float]:
    if not isinstance(viewport, dict):
        raise CallError("bad_args", "viewport must be {x: [lo, hi], y: [lo, hi]}")
    out = []
    for axis in ("x", "y"):
        pair = viewport.get(axis)
        ok = (
            isinstance(pair, list)
            and len(pair) == 2
            and all(isinstance(v, int | float) and not isinstance(v, bool) and math.isfinite(v) for v in pair)
            and pair[0] < pair[1]
        )
        if not ok:
            raise CallError("bad_args", f"viewport.{axis} must be [lo, hi] with finite lo < hi")
        out.extend(float(v) for v in pair)
    return out[0], out[1], out[2], out[3]


def _substitution(engine, params: dict | None) -> dict:
    values = {name: p.value for name, p in engine.scene.params.items()}
    for name, value in (params or {}).items():
        if name not in values:
            raise CallError("bad_args", f"there is no parameter {name!r} to override")
        if not isinstance(value, int | float) or isinstance(value, bool) or not math.isfinite(value):
            raise CallError("bad_args", f"the override for {name} must be a finite number")
        values[name] = value
    return {sp.Symbol(name, real=True): _exact(v) for name, v in values.items()}


def sample(
    engine, object_id: str, viewport: Any, pixels: Any = (800, 600), params: dict | None = None
) -> dict:
    """Drawable, checked geometry for one scene object (see the module docstring)."""
    started = time.perf_counter()
    xlo, xhi, ylo, yhi = _check_viewport(viewport)
    if isinstance(pixels, dict):
        pixels = (pixels.get("w", 800), pixels.get("h", 600))
    w, h = pixels
    if not all(isinstance(v, int | float) and 16 <= v <= 16384 for v in (w, h)):
        raise CallError("bad_args", "pixels must be {w, h} between 16 and 16384")
    view = View(xlo, xhi, ylo, yhi, float(w), float(h))
    obj = engine.scene.objects.get(object_id) or engine.scene.notes.get(object_id)
    if obj is None:
        raise CallError("bad_args", f"there is no object {object_id!r}")
    subs = _substitution(engine, params)
    work = Work(deadline=started + WALL_CLOCK_S)
    try:
        out = _geometry(engine, obj, view, subs, work)
    except CallError:
        raise
    except Exception as exc:  # a gap in the sampler must be a reported failure, never a crash
        raise CallError("math", f"could not sample this object ({type(exc).__name__}: {exc})") from None
    status, reason = work.status()
    result = {"object": object_id, "kind": obj.kind, "status": status, "segments": [], "markers": []}
    result.update(out)
    if reason:
        result["reason"] = reason
    result["stats"] = {
        "evals": work.evals,
        "points": work.points,
        "ms": round((time.perf_counter() - started) * 1000, 1),
    }
    return result


def _bound(engine, obj, subs: dict, names: tuple[str, ...]) -> list:
    exprs = [obj.exprs[n].subs(subs) for n in names]
    return exprs


def _geometry(engine, obj, view: View, subs: dict, work: Work) -> dict:
    kind = obj.kind
    if kind == "explicit":
        f, x = obj.exprs["f"].subs(subs), obj.exprs["var"]
        _require_only(f, {x})
        lo, hi = (_spec_value(v) for v in obj.spec["domain"])
        return _explicit_in_view(f, x, lo, hi, view, work)
    if kind == "parametric":
        fx, fy, t = obj.exprs["x"].subs(subs), obj.exprs["y"].subs(subs), obj.exprs["var"]
        _require_only(fx, {t})
        _require_only(fy, {t})
        t0, t1 = (_spec_value(v) for v in obj.spec["range"])
        return {"segments": _parametric(fx, fy, t, t0, t1, view, work)}
    if kind == "polar":
        r, th = obj.exprs["r"].subs(subs), obj.exprs["var"]
        _require_only(r, {th})
        t0, t1 = (_spec_value(v) for v in obj.spec["range"])
        return {"segments": _parametric(r * sp.cos(th), r * sp.sin(th), th, t0, t1, view, work)}
    if kind == "implicit":
        rel, vx, vy = obj.exprs["relation"].subs(subs), obj.exprs["x"], obj.exprs["y"]
        _require_only(rel, {vx, vy})
        return {"segments": _implicit(rel, vx, vy, view, work)}
    if kind == "region":
        cond, vx, vy = obj.exprs["condition"].subs(subs), obj.exprs["x"], obj.exprs["y"]
        _require_only(cond, {vx, vy})
        return {"fill": _region(cond, vx, vy, view, work)}
    if kind == "point":
        x, y = (_num(e) for e in _bound(engine, obj, subs, ("x", "y")))
        marker = {"kind": "point", "x": x, "y": y, "closed": True}
        if obj.spec.get("label"):
            marker["label"] = obj.spec["label"]
        return {"markers": [marker]}
    if kind == "vector":
        ox, oy, vx, vy = (_num(e) for e in _bound(engine, obj, subs, ("ox", "oy", "vx", "vy")))
        return {
            "segments": [[[ox, oy], [ox + vx, oy + vy]]],
            "arrow": {"from": [ox, oy], "to": [ox + vx, oy + vy]},
        }
    if kind == "area":
        f, x = obj.exprs["f"].subs(subs), obj.exprs["var"]
        _require_only(f, {x})
        lo, hi = (_spec_value(v) for v in obj.spec["over"])
        curve = _explicit_in_view(f, x, lo, hi, view, work)
        polygons = [[*p, [p[-1][0], 0.0], [p[0][0], 0.0]] for p in curve["segments"]]
        return {"segments": curve["segments"], "markers": curve["markers"], "fill": {"polygons": polygons}}
    if kind == "riemann":
        return _riemann(obj, subs, work)
    if kind == "data":
        xs, ys = obj.spec["x"], obj.spec["y"]
        points = [[float(a), float(b)] for a, b in zip(xs, ys)]
        if obj.spec.get("mode") == "line":
            return {"segments": [points] if len(points) >= 2 else [], "points": points}
        return {"points": points}
    if kind == "highlight":
        target = engine.scene.objects.get(obj.spec["target"]) or engine.scene.notes.get(obj.spec["target"])
        if target is None:
            raise CallError("bad_args", f"the highlighted object {obj.spec['target']!r} no longer exists")
        if "over" in obj.spec and target.kind == "explicit":
            f, x = target.exprs["f"].subs(subs), target.exprs["var"]
            lo, hi = (_spec_value(v) for v in obj.spec["over"])
            return _explicit_in_view(f, x, lo, hi, view, work)
        return _geometry(engine, target, view, subs, work)
    if kind in ("label", "latex"):
        at = obj.spec["at"]
        if "object" in at:
            return {"anchor": {"object": at["object"]}}
        x, y = (_num(_spec_value(v)) for v in at["point"])
        return {"markers": [{"kind": "point", "x": x, "y": y}]}
    if kind == "arrow":
        a = [_num(_spec_value(v)) for v in obj.spec["start"]]
        b = [_num(_spec_value(v)) for v in obj.spec["end"]]
        return {"segments": [[a, b]], "arrow": {"from": a, "to": b}}
    raise CallError("bad_args", f"objects of kind {kind!r} cannot be sampled yet")


def _require_only(expr, allowed: set) -> None:
    extra = sorted(str(s) for s in expr.free_symbols - allowed)
    if extra:
        raise CallError(
            "bad_args", f"cannot sample: {', '.join(extra)} has no value", hint="declare it as a parameter"
        )


def _explicit_in_view(f, x, lo, hi, view: View, work: Work) -> dict:
    lo_v = sp.Max(lo, _exact(view.xlo))
    hi_v = sp.Min(hi, _exact(view.xhi))
    if not bool(lo_v < hi_v):
        return {"segments": [], "markers": []}
    segments, markers = _explicit(f, x, lo_v, hi_v, view, work)
    return {"segments": segments, "markers": markers}


def _riemann(obj, subs: dict, work: Work) -> dict:
    f, x = obj.exprs["f"].subs(subs), obj.exprs["var"]
    _require_only(f, {x})
    lo, hi = (_spec_value(v) for v in obj.spec["over"])
    n, rule = int(obj.spec["n"]), obj.spec.get("rule", "mid")
    width = (hi - lo) / n
    polygons = []
    total = sp.Integer(0)
    for i in range(n):
        a, b = lo + i * width, lo + (i + 1) * width
        if rule == "trapezoid":
            ya, yb = _real_value(f.subs(x, a)), _real_value(f.subs(x, b))
            if ya is None or yb is None:
                work.reasons.append(f"the function is undefined at a sample point near x = {float(a):g}")
                continue
            polygons.append([[_num(a), 0.0], [_num(a), _num(ya)], [_num(b), _num(yb)], [_num(b), 0.0]])
            total += (ya + yb) / 2 * width
            continue
        c = {"left": a, "right": b, "mid": (a + b) / 2}[rule]
        yc = _real_value(f.subs(x, c))
        if yc is None:
            work.reasons.append(f"the function is undefined at the sample point x = {float(c):g}")
            continue
        y = _num(yc)
        polygons.append([[_num(a), 0.0], [_num(a), y], [_num(b), y], [_num(b), 0.0]])
        total += yc * width
    return {"fill": {"polygons": polygons}, "total": _num(total)}


def register(methods) -> None:
    def handler(params: dict) -> dict:
        try:
            return sample(
                methods.engine,
                params.get("object"),
                params.get("viewport"),
                params.get("pixels") or (800, 600),
                params.get("params"),
            )
        except CallError as exc:
            return {"status": "error", "error": exc.to_json()}

    methods.register("sample", handler)
