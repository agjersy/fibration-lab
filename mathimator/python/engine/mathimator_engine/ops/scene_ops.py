"""Scene operations: parameters, objects, notes, and view hints (ARCHITECTURE 5.3, 5.4).

These are the only operations that change scene state. Each plotted object records its
expressions by content and checks that every free symbol is either a plot variable or a
declared parameter, so nothing on the canvas depends on an unbound name.
"""

from __future__ import annotations

from typing import Any

import sympy as sp

from ..engine import require_defined
from ..errors import CallError
from ..memo import Out
from ..registry import HEAVY_BUDGET_S, op
from ..scene import Param, SceneObject
from ..schema import OBJECT_ID, STYLE, handle, integer, interval, number, scalar, string
from .common import HAS_EXPLICIT, HAS_OBJECTS, HAS_PARAMS, check_bound, exact_text

POINT = {"type": "array", "items": scalar(), "minItems": 2, "maxItems": 2}
MAX_DATA_POINTS = 100_000


def _place(engine, prefix: str, kind: str, obj_id: str | None, exprs, spec, style, notes: bool = False) -> str:
    table = engine.scene.notes if notes else engine.scene.objects
    if obj_id is None:
        obj_id = engine.scene.next_id(prefix)
        while obj_id in table:
            obj_id = engine.scene.next_id(prefix)
    elif obj_id in table and table[obj_id].kind != kind:
        raise CallError("bad_args", f"{obj_id!r} is already a {table[obj_id].kind}; choose another id")
    elif obj_id in (engine.scene.notes if not notes else engine.scene.objects):
        raise CallError("bad_args", f"the id {obj_id!r} is already used")
    for name, expr in exprs.items():
        require_defined(expr, f"the {kind}'s {name}")
    table[obj_id] = SceneObject(obj_id, kind, exprs, spec, dict(style or {}), engine.origin)
    return obj_id


CURVE_KINDS = ("explicit", "parametric", "polar", "implicit", "region", "area", "riemann")
VAR_KEYS = ("var", "x", "y")


def _bounds(engine, pair, what: str, finite: bool = False):
    """An interval of a scene object: two real numbers (no symbols), finite when asked."""
    lo, hi = engine.interval(pair, what)
    for end in (lo, hi):
        if end.free_symbols:
            raise CallError(
                "bad_args",
                f"the {what} end {end} contains a symbol; interval ends must be numbers",
                hint="use numbers or constants such as pi",
            )
        if end.is_extended_real is not True or (finite and end.is_finite is not True):
            raise CallError("bad_args", f"the {what} end {end} is not a {'finite ' if finite else ''}real number")
    return lo, hi


def _range(engine, pair, what: str, finite: bool = False) -> tuple[dict[str, Any], dict[str, Any]]:
    lo, hi = _bounds(engine, pair, what, finite)
    return exact_text(lo), exact_text(hi)


def _var(engine, arg, what: str = "variable") -> sp.Symbol:
    """A plot variable: a symbol that is not a declared parameter."""
    var = engine.symbol(arg, what)
    if str(var) in engine.scene.params:
        raise CallError(
            "bad_args",
            f"{var} is a declared parameter, so it cannot also be the {what}",
            hint="use another variable name, or another parameter name",
        )
    return var


def _coordinate(engine, arg, what: str = "coordinate") -> sp.Expr:
    """A point coordinate: real and finite, possibly depending on parameters."""
    value = engine.expr(arg, what)
    check_bound(engine, value, set(), what)
    if value.is_extended_real is False or value.is_finite is False:
        raise CallError("bad_args", f"the {what} {value} is not a finite real number")
    return value


# ---- parameters ------------------------------------------------------------------


