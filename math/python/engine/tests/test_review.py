"""Regressions from the November 2026 review of the first build: things the engine accepted
and should not have."""

from __future__ import annotations

import pytest

from fibration_math_engine.engine import Engine


def test_huge_results_are_a_budget_error_not_a_crash(s):
    err = s.err("sym.define", text="((2**1000)**1000)**30")
    assert err["kind"] == "budget"


@pytest.mark.parametrize("text", ["1/0", "0/0", "x/0", "oo - oo"])
def test_undefined_values_are_refused_at_construction(s, text):
    assert s.err("sym.define", text=text)["kind"] == "math"


def test_undefined_results_are_refused(s):
    f = s.define("1/(x - a)")
    err = s.err("subs", expr=f, values={"x": "a"})
    assert err["kind"] == "math" and "undefined" in err["detail"]
    assert s.err("evaluate", expr=s.define("sin(x)/x"), at={"x": 0})["kind"] == "math"
    # Signed infinities are values of the extended reals and stay allowed.
    assert s.ok("sym.define", text="-oo")


def test_tangent_at_a_pole_leaves_the_scene_unchanged(s):
    s.ok("plot.explicit", expr="1/x", var="x", id="c")
    before = s.engine.scene.hash()
    assert s.err("plot.tangent", of="c", at=0)["kind"] == "math"
    assert s.engine.scene.hash() == before


@pytest.mark.parametrize("name", ["pi", "e", "E", "I", "oo", "sin", "log", "max"])
def test_reserved_names_cannot_be_symbols_or_parameters(s, name):
    assert s.err("param.declare", name=name, value=1, min=0, max=2)["kind"] == "parse"
    assert s.err("sym.symbols", names=name)["kind"] == "parse"


def test_a_name_is_a_parameter_or_a_plot_variable_never_both(s):
    s.ok("param.declare", name="t", value=1, min=0, max=2)
    err = s.err("plot.explicit", expr="t**2", var="t")
    assert err["kind"] == "bad_args" and "parameter" in err["detail"]
    s.ok("plot.explicit", expr="x**2", var="x", id="c")
    err = s.err("param.declare", name="x", value=1, min=0, max=2)
    assert "plot variable of 'c'" in err["detail"]
    assert s.err("plot.implicit", equation="x = 1", x="x", y="x")["kind"] == "bad_args"


def test_interval_ends_must_be_numbers(s):
    assert "symbol" in s.err("plot.explicit", expr="x", var="x", domain=[0, "b"])["detail"]
    s.ok("plot.explicit", expr="x", var="x", domain=["-oo", "oo"], id="c")  # infinite domains are fine
    assert s.err("plot.area", of="c", over=[0, "oo"])["kind"] == "bad_args"
    assert s.err("plot.parametric", x="cos(t)", y="sin(t)", var="t", range=[0, "oo"])["kind"] == "bad_args"
    assert s.err("cam.view", x=["-oo", 1])["kind"] == "bad_args"
    assert s.err("plot.explicit", expr="x", var="x", domain=["I", 2])["kind"] == "bad_args"


def test_points_are_finite_and_real(s):
    assert s.err("plot.point", at=["oo", 0])["kind"] == "bad_args"
    assert s.err("plot.point", at=["sqrt(-1)", 0])["kind"] == "bad_args"
    s.ok("param.declare", name="a", value=1, min=0, max=2)
    s.ok("plot.point", at=["a", "a**2"])  # a point may follow a parameter
    assert s.err("plot.point", at=["b", 0])["kind"] == "bad_args"


@pytest.mark.parametrize("bad", ["", " p", "1p", "a b", "<script>", "p\nq", "é"])
def test_object_ids_are_plain_identifiers(s, bad):
    assert s.err("plot.point", at=[0, 0], id=bad)["kind"] == "bad_args"


def test_errors_never_escape_call():
    e = Engine()
    for fn, args in [
        ("sym.define", {"text": "factorial(3000)**3000"}),
        ("sym.from_srepr", {"srepr": "Integer(" + "9" * 300 + ")"}),
        ("numeric", {"expr": "x"}),
        ("diff", {}),
        ("diff", []),
    ]:
        out = e.call(fn, args)
        assert out["status"] in ("ok", "error")


# ---- sampler ---------------------------------------------------------------------------

VIEW = {"x": [-3, 3], "y": [-3, 3]}


def _sample(s, text):
    from fibration_math_engine.sample import sample

    s.ok("plot.explicit", expr=text, var="x", id="c")
    return sample(s.engine, "c", VIEW)


def test_real_roots_of_negative_numbers(s):
    assert s.obj(s.val("evaluate", expr=s.define("cbrt(x)"), at={"x": -8})) == -2
    assert s.obj(s.val("evaluate", expr=s.define("root(x, 5)"), at={"x": -32})) == -2
    assert s.err("sym.define", text="root(x, 1/2)")["kind"] == "parse"
    out = _sample(s, "cbrt(x)")
    assert out["status"] == "certified" and len(out["segments"]) == 1
    assert out["segments"][0][0][0] == -3.0  # drawn for negative x too


