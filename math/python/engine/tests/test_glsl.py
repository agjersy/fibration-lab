"""``code.glsl``: the preview shader must compute the same mathematics as the engine.

The string-level tests always run. The numeric tests execute the generated GLSL in headless
Chromium (WebGL2) through ``glsl_eval.cjs`` and compare with mpmath; they skip when node,
Playwright, or WebGL2 with float render targets is missing.
"""

from __future__ import annotations

import json
import math
import shutil
import struct
import subprocess
from pathlib import Path

import mpmath
import pytest
import sympy as sp

from fibration_math_engine.engine import Engine
from fibration_math_engine.glsl import glsl, shader_header
from fibration_math_engine.server import Methods

PARAMS = {"a": 2.0, "b": 1.5, "c": 0.25, "k": 3.0}

# Explicit curves y = f(x). Chosen to cover every construct the emitter knows, including the
# places where GLSL leaves a result undefined and the real function is undefined too.
EXPLICIT = [
    "a*sin(b*x) + c*x",
    "x**2 - 3*x + 1",
    "x**3",
    "x**5 - x",
    "x**12/1000",
    "1/(x**2 + 1)",
    "x**(-3)",
    "sqrt(x)",
    "1/sqrt(x + 4)",
    "log(x)",
    "log(x, 2)",
    "exp(-x**2/2)",
    "abs(x) - floor(x) + ceiling(x/2)",
    "sign(x)*x**2",
    "cbrt(x)",
    "root(x, 5)",
    "x**(2/3)",
    "tan(x/3) + cot(x/8 + 3/2)",
    "sec(x/4) + csc(x/8 + 3/2)",
    "asin(x/4) + acos(x/4)",
    "asin(x)",
    "atan(x) + atan2(x, 2)",
    "sinh(x/2) + cosh(x/2) + tanh(x)",
    "asinh(x) + acosh(x + 5) + atanh(x/4)",
    "acosh(x)",
    "Max(x, x**2) + Min(x, 1)",
    "Heaviside(x)*x + Heaviside(-x)",
    "x**a",
    "x**b",
    "a**x",
    "2**x + pi*x + e",
    "0.1*x + 1/1000",
    "x**x",
]
XS = [-3.0, -2.5, -1.75, -1.0, -0.5, -0.125, 0.125, 0.5, 1.0, 1.75, 2.5, 3.0]


def f32(v: float) -> float:
    return struct.unpack("f", struct.pack("f", v))[0]


@pytest.fixture
def engine() -> Engine:
    e = Engine()
    for name, value in PARAMS.items():
        assert e.call("param.declare", {"name": name, "value": value, "min": -10, "max": 10})["status"] == "ok"
    return e


def plot(engine: Engine, fn: str, **args) -> str:
    out = engine.call(fn, args)
    assert out["status"] == "ok", out
    return out["value"]["id"]


# ---- string level ----------------------------------------------------------------------


def test_literals_are_floats_and_integer_powers_are_products(engine):
    out = glsl(engine, plot(engine, "plot.explicit", expr="3*x**2 - x/2 + 7", var="x"))
    assert out["supported"] and out["kind"] == "explicit" and out["precision"] == "highp"
    code = out["glsl"]
    assert code.startswith("float f(float x) { return ")
    assert "m_sq(x)" in code and "pow(" not in code
    assert "3.0" in code and "7.0" in code and "((-1.0) / 2.0)" in code
    assert out["uniforms"] == []


def test_parameters_are_uniforms_and_a_new_value_does_not_change_the_code(engine):
    oid = plot(engine, "plot.explicit", expr="a*sin(b*x)", var="x")
    first = glsl(engine, oid)
    assert first["uniforms"] == [
        {"name": "u_a", "param": "a", "value": 2.0},
        {"name": "u_b", "param": "b", "value": 1.5},
    ]
    assert "u_a" in first["glsl"] and "u_b" in first["glsl"] and "u_c" not in first["glsl"]
    engine.call("param.set", {"values": {"a": -1.0}})
    second = glsl(engine, oid)
    assert second["glsl"] == first["glsl"]  # a drag is a uniform update, never a recompile
    assert second["uniforms"][0]["value"] == -1.0
    assert shader_header(first).startswith("#version 300 es\nprecision highp float;\nuniform float u_a;\n")