@op(
    "param.declare",
    "parameter",
    "Declare a parameter with a value and slider range; returns its symbol",
    args={
        "name": string("A symbol name such as a", max_length=32),
        "value": number(),
        "min": number(),
        "max": number(),
        "step": number(minimum=0),
    },
    required=["name", "value", "min", "max"],
    result="handle:expr",
)
def declare(engine, name, value, min, max, step=None):
    from ..mathparse import symbol

    sym = symbol(name)
    if not min < max:
        raise CallError("bad_args", f"the range [{min}, {max}] is empty")
    if not min <= value <= max:
        raise CallError("bad_args", f"the value {value} is outside the range [{min}, {max}]")
    for obj in engine.scene.objects.values():
        if obj.kind in CURVE_KINDS and any(obj.exprs.get(k) == sym for k in VAR_KEYS):
            raise CallError(
                "bad_args",
                f"{name} is the plot variable of {obj.id!r}, so it cannot also be a parameter",
                hint="choose another parameter name",
            )
    engine.scene.params[name] = Param(name, float(value), float(min), float(max), None if step is None else float(step))
    return Out(sym)


@op(
    "param.set",
    "parameter",
    "Set parameter values by name: {'a': 2.4}",
    args={"values": {"type": "object", "properties": {}, "additionalProperties": number()}},
    required=["values"],
    result="none",
    mask=HAS_PARAMS,
)
def set_params(engine, values):
    for name, value in values.items():
        p = engine.scene.params.get(name)
        if p is None:
            known = ", ".join(sorted(engine.scene.params)) or "none"
            raise CallError("bad_args", f"there is no parameter {name!r} (declared: {known})")
        if not p.min <= value <= p.max:
            raise CallError(
                "bad_args",
                f"{name} = {value} is outside its range [{p.min}, {p.max}]",
                hint="widen the range with param.declare",
            )
        p.value = float(value)
    return None


# ---- plotted objects ---------------------------------------------------------------

COMMON = {"id": OBJECT_ID, "style": STYLE}


@op(
    "plot.explicit",
    "object",
    "Plot y = f(x) over a domain (default [-10, 10])",
    args={"expr": scalar("f(x)"), "var": scalar("x"), "domain": interval(), **COMMON},
    required=["expr", "var"],
    result="{id}",
)
def explicit(engine, expr, var, domain=None, id=None, style=None):
    f, x = engine.value(expr), _var(engine, var)
    if not isinstance(f, sp.Expr):
        raise CallError("bad_args", "plot.explicit needs an expression, not an equation", hint="use plot.implicit")
    check_bound(engine, f, {x}, "curve")
    lo, hi = _range(engine, domain or [-10, 10], "domain")
    return {"id": _place(engine, "curve", "explicit", id, {"f": f, "var": x}, {"domain": [lo, hi]}, style)}


@op(
    "plot.parametric",
    "object",
    "Plot (x(t), y(t)) for t in a range",
    args={"x": scalar("x(t)"), "y": scalar("y(t)"), "var": scalar("t"), "range": interval(), **COMMON},
    required=["x", "y", "var", "range"],
    result="{id}",
)
def parametric(engine, x, y, var, range, id=None, style=None):
    fx, fy, t = engine.expr(x, "x(t)"), engine.expr(y, "y(t)"), _var(engine, var)
    check_bound(engine, fx, {t}, "x(t)")
    check_bound(engine, fy, {t}, "y(t)")
    lo, hi = _range(engine, range, "parameter range", finite=True)
    return {
        "id": _place(engine, "curve", "parametric", id, {"x": fx, "y": fy, "var": t}, {"range": [lo, hi]}, style)
    }


@op(
    "plot.polar",
    "object",
    "Plot r = f(theta) for theta in a range (default [0, 2*pi])",
    args={"r": scalar("r(theta)"), "var": scalar("theta"), "range": interval(), **COMMON},
    required=["r", "var"],
    result="{id}",
)
def polar(engine, r, var, range=None, id=None, style=None):
    fr, th = engine.expr(r, "r(theta)"), _var(engine, var)
    check_bound(engine, fr, {th}, "r(theta)")
    lo, hi = _range(engine, range or [0, "2*pi"], "angle range", finite=True)
    return {"id": _place(engine, "curve", "polar", id, {"r": fr, "var": th}, {"range": [lo, hi]}, style)}


