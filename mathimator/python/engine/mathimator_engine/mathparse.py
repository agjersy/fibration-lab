"""Reading math text and SymPy ``srepr`` strings without ``eval`` (ARCHITECTURE 5.2, 16).

Both readers go through ``ast.parse`` and a whitelisting walker that builds SymPy objects
directly. ``sympy.parse_expr``, ``sympify`` and ``eval`` are never called on text that came
from a cell, a model, or a file. This follows the Analyst kernel's ``eqcore`` rule.

Math text (``read_math``) is what people and models type::

    a*sin(b*x) + c*x        x**2 + y^2 = 1        0 < x < 2*pi and y > 0

* Numbers are exact: ``0.1`` is the rational 1/10, not a binary float.
* Every free name is a real symbol (the same symbol ``sym.symbols`` and ``param.declare``
  make), unless it is a known constant (``pi``, ``E``, ``e``, ``I``, ``oo``, ``inf``).
* Only the functions in ``FUNCTIONS`` may be called.
* A single top-level ``=`` makes an equation.
* There is no implicit multiplication: ``2x`` is an error whose hint says to write ``2*x``.

``srepr`` text (``read_srepr``) is what ``sympy.srepr`` prints. It is accepted for the
classes in ``SREPR_CLASSES`` only, and is how ``sym.from_sympy`` and the memo store move
SymPy objects across process boundaries.
"""

from __future__ import annotations

import ast
import re
from collections.abc import Callable
from typing import Any

import sympy as sp

from .errors import CallError

MAX_TEXT = 4000
MAX_NODES = 4000
MAX_INT_DIGITS = 200
MAX_CONST_EXPONENT = 1000
MAX_NAME = 32

_NAME_RE = re.compile(r"^[A-Za-z][A-Za-z0-9_]*$")


def symbol(name: str) -> sp.Symbol:
    """The one way the engine makes a symbol from a name: real-valued."""
    if not _NAME_RE.match(name) or len(name) > MAX_NAME:
        raise CallError("parse", f"{name!r} is not a valid symbol name", hint="use letters, digits and _")
    return sp.Symbol(name, real=True)


def _log(*args):
    if len(args) == 1:
        return sp.log(args[0])
    if len(args) == 2:
        return sp.log(args[0], args[1])
    raise CallError("parse", "log takes one argument, or two (value, base)")


def _root(x, n):
    return sp.root(x, n)


# name -> (callable, min args, max args)
FUNCTIONS: dict[str, tuple[Callable[..., Any], int, int]] = {
    "sin": (sp.sin, 1, 1),
    "cos": (sp.cos, 1, 1),
    "tan": (sp.tan, 1, 1),
    "cot": (sp.cot, 1, 1),
    "sec": (sp.sec, 1, 1),
    "csc": (sp.csc, 1, 1),
    "asin": (sp.asin, 1, 1),
    "acos": (sp.acos, 1, 1),
    "atan": (sp.atan, 1, 1),
    "arcsin": (sp.asin, 1, 1),
    "arccos": (sp.acos, 1, 1),
    "arctan": (sp.atan, 1, 1),
    "atan2": (sp.atan2, 2, 2),
    "sinh": (sp.sinh, 1, 1),
    "cosh": (sp.cosh, 1, 1),
    "tanh": (sp.tanh, 1, 1),
    "asinh": (sp.asinh, 1, 1),
    "acosh": (sp.acosh, 1, 1),
    "atanh": (sp.atanh, 1, 1),
    "exp": (sp.exp, 1, 1),
    "log": (_log, 1, 2),
    "ln": (sp.log, 1, 1),
    "sqrt": (sp.sqrt, 1, 1),
    "cbrt": (sp.cbrt, 1, 1),
    "root": (_root, 2, 2),
    "abs": (sp.Abs, 1, 1),
    "Abs": (sp.Abs, 1, 1),
    "sign": (sp.sign, 1, 1),
    "floor": (sp.floor, 1, 1),
    "ceiling": (sp.ceiling, 1, 1),
    "ceil": (sp.ceiling, 1, 1),
    "factorial": (sp.factorial, 1, 1),
    "gamma": (sp.gamma, 1, 1),
    "erf": (sp.erf, 1, 1),
    "Heaviside": (sp.Heaviside, 1, 1),
    "heaviside": (sp.Heaviside, 1, 1),
    "Max": (sp.Max, 2, 8),
    "Min": (sp.Min, 2, 8),
    "max": (sp.Max, 2, 8),
    "min": (sp.Min, 2, 8),
    "re": (sp.re, 1, 1),
    "im": (sp.im, 1, 1),
    "binomial": (sp.binomial, 2, 2),
}