def test_function_shape_per_kind(engine):
    cases = {
        "vec2 p(float t)": plot(engine, "plot.parametric", x="cos(3*t)", y="sin(2*t)", var="t", range=[0, 6]),
        "float r(float t)": plot(engine, "plot.polar", r="1 + cos(k*t)", var="t"),
        "float F(vec2 xy)": plot(engine, "plot.implicit", equation="x**2 + y**2 = a", x="x", y="y"),
        "bool inside(vec2 xy)": plot(engine, "plot.region", condition="y < sin(x) and y > 0", x="x", y="y"),
    }
    for signature, oid in cases.items():
        out = glsl(engine, oid)
        assert out["supported"], out
        assert out["glsl"].startswith(signature), out["glsl"]
    curve = plot(engine, "plot.explicit", expr="x**2", var="x", id="c")
    area = plot(engine, "plot.area", of="c", over=[0, 1])
    assert glsl(engine, area)["glsl"] == glsl(engine, curve)["glsl"]


@pytest.mark.parametrize("text", ["gamma(x)", "erf(x)", "factorial(x)", "binomial(x, 2)", "I*x", "re(x) + gamma(x)"])
def test_what_cannot_be_translated_is_refused_with_a_reason(engine, text):
    out = glsl(engine, plot(engine, "plot.explicit", expr=text, var="x"))
    assert out["supported"] is False and out["reason"] and "glsl" not in out


def test_unevaluated_integrals_and_other_kinds_are_refused(engine):
    plot(engine, "plot.explicit", expr="x", var="x", id="c")
    x = sp.Symbol("x", real=True)
    engine.scene.objects["c"].exprs["f"] = sp.Integral(sp.exp(sp.sin(x)), x)
    assert glsl(engine, "c")["supported"] is False
    point = plot(engine, "plot.point", at=[0, 0])
    out = glsl(engine, point)
    assert out["supported"] is False and "sample" in out["reason"]


def test_rpc_method_and_errors(engine):
    methods = Methods(engine)
    from fibration_math_engine.glsl import register

    register(methods)
    oid = plot(engine, "plot.explicit", expr="x", var="x")
    assert methods.table["code.glsl"]({"object": oid})["supported"] is True
    assert methods.table["code.glsl"]({"object": "nope"})["error"]["kind"] == "bad_args"
    assert methods.table["code.glsl"]({})["error"]["kind"] == "bad_args"


# ---- numeric agreement in a real WebGL2 implementation ----------------------------------


def run_in_browser(cases: list[dict]) -> dict:
    node = shutil.which("node")
    if node is None:
        pytest.skip("node is not installed")
    script = Path(__file__).with_name("glsl_eval.cjs")
    proc = subprocess.run(
        [node, str(script)], input=json.dumps({"cases": cases}), capture_output=True, text=True, timeout=180
    )
    if proc.returncode == 3:
        pytest.skip(proc.stderr.strip().splitlines()[-1] if proc.stderr.strip() else "WebGL2 is not available")
    assert proc.returncode == 0, proc.stderr
    return json.loads(proc.stdout)["results"]


def decode(v) -> float:
    return {"nan": math.nan, "inf": math.inf, "-inf": -math.inf}.get(v, v) if isinstance(v, str) else float(v)


def reference(expr, symbols, values) -> float:
    """The engine's meaning of expr at a point: a real number, or NaN where it is undefined."""
    fn = sp.lambdify(symbols, expr.subs({sp.Symbol(k, real=True): sp.Float(v) for k, v in PARAMS.items()}), "mpmath")
    try:
        y = fn(*(mpmath.mpf(v) for v in values))
        if isinstance(y, mpmath.mpc):
            if abs(y.imag) > 1e-12:
                return math.nan
            y = y.real
        y = float(y)
    except (ZeroDivisionError, ValueError, TypeError, OverflowError):
        return math.nan
    return y


def close(got: float, want: float) -> bool:
    if math.isnan(want):
        return math.isnan(got)
    if math.isinf(want):
        return got == want
    # GLSL does not fix the accuracy of sin, exp and friends. SwiftShader's sin(4.5) is off by
    # 1.8e-4, which is the largest error seen here; at preview scale that is a small fraction
    # of a pixel, and the certified render never uses these values.
    return abs(got - want) <= 1e-3 * max(1.0, abs(want))


def case(result: dict, cid: str, main: str, xs, ys=None) -> dict:
    return {
        "id": cid,
        "header": shader_header(result),
        "glsl": result["glsl"],
        "main": main,
        "xs": xs,
        "ys": ys or [],
        "uniforms": {u["name"]: u["value"] for u in result["uniforms"]},
    }


