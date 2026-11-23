"""The restricted reader: exact, real, and unable to reach eval (ARCHITECTURE 16)."""

from __future__ import annotations

import pytest
import sympy as sp

from mathimator_engine.errors import CallError
from mathimator_engine.mathparse import read_math, read_srepr

x = sp.Symbol("x", real=True)
y = sp.Symbol("y", real=True)


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("x**2 + 1", x**2 + 1),
        ("x^2", x**2),
        ("0.1", sp.Rational(1, 10)),
        ("1e-3*x", x / 1000),
        ("sin(x)/x", sp.sin(x) / x),
        ("log(x, 2)", sp.log(x, 2)),
        ("ln(x)", sp.log(x)),
        ("abs(x - pi)", sp.Abs(x - sp.pi)),
        ("e**x", sp.exp(x)),
        ("-x", -x),
        ("arctan(x)", sp.atan(x)),
    ],
)
def test_reads_math_exactly(text, expected):
    assert read_math(text) == expected


def test_symbols_are_real():
    assert read_math("x").is_real is True


def test_equation_and_inequalities():
    assert read_math("x**2 + y**2 = 1") == sp.Eq(x**2 + y**2, 1)
    assert read_math("x <= 1") == sp.Le(x, 1)
    assert read_math("0 < x < 1") == sp.And(sp.Lt(0, x), sp.Lt(x, 1))
    assert read_math("x > 0 and y > 0") == sp.And(x > 0, y > 0)


@pytest.mark.parametrize(
    "text",
    [
        "__import__('os')",
        "().__class__",
        "x.__class__",
        "(lambda: 1)()",
        "[c for c in 'ab']",
        "'abc'",
        "f'{x}'",
        "open('/etc/passwd')",
        "eval('1')",
        "x if x else 1",
        "{1: 2}",
        "_x",
        "sin",
        "sin(x, y)",
        "x = y = 1",
        "a[0]",
        "x; y",
    ],
)
def test_rejects_everything_else(text):
    with pytest.raises(CallError) as info:
        read_math(text)
    assert info.value.kind in ("parse", "budget")


def test_implicit_multiplication_has_a_hint():
    with pytest.raises(CallError) as info:
        read_math("2x")
    assert "2*x" in (info.value.hint or "")


def test_size_limits():
    with pytest.raises(CallError) as info:
        read_math("+".join(["x"] * 3000))
    assert info.value.kind == "budget"
    with pytest.raises(CallError):
        read_math("2**100000")
    with pytest.raises(CallError):
        read_math("1" * 500)


@pytest.mark.parametrize(
    "obj",
    [
        x**2 + sp.sin(x) / 3,
        sp.Piecewise((x, x < 0), (x**2, True)),
        sp.Interval(0, 1, True, False) | sp.FiniteSet(3),
        sp.Eq(x, sp.pi),
        sp.Float("0.125", 30) * sp.exp(-x),
        sp.Integral(sp.exp(-(x**2)), (x, -sp.oo, sp.oo)),
        sp.S.EmptySet,
        -sp.oo,
    ],
)
def test_srepr_round_trip(obj):
    assert read_srepr(sp.srepr(obj)) == obj


@pytest.mark.parametrize(
    "text",
    ["__import__('os').system('true')", "Symbol('x').__class__", "Function('f')(Symbol('x'))", "Symbol('x', evaluate=__import__)"],
)
def test_srepr_rejects_unknown_constructs(text):
    with pytest.raises(CallError):
        read_srepr(text)