CONSTANTS: dict[str, Any] = {
    "pi": sp.pi,
    "E": sp.E,
    "e": sp.E,
    "I": sp.I,
    "oo": sp.oo,
    "inf": sp.oo,
    "infinity": sp.oo,
}

_COMPARE = {
    ast.Lt: sp.Lt,
    ast.LtE: sp.Le,
    ast.Gt: sp.Gt,
    ast.GtE: sp.Ge,
    ast.Eq: sp.Eq,
    ast.NotEq: sp.Ne,
}


def _split_equation(text: str) -> tuple[str, str] | None:
    """Split on a single top-level ``=`` that is not part of ``==``, ``<=``, ``>=``, ``!=``."""
    positions = [
        i
        for i, ch in enumerate(text)
        if ch == "="
        and (i == 0 or text[i - 1] not in "=<>!")
        and (i + 1 >= len(text) or text[i + 1] != "=")
    ]
    if not positions:
        return None
    if len(positions) > 1:
        raise CallError("parse", "more than one '=' in the expression")
    i = positions[0]
    return text[:i], text[i + 1 :]


def _parse_tree(text: str) -> ast.Expression:
    try:
        tree = ast.parse(text.strip(), mode="eval")
    except SyntaxError as exc:
        hint = None
        if re.search(r"\d[A-Za-z(]", text):
            hint = "multiplication must be written out: 2*x, not 2x"
        raise CallError("parse", f"cannot read {text.strip()!r}: {exc.msg}", hint=hint) from None
    nodes = sum(1 for _ in ast.walk(tree))
    if nodes > MAX_NODES:
        raise CallError("budget", f"the expression has {nodes} parts; the limit is {MAX_NODES}")
    return tree


class _MathWalker:
    def __init__(self, source: str):
        self.source = source

    def build(self, node: ast.AST) -> Any:
        method = getattr(self, "_" + type(node).__name__, None)
        if method is None:
            raise CallError("parse", f"{type(node).__name__.lower()} is not allowed in math text")
        return method(node)

    def _Expression(self, node: ast.Expression):
        return self.build(node.body)

    def _Constant(self, node: ast.Constant):
        value = node.value
        if isinstance(value, bool) or not isinstance(value, int | float):
            raise CallError("parse", f"{value!r} is not a number")
        text = ast.get_source_segment(self.source, node) or repr(value)
        digits = sum(ch.isdigit() for ch in text)
        if digits > MAX_INT_DIGITS:
            raise CallError("budget", f"a number with {digits} digits is too long")
        if isinstance(value, int):
            return sp.Integer(value)
        # Exact decimal: "0.1" is 1/10. Scientific notation is exact too.
        try:
            return sp.Rational(text.replace("_", ""))
        except (TypeError, ValueError):
            return sp.Rational(repr(value))

    def _Name(self, node: ast.Name):
        if node.id in CONSTANTS:
            return CONSTANTS[node.id]
        if node.id in FUNCTIONS:
            raise CallError("parse", f"{node.id} is a function; call it, as in {node.id}(x)")
        if node.id.startswith("_"):
            raise CallError("parse", f"{node.id!r} is not a valid symbol name")
        return symbol(node.id)

    def _UnaryOp(self, node: ast.UnaryOp):
        operand = self.build(node.operand)
        if isinstance(node.op, ast.USub):
            return -operand
        if isinstance(node.op, ast.UAdd):
            return operand
        if isinstance(node.op, ast.Not):
            return sp.Not(operand)
        raise CallError("parse", "this unary operator is not allowed")

    def _BinOp(self, node: ast.BinOp):
        left = self.build(node.left)
        right = self.build(node.right)
        op = node.op
        if isinstance(op, ast.Add):
            return left + right
        if isinstance(op, ast.Sub):
            return left - right
        if isinstance(op, ast.Mult):
            return left * right
        if isinstance(op, ast.Div):
            return left / right
        if isinstance(op, ast.Pow | ast.BitXor):
            if right.is_number and right.is_Integer and abs(int(right)) > MAX_CONST_EXPONENT and left.is_number:
                raise CallError("budget", f"the constant power {right} is too large")
            return left**right
        raise CallError("parse", f"the operator {type(op).__name__} is not allowed")

    def _Call(self, node: ast.Call):
        if not isinstance(node.func, ast.Name) or node.func.id not in FUNCTIONS:
            name = node.func.id if isinstance(node.func, ast.Name) else "this"
            raise CallError(
                "parse",
                f"{name} is not a known function",
                hint="known functions: " + ", ".join(sorted({k for k in FUNCTIONS if k.islower()})),
            )
        if node.keywords:
            raise CallError("parse", "keyword arguments are not allowed in math text")
        fn, lo, hi = FUNCTIONS[node.func.id]
        if not lo <= len(node.args) <= hi:
            raise CallError("parse", f"{node.func.id} takes {lo if lo == hi else f'{lo} to {hi}'} argument(s)")
        args = [self.build(a) for a in node.args]
        return fn(*args)

    def _Compare(self, node: ast.Compare):
        parts = [self.build(node.left), *[self.build(c) for c in node.comparators]]
        rels = []
        for op, a, b in zip(node.ops, parts, parts[1:]):
            cls = _COMPARE.get(type(op))
            if cls is None:
                raise CallError("parse", "this comparison is not allowed")
            rels.append(cls(a, b))
        return rels[0] if len(rels) == 1 else sp.And(*rels)

    def _BoolOp(self, node: ast.BoolOp):
        values = [self.build(v) for v in node.values]
        return sp.And(*values) if isinstance(node.op, ast.And) else sp.Or(*values)


