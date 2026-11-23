"""Proxy objects over engine handles, and the call helper that wraps results.

Nothing symbolic is computed here. An ``Expr`` is a handle reference plus cached display
text; every operation on it, operators included, is one engine call that returns a new
handle. Equality (``==``) keeps Python's identity meaning so proxies stay hashable; use
``a.eq(b)`` for an equation and ``a.equals(b)`` for the engine's truth.
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import Any

from . import _client
from .errors import MathError, NoLimit, Undecided


def _wire(value: Any) -> Any:
    """SDK values to wire arguments."""
    if isinstance(value, Handle):
        return value.ref
    if isinstance(value, bool):
        return value
    if isinstance(value, int | float | str):
        return value
    if isinstance(value, list | tuple):
        return [_wire(v) for v in value]
    if isinstance(value, dict):
        return {str(k): _wire(v) for k, v in value.items() if v is not None}
    if hasattr(value, "__index__"):
        return int(value)
    if hasattr(value, "__float__"):
        return float(value)
    raise TypeError(f"cannot send a {type(value).__name__} to the engine")


def _wrap(value: Any) -> Any:
    """Wire results to SDK values."""
    if isinstance(value, dict):
        if "$h" in value and set(value) <= {"$h", "kind"}:
            cls = {"set": Set, "relation": Relation, "boolean": Relation}.get(value.get("kind"), Expr)
            return cls(value)
        if "truth" in value and set(value) <= {"truth", "method", "reason"}:
            return Truth(value)
        return {k: _wrap(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_wrap(v) for v in value]
    return value


def call(fn: str, **args: Any) -> Any:
    """One engine call; raises MathError on failure and returns the wrapped value."""
    wire = {k: _wire(v) for k, v in args.items() if v is not None}
    result = _client.client().call(fn, wire)
    if result.get("status") != "ok":
        err = result.get("error") or {}
        kind = err.get("kind", "math")
        if kind == "undecided":
            raise Undecided(err.get("detail", ""), err.get("hint"), fn)
        raise MathError(kind, err.get("detail", "the call failed"), err.get("hint"), fn)
    return _wrap(result.get("value"))


def _scalar(value: Any) -> Any:
    if isinstance(value, Handle | int | float | str):
        return value
    raise TypeError(f"expected a number, math text, or an expression, not {type(value).__name__}")


def interval(pair: Iterable[Any] | None) -> list[Any] | None:
    if pair is None:
        return None
    items = list(pair)
    if len(items) != 2:
        raise ValueError("an interval is a pair (lo, hi)")
    return [_scalar(v) for v in items]


class Truth:
    """A three-valued answer. ``bool()`` raises Undecided when the engine could not decide."""

    def __init__(self, raw: dict[str, Any]):
        self.raw = raw
        self.value: bool | None = {"true": True, "false": False}.get(raw.get("truth"))
        self.method: str = raw.get("method", "none")
        self.reason: str | None = raw.get("reason")

    @property
    def decided(self) -> bool:
        return self.value is not None

    @property
    def proven(self) -> bool:
        """True when decided symbolically (can support a proven claim)."""
        return self.value is not None and self.method == "symbolic"

    def __bool__(self) -> bool:
        if self.value is None:
            raise Undecided(self.reason or "the engine could not decide this", "check t.decided before branching")
        return self.value

    def __repr__(self) -> str:
        word = {True: "true", False: "false", None: "unknown"}[self.value]
        return f"Truth({word}, {self.method})" + (f" — {self.reason}" if self.reason else "")


class Handle:
    """Base for every proxy over an engine object."""

    __slots__ = ("_info", "name", "ref")

    def __init__(self, ref: dict[str, Any], name: str | None = None):
        self.ref = {"$h": ref["$h"], "kind": ref.get("kind", "expr")}
        self.name = name
        self._info: dict[str, Any] | None = None

    def info(self) -> dict[str, Any]:
        if self._info is None:
            self._info = call("info", of=self)
        return self._info

    @property
    def latex(self) -> str:
        return self.info()["latex"]

    @property
    def text(self) -> str:
        return self.info()["text"]

    @property
    def head(self) -> str:
        return self.info()["head"]

    @property
    def free_symbols(self) -> list[str]:
        return self.info()["free_symbols"]

    def srepr(self) -> str:
        return call("to_srepr", of=self)

    def to_sympy(self):
        """A SymPy object for your own code (needs SymPy in this environment)."""
        import sympy

        return sympy.parse_expr(self.srepr(), {k: getattr(sympy, k) for k in dir(sympy) if not k.startswith("_")})

    def __repr__(self) -> str:
        label = f"{self.name} = " if self.name else ""
        try:
            return f"<{type(self).__name__} {label}{self.text}>"
        except Exception:
            return f"<{type(self).__name__} #{self.ref['$h']}>"

    def _repr_latex_(self) -> str:
        return f"${self.latex}$"


def _arith(op: str, a: Any, b: Any = None) -> Expr:
    return call("expr.arith", op=op, a=_scalar(a), b=None if b is None else _scalar(b))


def _rel(lhs: Any, rel: str, rhs: Any) -> Relation:
    return call("expr.relation", lhs=_scalar(lhs), rel=rel, rhs=_scalar(rhs))


class Expr(Handle):
    """An expression held by the engine."""

    __slots__ = ()

    # ---- operators -----------------------------------------------------------
    def __add__(self, other):
        return _arith("add", self, other)

    def __radd__(self, other):
        return _arith("add", other, self)

    def __sub__(self, other):
        return _arith("sub", self, other)

    def __rsub__(self, other):
        return _arith("sub", other, self)

    def __mul__(self, other):
        return _arith("mul", self, other)

    def __rmul__(self, other):
        return _arith("mul", other, self)

    def __truediv__(self, other):
        return _arith("div", self, other)

    def __rtruediv__(self, other):
        return _arith("div", other, self)

    def __pow__(self, other):
        return _arith("pow", self, other)

    def __rpow__(self, other):
        return _arith("pow", other, self)

    def __neg__(self):
        return _arith("neg", self)

    def __pos__(self):
        return self

    def __lt__(self, other):
        return _rel(self, "<", other)

    def __le__(self, other):
        return _rel(self, "<=", other)

    def __gt__(self, other):
        return _rel(self, ">", other)

    def __ge__(self, other):
        return _rel(self, ">=", other)

    __hash__ = Handle.__hash__

    def eq(self, other) -> Relation:
        """The equation self = other."""
        return _rel(self, "=", other)

    def __call__(self, **values) -> Expr:
        """Exact value at symbol values by name: f(x=2)."""
        return call("evaluate", expr=self, at=values)

    def __float__(self) -> float:
        return self.numeric()

    # ---- derived ---------------------------------------------------------------
    def diff(self, var, n: int = 1) -> Expr:
        return call("diff", expr=self, var=var, n=n)

    def integrate(self, var, lo=None, hi=None) -> Expr:
        bounds = None if lo is None and hi is None else interval((lo, hi))
        out = call("integrate", expr=self, var=var, bounds=bounds)
        return out["value"]

    def limit(self, var, at, dir: str = "+-") -> Expr:
        out = call("limit", expr=self, var=var, at=_scalar(at), dir=dir)
        exists: Truth = out["exists"]
        if exists.value is None:
            raise Undecided(exists.reason or "the limit could not be evaluated", fn="limit")
        if not exists.value:
            raise NoLimit(exists.reason or "the limit does not exist")
        return out["value"]  # may be oo or -oo: the limit exists in the extended reals

    def series(self, var, at=0, n: int = 6) -> Expr:
        return call("series", expr=self, var=var, at=_scalar(at), n=n)

    def simplify(self) -> Expr:
        return call("simplify", expr=self)

    def expand(self) -> Expr:
        return call("expand", expr=self)

    def factor(self) -> Expr:
        return call("factor", expr=self)

    def cancel(self) -> Expr:
        return call("cancel", expr=self)

    def trigsimp(self) -> Expr:
        return call("trigsimp", expr=self)

    def subs(self, mapping: dict[str, Any] | None = None, **values) -> Expr:
        merged = {**(mapping or {}), **values}
        merged = {(k.text if isinstance(k, Handle) else k): v for k, v in merged.items()}
        return call("subs", expr=self, values=merged)

    def solve(self, var, within=None) -> list[Expr]:
        """Real solutions of self = 0. Raises Undecided if the engine cannot list them all."""
        out = call("solve", equation=self, var=var, within=interval(within))
        if out["complete"].value is not True:
            raise Undecided(out["complete"].reason or "the solutions are not a finite list", fn="solve")
        return out["solutions"]

    def roots(self, var, within=None) -> Roots:
        out = call("roots_in", expr=self, var=var, within=interval(within))
        return Roots([r["x"] for r in out["roots"]], out["complete"], out["method"])

    def extrema(self, var, lo, hi) -> dict[str, Any]:
        return call("extrema", expr=self, var=var, over=interval((lo, hi)))

    def sign_on(self, var, lo, hi) -> str:
        return call("sign_on", expr=self, var=var, over=interval((lo, hi)))["sign"]

    # ---- introspection -----------------------------------------------------------
    def singularities(self, var, within=None) -> Set:
        out = call("singularities", expr=self, var=var, within=interval(within))
        s: Set = out["set"]
        s.points = out["points"]
        return s

    def continuous_domain(self, var, within=None) -> Set:
        return call("continuous_domain", expr=self, var=var, within=interval(within))

    def is_continuous_on(self, var, lo, hi) -> Truth:
        return call("is_continuous_on", expr=self, var=var, over=interval((lo, hi)))

    def has_finite_limit(self, var, at, dir: str = "+-") -> Truth:
        return call("is_finite_limit", expr=self, var=var, at=_scalar(at), dir=dir)

    def equals(self, other) -> Truth:
        return call("equals", a=self, b=_scalar(other))

    def degree(self, var) -> int | None:
        return call("degree", expr=self, var=var)

    def numeric(self, digits: int = 15) -> float:
        out = call("numeric", expr=self, digits=digits)
        if out.get("value") is None:
            raise MathError("math", f"{out.get('text')} is not a finite real number", fn="numeric")
        return out["value"]


class Relation(Handle):
    """An equation, inequality, or logical combination of them."""

    __slots__ = ()

    def parts(self) -> tuple[Expr, Expr]:
        out = call("expr.parts", relation=self)
        return out["lhs"], out["rhs"]

    def subs(self, mapping: dict[str, Any] | None = None, **values) -> Relation:
        merged = {**(mapping or {}), **values}
        return call("subs", expr=self, values=merged)

    def solve(self, var, within=None) -> list[Expr]:
        out = call("solve", equation=self, var=var, within=interval(within))
        if out["complete"].value is not True:
            raise Undecided(out["complete"].reason or "the solutions are not a finite list", fn="solve")
        return out["solutions"]


class Set(Handle):
    """A set of real numbers."""

    __slots__ = ("points",)

    def __init__(self, ref: dict[str, Any], name: str | None = None):
        super().__init__(ref, name)
        self.points: list[Expr] | None = None

    def is_empty(self) -> Truth:
        return call("set.is_empty", set=self)

    def contains(self, value) -> Truth:
        return call("set.contains", set=self, value=_scalar(value))

    def intervals(self) -> dict[str, Any]:
        return call("set.as_intervals", set=self)


class Roots(list):
    """Roots with how completely they are known."""

    def __init__(self, items: list[Expr], complete: Truth, method: str):
        super().__init__(items)
        self.complete = complete
        self.method = method