def test_generated_glsl_agrees_with_the_engine(engine):
    xs = [f32(v) for v in XS]
    cases, checks = [], []
    for i, text in enumerate(EXPLICIT):
        oid = plot(engine, "plot.explicit", expr=text, var="x", id=f"e{i}")
        out = glsl(engine, oid)
        assert out["supported"], (text, out)
        obj = engine.scene.objects[oid]
        cases.append(case(out, oid, "vec4(f(x), 0.0, 0.0, 1.0)", xs))
        checks.append((oid, text, [(reference(obj.exprs["f"], [obj.exprs["var"]], [x]),) for x in xs]))

    ts = [f32(v) for v in (0.0, 0.3, 1.0, 2.0, 3.5, 5.0)]
    oid = plot(engine, "plot.parametric", x="cos(3*t) + t/a", y="sin(2*t)*b", var="t", range=[0, 6], id="par")
    obj = engine.scene.objects[oid]
    cases.append(case(glsl(engine, oid), oid, "vec4(p(x), 0.0, 1.0)", ts))
    checks.append(
        (oid, "parametric", [tuple(reference(obj.exprs[n], [obj.exprs["var"]], [t]) for n in ("x", "y")) for t in ts])
    )

    oid = plot(engine, "plot.polar", r="1 + cos(k*t)/a", var="t", id="pol")
    obj = engine.scene.objects[oid]
    cases.append(case(glsl(engine, oid), oid, "vec4(r(x), 0.0, 0.0, 1.0)", ts))
    checks.append((oid, "polar", [(reference(obj.exprs["r"], [obj.exprs["var"]], [t]),) for t in ts]))

    px = [f32(v) for v in (-2.0, -1.0, -0.3, 0.0, 0.4, 1.0, 1.5, 2.5)]
    py = [f32(v) for v in (0.5, -1.0, 0.2, 0.0, 0.3, 0.9, -0.5, 0.1)]
    oid = plot(engine, "plot.implicit", equation="x**2 + y**2/c = a*x + 1", x="x", y="y", id="imp")
    obj = engine.scene.objects[oid]
    rel = obj.exprs["relation"]
    cases.append(case(glsl(engine, oid), oid, "vec4(F(vec2(x, y)), 0.0, 0.0, 1.0)", px, py))
    checks.append(
        (oid, "implicit", [(reference(rel.lhs - rel.rhs, [obj.exprs["x"], obj.exprs["y"]], [u, v]),) for u, v in zip(px, py)])
    )

    oid = plot(
        engine, "plot.region", condition="(y < sin(x) and y > 0) or x**2 + y**2 < c or not (x < 2)", x="x", y="y", id="reg"
    )
    obj = engine.scene.objects[oid]
    cond = obj.exprs["condition"].subs({sp.Symbol(k, real=True): v for k, v in PARAMS.items()})
    cases.append(case(glsl(engine, oid), oid, "vec4(inside(vec2(x, y)) ? 1.0 : 0.0, 0.0, 0.0, 1.0)", px, py))
    checks.append(
        (oid, "region", [(1.0 if bool(cond.subs({obj.exprs["x"]: u, obj.exprs["y"]: v})) else 0.0,) for u, v in zip(px, py)])
    )

    oid = plot(engine, "plot.explicit", expr="x", var="x", id="pw")
    x = sp.Symbol("x", real=True)
    engine.scene.objects[oid].exprs["f"] = sp.Piecewise((x**2, x < 0), (sp.sin(x), x < 2), (sp.Integer(1), x <= sp.Rational(5, 2)))
    cases.append(case(glsl(engine, oid), oid, "vec4(f(x), 0.0, 0.0, 1.0)", xs))
    pw = engine.scene.objects[oid].exprs["f"]
    checks.append((oid, "piecewise", [(float(pw.subs(x, v)) if v <= 2.5 else math.nan,) for v in xs]))

    results = run_in_browser(cases)
    failures = []
    undefined_points = 0
    for cid, label, expected in checks:
        got = results[cid]
        if isinstance(got, dict):
            failures.append(f"{label}: shader error: {got['error']}")
            continue
        for row, want in zip(got, expected):
            for g, w in zip(row, want):
                undefined_points += math.isnan(w)
                if not close(decode(g), w):
                    failures.append(f"{label}: got {decode(g)!r}, engine says {w!r}")
    assert not failures, "\n".join(failures[:40])
    assert undefined_points >= 20  # the undefined cases really were exercised