@op(
    "plot.implicit",
    "object",
    "Plot the curve where an equation holds (an expression means expr = 0)",
    args={"equation": scalar("e.g. 'x**2 + y**2 = 1'"), "x": scalar(), "y": scalar(), **COMMON},
    required=["equation", "x", "y"],
    result="{id}",
)
def implicit(engine, equation, x, y, id=None, style=None):
    rel = engine.value(equation)
    vx, vy = _var(engine, x, "x variable"), _var(engine, y, "y variable")
    if vx == vy:
        raise CallError("bad_args", "the x and y variables must be different symbols")
    if isinstance(rel, sp.Expr):
        rel = sp.Eq(rel, 0, evaluate=False)
    if not isinstance(rel, sp.Eq):
        raise CallError("bad_args", "plot.implicit needs an equation", hint="for an inequality use plot.region")
    check_bound(engine, rel, {vx, vy}, "equation")
    return {"id": _place(engine, "curve", "implicit", id, {"relation": rel, "x": vx, "y": vy}, {}, style)}


@op(
    "plot.region",
    "object",
    "Shade the region where a condition holds, e.g. 'y < sin(x) and y > 0'",
    args={"condition": scalar(), "x": scalar(), "y": scalar(), **COMMON},
    required=["condition", "x", "y"],
    result="{id}",
)
def region(engine, condition, x, y, id=None, style=None):
    cond = engine.value(condition)
    vx, vy = _var(engine, x, "x variable"), _var(engine, y, "y variable")
    if vx == vy:
        raise CallError("bad_args", "the x and y variables must be different symbols")
    if not isinstance(cond, sp.logic.boolalg.Boolean) or isinstance(cond, sp.Eq):
        raise CallError("bad_args", "plot.region needs an inequality or a combination of them")
    check_bound(engine, cond, {vx, vy}, "condition")
    return {"id": _place(engine, "region", "region", id, {"condition": cond, "x": vx, "y": vy}, {}, style)}


@op(
    "plot.point",
    "object",
    "A point at (x, y), optionally labelled",
    args={"at": POINT, "label": string(max_length=200), **COMMON},
    required=["at"],
    result="{id}",
)
def point(engine, at, label=None, id=None, style=None):
    px, py = (_coordinate(engine, v) for v in at)
    spec = {"label": label} if label else {}
    return {"id": _place(engine, "point", "point", id, {"x": px, "y": py}, spec, style)}


@op(
    "plot.vector",
    "object",
    "An arrow from a point along a vector",
    args={"origin": POINT, "vector": POINT, **COMMON},
    required=["origin", "vector"],
    result="{id}",
)
def vector(engine, origin, vector, id=None, style=None):
    ox, oy = (_coordinate(engine, v, "origin") for v in origin)
    vx, vy = (_coordinate(engine, v, "vector component") for v in vector)
    return {"id": _place(engine, "vector", "vector", id, {"ox": ox, "oy": oy, "vx": vx, "vy": vy}, {}, style)}


def _curve(engine, of: str) -> SceneObject:
    obj = engine.scene.objects.get(of)
    if obj is None:
        raise CallError("bad_args", f"there is no object {of!r}")
    if obj.kind != "explicit":
        raise CallError("bad_args", f"{of!r} is a {obj.kind}; this needs a curve y = f(x)")
    return obj


@op(
    "plot.tangent",
    "object",
    "The tangent line to a curve y = f(x) at x = a; returns the line's expression",
    args={"of": OBJECT_ID, "at": scalar(), **COMMON},
    required=["of", "at"],
    result="{id, line: handle}",
    mask=HAS_EXPLICIT,
)
def tangent(engine, of, at, id=None, style=None):
    curve = _curve(engine, of)
    f, x = curve.exprs["f"], curve.exprs["var"]
    a = engine.expr(at, "tangent point")
    check_bound(engine, a, set(), "tangent point")
    slope = sp.diff(f, x).subs(x, a)
    line = f.subs(x, a) + slope * (x - a)
    obj_id = _place(
        engine, "curve", "explicit", id, {"f": line, "var": x}, {**curve.spec, "tangent_of": of, "at": exact_text(a)}, style
    )
    return {"id": obj_id, "line": Out(line)}


