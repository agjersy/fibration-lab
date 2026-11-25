"""Objects on the canvas. Each function returns the object's id (a ``str``)."""

from __future__ import annotations

from typing import Any

from ._objects import Handle, _scalar, call, interval


class Plotted(str):
    """An object id, with any extra results of the call as attributes (``line``, ``value``)."""

    def __new__(cls, out: dict[str, Any]):
        self = super().__new__(cls, out["id"])
        for key, value in out.items():
            if key != "id":
                setattr(self, key, value)
        return self


def _one_var(kw: dict[str, Any], default: tuple[Any, Any] | None, what: str):
    """``x=(-10, 10)`` style keyword: (variable name, interval)."""
    if len(kw) != 1:
        raise TypeError(f"{what}: give the variable and its range as one keyword, e.g. x=(-10, 10)")
    (name, rng), = kw.items()
    return name, interval(rng) if rng is not None else default


def explicit(f, var: Any = None, domain=None, *, id: str | None = None, style: dict | None = None, **var_range):
    """``plot.explicit(f, x=(-10, 10))`` or ``plot.explicit(f, var=x, domain=(-10, 10))``."""
    if var is None:
        var, domain = _one_var(var_range, None, "plot.explicit")
    elif var_range:
        raise TypeError("give the variable either as var= or as a keyword, not both")
    return Plotted(call("plot.explicit", expr=_scalar(f), var=var, domain=interval(domain), id=id, style=style))


def parametric(x, y, var: Any = None, range=None, *, id=None, style=None, **var_range):
    """``plot.parametric(cos(t), sin(t), t=(0, "2*pi"))``."""
    if var is None:
        var, range = _one_var(var_range, None, "plot.parametric")
    return Plotted(call("plot.parametric", x=_scalar(x), y=_scalar(y), var=var, range=interval(range), id=id, style=style))


def polar(r, var: Any = None, range=None, *, id=None, style=None, **var_range):
    """``plot.polar(r, theta=(0, "2*pi"))``."""
    if var is None:
        var, range = _one_var(var_range, None, "plot.polar")
    return Plotted(call("plot.polar", r=_scalar(r), var=var, range=interval(range), id=id, style=style))


def implicit(equation, x="x", y="y", *, id=None, style=None):
    """``plot.implicit("x**2 + y**2 = 1")``; an expression means expr = 0."""
    return Plotted(call("plot.implicit", equation=_scalar(equation), x=x, y=y, id=id, style=style))


def region(condition, x="x", y="y", *, id=None, style=None):
    """``plot.region("y < sin(x) and y > 0")``."""
    cond = condition if isinstance(condition, Handle) else _scalar(condition)
    return Plotted(call("plot.region", condition=cond, x=x, y=y, id=id, style=style))


def point(x, y, label: str | None = None, *, id=None, style=None):
    return Plotted(call("plot.point", at=[_scalar(x), _scalar(y)], label=label, id=id, style=style))


def vector(origin, vector, *, id=None, style=None):
    return Plotted(call("plot.vector", origin=interval(origin), vector=interval(vector), id=id, style=style))


def tangent(of: str, at, *, id=None, style=None):
    """Tangent to curve ``of`` at x = at; ``.line`` is its expression."""
    return Plotted(call("plot.tangent", of=of, at=_scalar(at), id=id, style=style))


def area(of: str, lo, hi, *, id=None, style=None):
    """Signed area under curve ``of``; ``.value`` is the exact integral, or None."""
    return Plotted(call("plot.area", of=of, over=interval((lo, hi)), id=id, style=style))


def riemann(of: str, lo, hi, n: int, rule: str = "mid", *, id=None, style=None):
    return Plotted(call("plot.riemann", of=of, over=interval((lo, hi)), n=n, rule=rule, id=id, style=style))


def data(x, y, mode: str = "points", *, id=None, style=None):
    """A series from numbers (any iterables of reals, including NumPy arrays)."""
    return Plotted(call("plot.data", x=[float(v) for v in x], y=[float(v) for v in y], mode=mode, id=id, style=style))