def read_math(text: str) -> Any:
    """Math text to a SymPy expression, relational, or boolean."""
    if not isinstance(text, str) or not text.strip():
        raise CallError("parse", "empty expression")
    if len(text) > MAX_TEXT:
        raise CallError("budget", f"the expression is longer than {MAX_TEXT} characters")
    split = _split_equation(text)
    if split is not None:
        lhs, rhs = (read_math(part) for part in split)
        return sp.Eq(lhs, rhs, evaluate=False)
    source = text.strip()
    return _MathWalker(source).build(_parse_tree(source))


# ---- srepr ---------------------------------------------------------------------------

_ASSUMPTIONS = {
    "real",
    "positive",
    "negative",
    "nonnegative",
    "nonpositive",
    "integer",
    "finite",
    "complex",
    "commutative",
    "extended_real",
    "zero",
    "nonzero",
}


def _srepr_symbol(name, **assumptions):
    if not isinstance(name, str) or not _NAME_RE.match(name) or len(name) > MAX_NAME:
        raise CallError("parse", f"bad symbol name in srepr: {name!r}")
    for key, value in assumptions.items():
        if key not in _ASSUMPTIONS or not isinstance(value, bool):
            raise CallError("parse", f"symbol assumption {key!r} is not allowed")
    return sp.Symbol(name, **assumptions)


def _srepr_integer(value):
    if not isinstance(value, int) or len(str(abs(value))) > MAX_INT_DIGITS:
        raise CallError("parse", "bad Integer in srepr")
    return sp.Integer(value)


def _srepr_rational(p, q):
    if not all(isinstance(v, int) and len(str(abs(v))) <= MAX_INT_DIGITS for v in (p, q)):
        raise CallError("parse", "bad Rational in srepr")
    return sp.Rational(p, q)


def _srepr_float(value, precision=53):
    if not isinstance(value, str | int | float) or not isinstance(precision, int) or not 1 <= precision <= 4096:
        raise CallError("parse", "bad Float in srepr")
    return sp.Float(value, precision=precision)


def _srepr_interval(a, b, left_open=False, right_open=False):
    return sp.Interval(a, b, bool(left_open), bool(right_open))


