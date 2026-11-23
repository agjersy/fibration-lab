"""The SDK over a real headless engine: proxies, operators, truth, errors, plotting."""

from __future__ import annotations

import pytest

from mathimator import MathError, NoLimit, Undecided, cam, note, param, plot, scene, sym


def test_operators_build_engine_objects():
    x = sym.symbols("x")
    f = 3 * x**2 - x / 2 + 1
    assert f.text == "3*x**2 - x/2 + 1"
    assert f.diff(x).text == "6*x - 1/2"
    assert (2 ** x).text == "2**x"
    assert (-x).text == "-x"
    assert f(x=2).text == "12"
    assert float(sym.define("pi")) == pytest.approx(3.141592653589793)


def test_symbols_and_names():
    x, y = sym.symbols("x y")
    assert (x.name, y.name) == ("x", "y")
    f = sym.define("x*y", name="f")
    assert repr(f) == "<Expr f = x*y>"
    assert f.free_symbols == ["x", "y"]


def test_truth_refuses_to_guess():
    x = sym.symbols("x")
    t = sym.define("cos(x) - x").roots(x, within=(0, 2)).complete
    assert t.value is None and not t.decided
    with pytest.raises(Undecided):
        bool(t)
    assert sym.define("sin(x)**2 + cos(x)**2").equals(1).proven


def test_limits():
    x = sym.symbols("x")
    assert sym.define("sin(x)/x").limit(x, 0).text == "1"
    with pytest.raises(NoLimit, match="left limit"):
        sym.define("1/x").limit(x, 0)
    assert sym.define("1/x**2").limit(x, 0).text == "oo"


def test_solve_raises_when_incomplete():
    x = sym.symbols("x")
    assert [s.text for s in (x**2 - 4).solve(x)] == ["-2", "2"]
    with pytest.raises(Undecided):
        sym.define("sin(x)").solve(x)
    assert len(sym.define("sin(x)").solve(x, within=(0, 7))) == 3


def test_errors_carry_kind_and_hint():
    with pytest.raises(MathError) as info:
        sym.define("2x")
    assert info.value.kind == "parse" and "2*x" in info.value.hint
    with pytest.raises(MathError) as info:
        plot.explicit("a*x", x=(-1, 1))
    assert info.value.kind == "bad_args" and "param.declare" in info.value.hint


def test_scene_building():
    sym.symbols("x")
    a, _b = param.declare(a=(1.0, -3, 3), b=(2.0, 0.1, 5, 0.1))
    f = a * sym.define("sin(b*x)")
    curve = plot.explicit(f, x=(-10, 10), id="curve")
    assert curve == "curve"
    t = plot.tangent("curve", at=0)
    assert t.line.text == "a*b*x"
    area = plot.area("curve", 0, "pi")
    assert area.value is not None
    note.label("peak", at="curve")
    param.set(a=2)
    cam.view(x=(-5, 5))
    state = scene.state()
    assert state["params"]["a"]["value"] == 2.0 and state["params"]["b"]["step"] == 0.1
    assert [o["kind"] for o in state["objects"]] == ["explicit", "explicit", "area"]
    assert state["view"]["mode"] == "window"
    removed = scene.remove("curve")
    assert set(removed) >= {"curve", t, area}


def test_available_operations_follow_the_scene():
    names = {o["name"] for o in scene.api(available_only=True)["operations"]}
    assert "plot.tangent" not in names
    plot.explicit("x**2", x=(-1, 1))
    names = {o["name"] for o in scene.api(available_only=True)["operations"]}
    assert "plot.tangent" in names


def test_from_sympy_round_trip():
    import sympy

    t = sympy.Symbol("t", real=True)
    obj = sym.from_sympy(sympy.exp(-t) * sympy.cos(3 * t))
    assert obj.text == "exp(-t)*cos(3*t)"
    assert obj.to_sympy() == sympy.exp(-t) * sympy.cos(3 * t)


def test_data_accepts_iterables():
    pid = plot.data(range(3), [0.0, 1.0, 4.0], mode="line")
    obj = next(o for o in scene.state()["objects"] if o["id"] == pid)
    assert obj["spec"]["y"] == [0.0, 1.0, 4.0]