@op(
    "plot.area",
    "object",
    "Shade the signed area under a curve over an interval; returns the exact integral when SymPy finds one",
    args={"of": OBJECT_ID, "over": interval(), **COMMON},
    required=["of", "over"],
    result="{id, value: handle | null}",
    mask=HAS_EXPLICIT,
    budget_s=HEAVY_BUDGET_S,
)
def area(engine, of, over, id=None, style=None):
    curve = _curve(engine, of)
    f, x = curve.exprs["f"], curve.exprs["var"]
    lo, hi = _bounds(engine, over, "area interval", finite=True)
    value = sp.integrate(f, (x, lo, hi))
    exact = not value.has(sp.Integral)
    spec = {"of": of, "over": [exact_text(lo), exact_text(hi)]}
    obj_id = _place(engine, "area", "area", id, {"f": f, "var": x}, spec, style)
    return {"id": obj_id, "value": Out(value) if exact else None}


@op(
    "plot.riemann",
    "object",
    "Riemann rectangles (or trapezoids) for a curve over an interval",
    args={
        "of": OBJECT_ID,
        "over": interval(),
        "n": integer(minimum=1, maximum=500),
        "rule": string(enum=["left", "right", "mid", "trapezoid"]),
        **COMMON,
    },
    required=["of", "over", "n"],
    result="{id}",
    mask=HAS_EXPLICIT,
)
def riemann(engine, of, over, n, rule="mid", id=None, style=None):
    curve = _curve(engine, of)
    lo, hi = _bounds(engine, over, "interval", finite=True)
    spec = {"of": of, "over": [exact_text(lo), exact_text(hi)], "n": int(n), "rule": rule}
    return {"id": _place(engine, "riemann", "riemann", id, dict(curve.exprs), spec, style)}


@op(
    "plot.data",
    "object",
    "A data series from inline numbers (run artifacts come with the Analyst pairing)",
    args={
        "x": {"type": "array", "items": number(), "maxItems": MAX_DATA_POINTS},
        "y": {"type": "array", "items": number(), "maxItems": MAX_DATA_POINTS},
        "mode": string(enum=["points", "line"]),
        **COMMON,
    },
    required=["x", "y"],
    result="{id}",
)
def data(engine, x, y, mode="points", id=None, style=None):
    if len(x) != len(y):
        raise CallError("bad_args", f"x has {len(x)} values and y has {len(y)}")
    spec = {"x": [float(v) for v in x], "y": [float(v) for v in y], "mode": mode, "source": {"kind": "inline"}}
    return {"id": _place(engine, "data", "data", id, {}, spec, style)}


# ---- notes ------------------------------------------------------------------------

AT = {"anyOf": [OBJECT_ID, POINT], "description": "An object id, or a point [x, y]"}


def _at(engine, at) -> dict[str, Any]:
    if isinstance(at, str):
        if at not in engine.scene.objects:
            raise CallError("bad_args", f"there is no object {at!r} to attach the note to")
        return {"object": at}
    px, py = (_coordinate(engine, v) for v in at)
    return {"point": [exact_text(px), exact_text(py)]}


@op(
    "note.label",
    "annotate",
    "A text label at a point or on an object",
    args={"text": string(max_length=500), "at": AT, "id": OBJECT_ID, "style": STYLE},
    required=["text", "at"],
    result="{id}",
)
def label(engine, text, at, id=None, style=None):
    return {"id": _place(engine, "note", "label", id, {}, {"text": text, "at": _at(engine, at)}, style, notes=True)}


@op(
    "note.latex",
    "annotate",
    "A rendered formula (from an object) at a point or on an object",
    args={"expr": handle("expr", "relation", "boolean", "set"), "at": AT, "id": OBJECT_ID, "style": STYLE},
    required=["expr", "at"],
    result="{id}",
)
def latex(engine, expr, at, id=None, style=None):
    obj = engine.value(expr)
    spec = {"latex": sp.latex(obj), "at": _at(engine, at)}
    return {"id": _place(engine, "note", "latex", id, {"expr": obj}, spec, style, notes=True)}