def test_steep_continuous_functions_are_drawn_through(s):
    out = _sample(s, "root(x, 9)")
    assert len(out["segments"]) == 1, out.get("reason")
    ys = [y for _, y in out["segments"][0]]
    assert min(abs(y) for y in ys) < 0.05  # passes through the origin, no gap


def test_erf_is_sampled_without_crashing(s):
    assert _sample(s, "erf(10*x)")["status"] == "certified"


def test_undefined_stretches_are_not_drawn_far_away(s):
    out = _sample(s, "1/floor(x)")  # undefined on [0, 1), where floor(x) is 0
    assert all(abs(y) < 1e6 for seg in out["segments"] for _, y in seg)
    assert not any(0.01 < seg[0][0] < 0.99 for seg in out["segments"])


def test_overflow_is_clamped_to_renderable_coordinates(s):
    out = _sample(s, "exp(1000*x)")
    assert all(abs(y) <= 6e6 + 3 for seg in out["segments"] for _, y in seg)


def test_sampler_failures_are_reported_not_raised(s, monkeypatch):
    import fibration_math_engine.sample as sample_mod
    from fibration_math_engine.errors import CallError

    s.ok("plot.explicit", expr="x", var="x", id="c")
    monkeypatch.setattr(sample_mod, "_geometry", lambda *a, **k: 1 / 0)
    with pytest.raises(CallError) as info:
        sample_mod.sample(s.engine, "c", VIEW)
    assert info.value.kind == "math"


def test_math_text_is_accepted_where_an_expression_is_expected(s):
    out = s.val("limit", expr="sin(x)/x", var="x", at=0)
    assert s.obj(out["value"]) == 1
    assert s.val("info", of="x**2 + 1")["head"] == "sum"
    assert s.val("expr.parts", relation="x = 1")["rel"] == "=="
    assert s.err("expr.parts", relation="x + 1")["kind"] == "bad_handle"
    assert s.err("set.is_empty", set="x")["kind"] == "bad_args"  # sets have no text form
    assert s.err("diff", expr="__import__('os')", var="x")["kind"] == "parse"


def test_long_staircases_certify_quickly_with_every_step_end_marked(s):
    import time

    from fibration_math_engine.sample import sample

    s.ok("plot.explicit", expr="floor(x)/2", var="x", domain=[-6, 6], id="c")
    started = time.perf_counter()
    out = sample(s.engine, "c", {"x": [-6, 6], "y": [-4, 4]})
    assert time.perf_counter() - started < 2.0
    assert out["status"] == "certified" and len(out["segments"]) == 12
    # No segment rises: each step is flat.
    assert all(abs(seg[0][1] - seg[-1][1]) < 1e-9 for seg in out["segments"])
    ends = [m for m in out["markers"] if m["kind"] == "jump_end"]
    assert sum(m["closed"] for m in ends) == 11 and sum(not m["closed"] for m in ends) == 11


@pytest.mark.parametrize(
    ("text", "point", "left", "right"),
    [
        ("floor(x)/2", 2, "1/2", "1"),
        ("ceiling(2*x)", "1/2", "1", "2"),
        ("floor(-x)", 3, "-3", "-4"),
        ("sign(sin(x))", "pi", "1", "-1"),
        ("x*Heaviside(x) + 1", 0, "1", "1"),
        ("floor(x**2)", 0, "0", "0"),  # a stationary point of the argument: SymPy's limit is used
    ],
)
def test_one_sided_limits_at_step_points(text, point, left, right):
    import sympy as sp

    from fibration_math_engine.mathparse import read_math
    from fibration_math_engine.sample import one_sided_limit

    x = sp.Symbol("x", real=True)
    f, p = read_math(text), read_math(str(point))
    assert one_sided_limit(f, x, p, "left") == read_math(left)
    assert one_sided_limit(f, x, p, "right") == read_math(right)


def test_an_unanalysed_jump_is_drawn_as_a_break_not_a_riser(s, monkeypatch):
    """When the symbolic analysis cannot run, the fallback must still not join across a jump,
    and must not break a steep continuous curve."""
    import fibration_math_engine.sample as sample_mod
    from fibration_math_engine.errors import CallError

    def no_analysis(*args, **kwargs):
        raise CallError("timeout", "simulated")

    monkeypatch.setattr(sample_mod, "_analyze", no_analysis)
    s.ok("plot.explicit", expr="floor(x)", var="x", id="steps")
    out = sample_mod.sample(s.engine, "steps", VIEW)
    assert out["status"] == "sampled"
    assert len(out["segments"]) == 6
    assert all(abs(seg[0][1] - seg[-1][1]) < 1e-9 for seg in out["segments"])
    s.ok("plot.explicit", expr="root(x, 9)", var="x", id="steep")
    assert len(sample_mod.sample(s.engine, "steep", VIEW)["segments"]) == 1
