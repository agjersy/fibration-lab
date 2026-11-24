"""GLSL for the preview path (ARCHITECTURE 9.1): ``code.glsl``.

While a slider is dragged the renderer evaluates a scene object on the GPU, with the scene's
parameters as uniforms, so a new parameter value is a uniform update and never a recompile or
a round trip to the engine. This module turns one scene object into GLSL ES 3.00 (WebGL2).

Request ``{object: id}``; result::

    {object, kind, supported: true, precision: "highp",
     glsl:     the function for this object (see below),
     prelude:  helper functions the function may call (the same text for every object),
     uniforms: [{name: "u_a", param: "a", value: 1.0}, ...]}     # only those the object uses

    {object, kind, supported: false, reason}                       # no shader form; use `sample`

The function by object kind:

=============  ==========================  ===========================================
kind           function                    meaning
=============  ==========================  ===========================================
explicit       ``float f(float x)``        y = f(x)
area, riemann  ``float f(float x)``        the curve they belong to
parametric     ``vec2 p(float t)``         (x(t), y(t))
polar          ``float r(float t)``        r(theta)
implicit       ``float F(vec2 xy)``        lhs - rhs; the curve is F = 0
region         ``bool inside(vec2 xy)``    the condition
=============  ==========================  ===========================================

A complete shader is ``#version 300 es``, ``precision highp float;``, one
``uniform float <name>;`` per entry of ``uniforms``, ``prelude``, then ``glsl``.

Rules the translation keeps, because the preview must never show different mathematics from
the engine:

* It is an allowlist walk over the SymPy tree, like the engine's readers; a node it does not
  know makes the whole object ``supported: false`` with the reason. It never guesses. (SymPy's
  own ``sympy.printing.glsl`` is not used: it prints integer literals, emits ``pow`` for
  negative bases, and passes unknown functions through by name.)
* Every literal is a float literal. Rationals are divisions of exact floats.
* Where the real function is undefined the GLSL yields NaN on purpose, through the prelude's
  helpers, instead of relying on what GLSL leaves undefined: ``sqrt`` and ``log`` of a
  negative number, ``pow`` with a negative base, ``asin`` outside [-1, 1]. Integer powers are
  products (``pow(x, 2.0)`` is undefined for x < 0 in GLSL), so x**2 is right for negative x.
* A power with a non-constant exponent follows the real-number meaning: a negative base is
  defined only when the exponent is a whole number at run time.
* Values are 32-bit floats on the GPU. That is the precision of the preview only; the
  certified render (``sample``) is what the scene shows once the drag ends.
"""

from __future__ import annotations

import math
from typing import Any

import sympy as sp

from .errors import CallError

FLOAT32_MAX = 3.4028234e38

# Helpers every generated function may call. Kept small and branch-explicit: each one states
# what happens outside the real domain rather than leaving it to the GLSL implementation.
PRELUDE = """\
float m_nan() { return uintBitsToFloat(0x7fc00000u); }
float m_sq(float x) { return x * x; }
float m_cube(float x) { return x * x * x; }
float m_ipow(float b, int n) {
  float r = 1.0;
  int k = n < 0 ? -n : n;
  for (int i = 0; i < 64; i++) {
    if (k == 0) break;
    if ((k & 1) == 1) r *= b;
    b *= b;
    k >>= 1;
  }
  return n < 0 ? 1.0 / r : r;
}
float m_sqrt(float x) { return x < 0.0 ? m_nan() : sqrt(x); }
float m_log(float x) { return x > 0.0 ? log(x) : m_nan(); }
float m_cpow(float b, float e) {
  if (b > 0.0) return pow(b, e);
  if (b == 0.0) return e > 0.0 ? 0.0 : m_nan();
  return m_nan();
}
float m_rpow(float b, float e) {
  if (b > 0.0) return pow(b, e);
  if (b == 0.0) return e > 0.0 ? 0.0 : (e == 0.0 ? 1.0 : m_nan());
  if (e != floor(e)) return m_nan();
  float r = pow(-b, e);
  return mod(abs(e), 2.0) == 1.0 ? -r : r;
}
float m_asin(float x) { return abs(x) <= 1.0 ? asin(x) : m_nan(); }
float m_acos(float x) { return abs(x) <= 1.0 ? acos(x) : m_nan(); }
float m_acosh(float x) { return x >= 1.0 ? acosh(x) : m_nan(); }
float m_atanh(float x) { return abs(x) < 1.0 ? atanh(x) : m_nan(); }
float m_heaviside(float x, float h0) { return x > 0.0 ? 1.0 : (x < 0.0 ? 0.0 : h0); }
"""


class Unsupported(Exception):
    """The expression has no GLSL form here; the message is the reason given to the client."""