@op(
    "note.arrow",
    "annotate",
    "An annotation arrow between two points",
    args={"start": POINT, "end": POINT, "id": OBJECT_ID, "style": STYLE},
    required=["start", "end"],
    result="{id}",
)
def arrow(engine, start, end, id=None, style=None):
    a = [exact_text(_coordinate(engine, v)) for v in start]
    b = [exact_text(_coordinate(engine, v)) for v in end]
    return {"id": _place(engine, "note", "arrow", id, {}, {"start": a, "end": b}, style, notes=True)}


# ---- scene edits ---------------------------------------------------------------------


def _target(engine, target: str) -> SceneObject:
    obj = engine.scene.objects.get(target) or engine.scene.notes.get(target)
    if obj is None:
        raise CallError("bad_args", f"there is no object {target!r}")
    return obj


@op(
    "scene.highlight",
    "annotate",
    "Highlight an object, or the part of a curve over an interval",
    args={"target": OBJECT_ID, "over": interval(), "id": OBJECT_ID, "style": STYLE},
    required=["target"],
    result="{id}",
    mask=HAS_OBJECTS,
)
def highlight(engine, target, over=None, id=None, style=None):
    obj = _target(engine, target)
    spec: dict[str, Any] = {"target": target}
    if over is not None:
        if obj.kind != "explicit":
            raise CallError("bad_args", "an interval can only be highlighted on a curve y = f(x)")
        spec["over"] = list(_range(engine, over, "highlight interval"))
    return {"id": _place(engine, "highlight", "highlight", id, {}, spec, style)}


@op(
    "scene.style",
    "annotate",
    "Change an object's style (keys not given are kept)",
    args={"target": OBJECT_ID, "style": STYLE},
    required=["target", "style"],
    result="none",
    mask=HAS_OBJECTS,
)
def restyle(engine, target, style):
    obj = _target(engine, target)
    table = engine.scene.objects if target in engine.scene.objects else engine.scene.notes
    table[target] = SceneObject(obj.id, obj.kind, obj.exprs, obj.spec, {**obj.style, **style}, obj.origin)
    return None


@op(
    "scene.remove",
    "annotate",
    "Remove an object or note (and highlights and notes attached to it)",
    args={"target": OBJECT_ID},
    required=["target"],
    result="{removed: string[]}",
    mask=HAS_OBJECTS,
)
def remove(engine, target):
    _target(engine, target)
    removed = [target]
    engine.scene.objects.pop(target, None)
    engine.scene.notes.pop(target, None)
    for table in (engine.scene.objects, engine.scene.notes):
        for key in list(table):
            spec = table[key].spec
            attached = spec.get("target") == target or spec.get("of") == target or spec.get("tangent_of") == target
            attached = attached or (isinstance(spec.get("at"), dict) and spec["at"].get("object") == target)
            if attached:
                table.pop(key)
                removed.append(key)
    return {"removed": removed}


# ---- view hints (camera) -----------------------------------------------------------


@op(
    "cam.fit",
    "camera",
    "Fit the view to the scene's objects (a view hint; not scene state)",
    result="none",
    changes_view=True,
)
def cam_fit(engine):
    engine.scene.view = {"mode": "fit"}
    return None


@op(
    "cam.view",
    "camera",
    "Set the visible x range, and optionally the y range (a view hint; not scene state)",
    args={"x": interval(), "y": interval()},
    required=["x"],
    result="none",
    changes_view=True,
)
def cam_view(engine, x, y=None):
    view: dict[str, Any] = {"mode": "window", "x": [exact_text(v) for v in _bounds(engine, x, "x range", finite=True)]}
    if y is not None:
        view["y"] = [exact_text(v) for v in _bounds(engine, y, "y range", finite=True)]
    engine.scene.view = view
    return None