SREPR_CLASSES: dict[str, Callable[..., Any]] = {
    "Symbol": _srepr_symbol,
    "Integer": _srepr_integer,
    "Rational": _srepr_rational,
    "Float": _srepr_float,
    "Add": lambda *a: sp.Add(*a),
    "Mul": lambda *a: sp.Mul(*a),
    "Pow": lambda a, b: sp.Pow(a, b),
    "Abs": sp.Abs,
    "Equality": sp.Eq,
    "Unequality": sp.Ne,
    "StrictLessThan": sp.Lt,
    "LessThan": sp.Le,
    "StrictGreaterThan": sp.Gt,
    "GreaterThan": sp.Ge,
    "And": sp.And,
    "Or": sp.Or,
    "Not": sp.Not,
    "Piecewise": lambda *pairs: sp.Piecewise(*pairs),
    "ExprCondPair": lambda e, c: (e, c),
    "Interval": _srepr_interval,
    "Union": sp.Union,
    "Intersection": sp.Intersection,
    "Complement": sp.Complement,
    "FiniteSet": sp.FiniteSet,
    "Tuple": sp.Tuple,
    "Derivative": sp.Derivative,
    "Integral": sp.Integral,
    "Limit": sp.Limit,
    "Max": sp.Max,
    "Min": sp.Min,
}
for _name in ["sin", "cos", "tan", "cot", "sec", "csc", "asin", "acos", "atan", "atan2", "sinh", "cosh", "tanh", "asinh", "acosh", "atanh", "exp", "log", "sign", "floor", "ceiling", "factorial", "gamma", "erf", "Heaviside", "re", "im", "binomial"]:
    SREPR_CLASSES[_name] = getattr(sp, _name)

SREPR_NAMES: dict[str, Any] = {
    "pi": sp.pi,
    "E": sp.E,
    "I": sp.I,
    "oo": sp.oo,
    "zoo": sp.zoo,
    "nan": sp.nan,
    "true": sp.true,
    "false": sp.false,
    "Reals": sp.S.Reals,
    "Complexes": sp.S.Complexes,
    "Integers": sp.S.Integers,
    "Naturals": sp.S.Naturals,
    "EmptySet": sp.S.EmptySet,
    "Half": sp.S.Half,
    "One": sp.S.One,
    "Zero": sp.S.Zero,
    "NegativeOne": sp.S.NegativeOne,
    "Infinity": sp.oo,
    "NegativeInfinity": -sp.oo,
    "ImaginaryUnit": sp.I,
    "Pi": sp.pi,
    "Exp1": sp.E,
}


class _SreprWalker:
    def build(self, node: ast.AST) -> Any:
        if isinstance(node, ast.Expression):
            return self.build(node.body)
        if isinstance(node, ast.Constant) and isinstance(node.value, str | int | float | bool):
            return node.value
        if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.USub):
            value = self.build(node.operand)
            return -value
        if isinstance(node, ast.Name):
            if node.id in SREPR_NAMES:
                return SREPR_NAMES[node.id]
            if node.id in ("True", "False"):
                return node.id == "True"
            raise CallError("parse", f"{node.id} is not allowed in srepr")
        if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
            name = node.func.id
            if name in SREPR_NAMES and not node.args and not node.keywords:
                return SREPR_NAMES[name]  # e.g. NegativeOne()
            ctor = SREPR_CLASSES.get(name)
            if ctor is None:
                raise CallError("parse", f"{name} is not supported in srepr")
            args = [self.build(a) for a in node.args]
            kwargs = {}
            for kw in node.keywords:
                if kw.arg is None:
                    raise CallError("parse", "** is not allowed in srepr")
                kwargs[kw.arg] = self.build(kw.value)
            try:
                return ctor(*args, **kwargs)
            except CallError:
                raise
            except Exception as exc:  # malformed constructor arguments
                raise CallError("parse", f"cannot rebuild {name}: {exc}") from None
        raise CallError("parse", f"{type(node).__name__} is not allowed in srepr")


def read_srepr(text: str) -> Any:
    if not isinstance(text, str) or not text.strip():
        raise CallError("parse", "empty srepr")
    if len(text) > 20 * MAX_TEXT:
        raise CallError("budget", "the srepr text is too long")
    return _SreprWalker().build(_parse_tree(text))