def literal(value: float) -> str:
    """A GLSL float literal for a finite Python float."""
    if not math.isfinite(value) or abs(value) > FLOAT32_MAX:
        raise Unsupported(f"the number {value} does not fit a 32-bit float")
    text = repr(float(value))
    if "e" not in text and "." not in text:
        text += ".0"
    return f"({text})" if value < 0 else text


_DIRECT = {
    sp.sin: "sin",
    sp.cos: "cos",
    sp.tan: "tan",
    sp.atan: "atan",
    sp.sinh: "sinh",
    sp.cosh: "cosh",
    sp.tanh: "tanh",
    sp.asinh: "asinh",
    sp.exp: "exp",
    sp.Abs: "abs",
    sp.sign: "sign",
    sp.floor: "floor",
    sp.ceiling: "ceil",
    sp.log: "m_log",
    sp.asin: "m_asin",
    sp.acos: "m_acos",
    sp.acosh: "m_acosh",
    sp.atanh: "m_atanh",
}
_RECIPROCAL = {sp.cot: "tan", sp.sec: "cos", sp.csc: "sin"}
_RELATIONS = {
    sp.StrictLessThan: "<",
    sp.LessThan: "<=",
    sp.StrictGreaterThan: ">",
    sp.GreaterThan: ">=",
    sp.Equality: "==",
    sp.Unequality: "!=",
}


class Emitter:
    """Walks a SymPy expression and writes GLSL. ``names`` maps each allowed symbol to its
    GLSL spelling (the function's argument, or a uniform); ``used`` collects the symbols seen."""

    def __init__(self, names: dict[sp.Symbol, str]):
        self.names = names
        self.used: set[sp.Symbol] = set()

    # ---- numbers ---------------------------------------------------------------------
    def number(self, e) -> str:
        if e.is_Integer:
            return literal(float(int(e)))
        if e.is_Rational:
            return f"({literal(float(int(e.p)))} / {literal(float(int(e.q)))})"
        if e.is_Float or isinstance(e, sp.NumberSymbol):
            return literal(float(e))
        raise Unsupported(f"the value {e} is not a real number")

    # ---- real-valued expressions ---------------------------------------------------------
    def expr(self, e) -> str:
        if isinstance(e, sp.Symbol):
            if e not in self.names:
                raise Unsupported(f"the symbol {e} is neither the plot variable nor a parameter")
            self.used.add(e)
            return self.names[e]
        if e.is_Number or isinstance(e, sp.NumberSymbol):
            if e.is_real is not True or e.is_finite is not True:
                raise Unsupported(f"the value {e} is not a finite real number")
            return self.number(e)
        if isinstance(e, sp.Add):
            return "(" + " + ".join(self.expr(a) for a in e.args) + ")"
        if isinstance(e, sp.Mul):
            return "(" + " * ".join(self.expr(a) for a in e.args) + ")"
        if isinstance(e, sp.Pow):
            return self.power(e)
        if isinstance(e, sp.Piecewise):
            return self.piecewise(e)
        if isinstance(e, sp.Max | sp.Min):
            fn = "max" if isinstance(e, sp.Max) else "min"
            out = self.expr(e.args[0])
            for a in e.args[1:]:
                out = f"{fn}({out}, {self.expr(a)})"
            return out
        if isinstance(e, sp.Heaviside):
            h0 = e.args[1] if len(e.args) > 1 else sp.Rational(1, 2)
            return f"m_heaviside({self.expr(e.args[0])}, {self.expr(h0)})"
        if isinstance(e, sp.atan2):
            return f"atan({self.expr(e.args[0])}, {self.expr(e.args[1])})"
        if isinstance(e, sp.re):
            return self.expr(e.args[0])
        if isinstance(e, sp.im):
            self.expr(e.args[0])  # still must be translatable
            return "0.0"
        fn = _DIRECT.get(type(e))
        if fn is not None and len(e.args) == 1:
            return f"{fn}({self.expr(e.args[0])})"
        base = _RECIPROCAL.get(type(e))
        if base is not None:
            return f"(1.0 / {base}({self.expr(e.args[0])}))"
        name = type(e).__name__
        raise Unsupported(f"{name} has no GLSL form")

    def power(self, e) -> str:
        base, exp = e.args
        if base is sp.E:
            return f"exp({self.expr(exp)})"
        b = self.expr(base)
        if exp.is_Integer:
            n = int(exp)
            if abs(n) > 1_000_000:
                raise Unsupported(f"the power {n} is too large")
            body = {1: b, 2: f"m_sq({b})", 3: f"m_cube({b})"}.get(abs(n)) or f"m_ipow({b}, {abs(n)})"
            return f"(1.0 / {body})" if n < 0 else body
        if exp.is_Rational:
            if exp == sp.Rational(1, 2):
                return f"m_sqrt({b})"
            if exp == sp.Rational(-1, 2):
                return f"(1.0 / m_sqrt({b}))"
            return f"m_cpow({b}, {self.number(exp)})"
        if exp.is_number:
            return f"m_cpow({b}, {self.expr(exp)})"
        return f"m_rpow({b}, {self.expr(exp)})"

    def piecewise(self, e) -> str:
        out = "m_nan()"
        for value, cond in reversed(e.args):
            v = self.expr(value)
            out = v if cond is sp.true else f"({self.cond(cond)} ? {v} : {out})"
        return out

    # ---- conditions ------------------------------------------------------------------
    def cond(self, c) -> str:
        if c is sp.true:
            return "true"
        if c is sp.false:
            return "false"
        op = _RELATIONS.get(type(c))
        if op is not None:
            return f"({self.expr(c.lhs)} {op} {self.expr(c.rhs)})"
        if isinstance(c, sp.And):
            return "(" + " && ".join(self.cond(a) for a in c.args) + ")"
        if isinstance(c, sp.Or):
            return "(" + " || ".join(self.cond(a) for a in c.args) + ")"
        if isinstance(c, sp.Not):
            return f"(!{self.cond(c.args[0])})"
        raise Unsupported(f"the condition {type(c).__name__} has no GLSL form")


