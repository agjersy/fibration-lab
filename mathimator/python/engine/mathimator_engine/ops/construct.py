"""Constructors: the only places math text and srepr are read (ARCHITECTURE 5.2)."""

from __future__ import annotations

import re

import sympy as sp

from .. import mathparse
from ..errors import CallError
from ..memo import Out
from ..registry import op
from ..schema import handle, scalar, string


@op(
    "sym.symbols",
    "construct",
    "Real symbols from names separated by spaces or commas",
    args={"names": string("e.g. 'x y' or 'x, t'", max_length=400)},
    required=["names"],
    result="list[handle:expr]",
    pure=True,
)
def symbols(engine, names: str):
    parts = [p for p in re.split(r"[\s,]+", names.strip()) if p]
    if not parts:
        raise CallError("bad_args", "no symbol names given")
    if len(parts) > 26:
        raise CallError("budget", "at most 26 symbols per call")
    return [Out(mathparse.symbol(p)) for p in parts]


@op(
    "sym.define",
    "construct",
    "Read math text into an expression, equation, or inequality",
    args={"text": string("Math text, e.g. 'a*sin(b*x)', 'x**2 + y**2 = 1', '0 < x < 1'")},
    required=["text"],
    result="handle",
    pure=True,
)
def define(engine, text: str):
    return Out(mathparse.read_math(text))


@op(
    "sym.from_srepr",
    "construct",
    "Rebuild a SymPy object from its srepr text (how SymPy objects built in a cell reach the engine)",
    args={"srepr": string(max_length=80_000)},
    required=["srepr"],
    result="handle",
    pure=True,
)
def from_srepr(engine, srepr: str):
    return Out(mathparse.read_srepr(srepr))


@op(
    "sym.piecewise",
    "construct",
    "A piecewise expression; pieces are tried in order, and 'otherwise' may close the list",
    args={
        "pieces": {
            "type": "array",
            "minItems": 1,
            "maxItems": 20,
            "items": {
                "type": "object",
                "properties": {"expr": scalar(), "when": scalar("a condition, or 'otherwise'")},
                "required": ["expr", "when"],
            },
        }
    },
    required=["pieces"],
    result="handle:expr",
    pure=True,
)
def piecewise(engine, pieces):
    pairs = []
    for i, piece in enumerate(pieces):
        expr = engine.expr(piece["expr"], "piece")
        when = piece["when"]
        if when == "otherwise":
            if i != len(pieces) - 1:
                raise CallError("bad_args", "'otherwise' must be the last piece")
            cond = sp.true
        else:
            cond = engine.value(when)
            if not isinstance(cond, sp.logic.boolalg.Boolean):
                raise CallError("bad_args", f"piece {i + 1}: the condition must be a comparison such as x < 0")
        pairs.append((expr, cond))
    return Out(sp.Piecewise(*pairs))


_ARITH = {"add", "sub", "mul", "div", "pow", "neg"}


@op(
    "expr.arith",
    "construct",
    "Arithmetic on expressions (what the SDK's operators call)",
    args={
        "op": string(enum=sorted(_ARITH)),
        "a": scalar(),
        "b": scalar(),
    },
    required=["op", "a"],
    result="handle:expr",
    pure=True,
)
def arith(engine, op: str, a, b=None):
    x = engine.expr(a, "left operand")
    if op == "neg":
        return Out(-x)
    if b is None:
        raise CallError("bad_args", f"{op} needs a second operand b")
    y = engine.expr(b, "right operand")
    if op == "add":
        return Out(x + y)
    if op == "sub":
        return Out(x - y)
    if op == "mul":
        return Out(x * y)
    if op == "div":
        if y == 0:
            raise CallError("math", "division by zero")
        return Out(x / y)
    return Out(x**y)


@op(
    "expr.relation",
    "construct",
    "An equation or inequality between two expressions",
    args={"lhs": scalar(), "rel": string(enum=["=", "<", "<=", ">", ">=", "!="]), "rhs": scalar()},
    required=["lhs", "rel", "rhs"],
    result="handle:relation",
    pure=True,
)
def relation(engine, lhs, rel: str, rhs):
    a, b = engine.expr(lhs), engine.expr(rhs)
    cls = {"=": sp.Eq, "<": sp.Lt, "<=": sp.Le, ">": sp.Gt, ">=": sp.Ge, "!=": sp.Ne}[rel]
    return Out(cls(a, b, evaluate=False))


@op(
    "expr.parts",
    "construct",
    "The two sides of an equation or inequality",
    args={"relation": handle("relation")},
    required=["relation"],
    result="{lhs, rhs}",
    pure=True,
)
def parts(engine, relation):
    rel = engine.handles.get(relation, ("relation",))
    return {"lhs": Out(rel.lhs), "rhs": Out(rel.rhs), "rel": rel.rel_op}
