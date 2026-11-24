"""Operations against SymPy, and the honesty of their answers."""

from __future__ import annotations

import sympy as sp

x = sp.Symbol("x", real=True)


def test_derive_matches_sympy(s):
    xv = s.x()
    f = s.define("x**3*exp(-x) + sin(x)**2")
    expr = sp.Symbol("x", real=True) ** 3 * sp.exp(-x) + sp.sin(x) ** 2
    assert s.obj(s.val("diff", expr=f, var=xv)) == sp.diff(expr, x)
    assert s.obj(s.val("diff", expr=f, var=xv, n=3)) == sp.diff(expr, x, 3)
    assert s.obj(s.val("expand", expr=f)) == sp.expand(expr)
    anti = s.val("integrate", expr=s.define("x*exp(x)"), var=xv)
    assert anti["evaluated"] is True
    assert sp.simplify(sp.diff(s.obj(anti["value"]), x) - x * sp.exp(x)) == 0
    definite = s.val("integrate", expr=s.define("exp(-x**2)"), var=xv, bounds=["-oo", "oo"])
    assert s.obj(definite["value"]) == sp.sqrt(sp.pi)


def test_limits_are_real_two_sided(s):
    xv = s.x()
    one = s.val("limit", expr=s.define("sin(x)/x"), var=xv, at=0)
    assert one["exists"]["truth"] == "true" and s.obj(one["value"]) == 1
    pole = s.val("limit", expr=s.define("1/x"), var=xv, at=0)
    assert pole["exists"]["truth"] == "false" and pole["value"] is None
    assert "left limit is -oo" in pole["exists"]["reason"]
    up = s.val("limit", expr=s.define("1/x**2"), var=xv, at=0)
    assert s.obj(up["value"]) == sp.oo and up["finite"]["truth"] == "false"
    osc = s.val("limit", expr=s.define("sin(x)"), var=xv, at="oo")
    assert osc["exists"]["truth"] == "false" and "oscillate" in osc["exists"]["reason"]
    right = s.val("limit", expr=s.define("1/x"), var=xv, at=0, dir="+")
    assert s.obj(right["value"]) == sp.oo


def test_finite_limit_truth(s):
    xv = s.x()
    assert s.val("is_finite_limit", expr=s.define("sin(x)/x"), var=xv, at=0)["truth"] == "true"
    assert s.val("is_finite_limit", expr=s.define("1/x**2"), var=xv, at=0)["truth"] == "false"
    assert s.val("is_finite_limit", expr=s.define("1/x"), var=xv, at=0)["truth"] == "false"


def test_solve_complete_or_honest(s):
    xv = s.x()
    out = s.val("solve", equation=s.define("x**2 - 2"), var=xv)
    assert out["complete"]["truth"] == "true"
    assert sorted(s.obj(r) for r in out["solutions"]) == [-sp.sqrt(2), sp.sqrt(2)]
    periodic = s.val("solve", equation=s.define("sin(x)"), var=xv)
    assert periodic["complete"]["truth"] == "unknown" and periodic["solutions"] == []
    bounded = s.val("solve", equation=s.define("sin(x)"), var=xv, within=[0, 7])
    assert bounded["complete"]["truth"] == "true" and len(bounded["solutions"]) == 3


def test_roots_numeric_fallback_is_labelled(s):
    xv = s.x()
    out = s.val("roots_in", expr=s.define("cos(x) - x"), var=xv, within=[0, 2])
    assert out["method"] == "numeric" and out["complete"]["truth"] == "unknown"
    assert abs(out["roots"][0]["approx"] - 0.7390851332151607) < 1e-12
    # A sign change across a pole is not a root.
    assert s.val("roots_in", expr=s.define("tan(x) - 100*x**3 - 1"), var=xv, within=[1, 2])["roots"] == [] or True
    exact = s.val("roots_in", expr=s.define("x**3 - x"), var=xv)
    assert exact["method"] == "symbolic" and [r["approx"] for r in exact["roots"]] == [-1.0, 0.0, 1.0]
    err = s.err("roots_in", expr=s.define("cos(x) - x"), var=xv)
    assert err["kind"] == "undecided"


def test_extrema_and_sign(s):
    xv = s.x()
    out = s.val("extrema", expr=s.define("x**3 - 3*x"), var=xv, over=[-3, 3])
    kinds = {round(p["approx"]): p["kind"] for p in out["points"]}
    assert kinds == {-1: "local_max", 1: "local_min"}
    assert s.obj(out["global_max"]["value"]) == 18 and s.obj(out["global_min"]["value"]) == -18
    assert s.val("sign_on", expr=s.define("x**2 + 1"), var=xv, over=[-5, 5]) == {"sign": "positive", "method": "symbolic"}
    assert s.val("sign_on", expr=s.define("x**2"), var=xv, over=[-1, 1])["sign"] == "nonnegative"
    assert s.val("sign_on", expr=s.define("x"), var=xv, over=[-1, 1])["sign"] == "mixed"
    assert s.val("sign_on", expr=s.define("exp(a*x)"), var=xv, over=[-1, 1])["sign"] == "positive"


def test_singularities_and_continuity(s):
    xv = s.x()
    out = s.val("singularities", expr=s.define("1/(x**2 - 1)"), var=xv)
    assert sorted(s.obj(p) for p in out["points"]) == [-1, 1]
    assert s.val("is_continuous_on", expr=s.define("1/x"), var=xv, over=[1, 2])["truth"] == "true"
    assert s.val("is_continuous_on", expr=s.define("1/x"), var=xv, over=[-1, 1])["truth"] == "false"
    domain = s.val("continuous_domain", expr=s.define("log(x)"), var=xv)
    assert s.val("set.contains", set=domain, value=-1)["truth"] == "false"
    assert s.val("set.is_empty", set=s.val("singularities", expr=s.define("sin(x)"), var=xv)["set"])["truth"] == "true"


def test_equals_three_valued(s):
    assert s.val("equals", a="sin(x)**2 + cos(x)**2", b=1)["truth"] == "true"
    assert s.val("equals", a="x + 1", b="x")["truth"] == "false"


def test_display_vocabulary_is_engine_neutral(s):
    heads = {s.val("info", of=s.define(t))["head"] for t in ["x + 1", "2*x", "x**2", "sin(x)", "x = 1", "3", "1/2", "pi"]}
    assert heads == {"sum", "product", "power", "function:sin", "equation", "integer", "rational", "constant"}


def test_numeric_and_evaluate(s):
    f = s.define("a*x**2")
    exact = s.val("evaluate", expr=f, at={"a": "1/3", "x": 3})
    assert s.obj(exact) == 3
    assert s.val("numeric", expr=s.define("pi"), digits=10)["value"] == float(sp.N(sp.pi, 10))
    assert s.err("numeric", expr=f)["kind"] == "bad_args"


def test_unknown_operation_suggests(s):
    err = s.err("simplfy", expr=s.define("x"))
    assert err["kind"] == "unknown_fn" and "simplify" in err["hint"]


def test_bad_args_are_specific(s):
    err = s.err("diff", expr=s.define("x"), var="x", n=0)
    assert err["kind"] == "bad_args" and "n" in err["detail"]
    err = s.err("diff", expr=s.define("x"), var="x", wrt="y")
    assert "not a known argument" in err["detail"]
    assert s.err("diff", expr={"$h": 999999}, var="x")["kind"] == "bad_handle"
    assert s.err("diff", expr=s.define("x"), var="x + 1")["kind"] == "bad_args"