def _uniform(name: str) -> str:
    return f"u_{name}"


def translate(kind: str, exprs: dict[str, Any], params: dict[str, float]) -> dict[str, Any]:
    """GLSL for one object's expressions. Raises Unsupported with the reason."""
    names: dict[sp.Symbol, str] = {sp.Symbol(p, real=True): _uniform(p) for p in params}

    def emitter(variables: dict[sp.Symbol, str]) -> Emitter:
        clash = [str(v) for v in variables if str(v) in params]
        if clash:
            raise Unsupported(f"{clash[0]} is both a parameter and a plot variable")
        return Emitter({**names, **variables})

    if kind in ("explicit", "area", "riemann"):
        em = emitter({exprs["var"]: "x"})
        code = f"float f(float x) {{ return {em.expr(exprs['f'])}; }}"
    elif kind == "parametric":
        em = emitter({exprs["var"]: "t"})
        code = f"vec2 p(float t) {{ return vec2({em.expr(exprs['x'])}, {em.expr(exprs['y'])}); }}"
    elif kind == "polar":
        em = emitter({exprs["var"]: "t"})
        code = f"float r(float t) {{ return {em.expr(exprs['r'])}; }}"
    elif kind == "implicit":
        em = emitter({exprs["x"]: "xy.x", exprs["y"]: "xy.y"})
        rel = exprs["relation"]
        code = f"float F(vec2 xy) {{ return {em.expr(rel.lhs)} - {em.expr(rel.rhs)}; }}"
    elif kind == "region":
        em = emitter({exprs["x"]: "xy.x", exprs["y"]: "xy.y"})
        code = f"bool inside(vec2 xy) {{ return {em.cond(exprs['condition'])}; }}"
    else:
        raise Unsupported(f"a {kind} has no shader form; draw it from `sample`")
    used = sorted(str(s) for s in em.used if str(s) in params)
    return {
        "glsl": code,
        "uniforms": [{"name": _uniform(p), "param": p, "value": float(params[p])} for p in used],
    }


def glsl(engine, object_id: Any) -> dict[str, Any]:
    """GLSL for one scene object (see the module docstring)."""
    if not isinstance(object_id, str):
        raise CallError("bad_args", "code.glsl needs object: the id of a scene object")
    obj = engine.scene.objects.get(object_id) or engine.scene.notes.get(object_id)
    if obj is None:
        raise CallError("bad_args", f"there is no object {object_id!r}")
    params = {name: p.value for name, p in engine.scene.params.items()}
    out: dict[str, Any] = {"object": object_id, "kind": obj.kind}
    try:
        out.update(translate(obj.kind, obj.exprs, params))
    except Unsupported as exc:
        out.update({"supported": False, "reason": str(exc)})
        return out
    except RecursionError:
        out.update({"supported": False, "reason": "the expression is too deeply nested"})
        return out
    out.update({"supported": True, "precision": "highp", "prelude": PRELUDE})
    return out


def shader_header(result: dict[str, Any]) -> str:
    """Version line, precision, uniform declarations and prelude for a supported result."""
    uniforms = "".join(f"uniform float {u['name']};\n" for u in result["uniforms"])
    return f"#version 300 es\nprecision highp float;\n{uniforms}{result['prelude']}"


def register(methods) -> None:
    def handler(params: dict) -> dict:
        try:
            return glsl(methods.engine, params.get("object"))
        except CallError as exc:
            return {"status": "error", "error": exc.to_json()}

    methods.register("code.glsl", handler)
