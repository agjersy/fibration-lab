"""Equation analysis proper: tier A derivation, ``Math:`` parsing, simplification, rendering.

This module imports SymPy and is meant to run inside the equation worker subprocess
(``_eqworker``), never in the kernel process. Repository text is only ever read through
``ast.parse`` and a whitelisting walker: names become symbols, a fixed set of functions is
callable, and nothing else is accepted. ``sympy.parse_expr``, ``sympify`` and ``eval`` are
not used on repository text anywhere.
"""

from __future__ import annotations

import ast
import math
import re
import signal
import textwrap
import threading
from typing import Any

import sympy as sp

from .docstrings import function_docstring, latex_section, math_section

SIMPLIFY_BUDGET_S = 2.0
MAX_CONST_EXPONENT = 1000
MAX_CONST_DIGITS = 5000  # decimal digits a constant power may produce

SUPPORTED_FUNCS = {
    "exp": "exp",
    "log": "log",
    "log10": "log10",
    "log2": "log2",
    "sqrt": "sqrt",
    "sin": "sin",
    "cos": "cos",
    "tan": "tan",
    "arcsin": "asin",
    "arccos": "acos",
    "arctan": "atan",
    "asin": "asin",
    "acos": "acos",
    "atan": "atan",
    "sinh": "sinh",
    "cosh": "cosh",
    "tanh": "tanh",
    "abs": "Abs",
}
# Reductions over arrays. They have no closed form in the parameters, so they are kept as
# opaque functions for rendering and evaluated with NumPy semantics by ``eq.check``.
# Accepted in ``Math:`` sections only; tier A derivation stays as PROTOCOL.md section 3 lists.
REDUCTIONS = ("mean", "sum", "min", "max")
MODULE_PREFIXES = ("np", "numpy", "math")
CONSTANTS = {"pi": "pi", "e": "E"}
MATH_NAME_CONSTANTS = {"pi": sp.pi, "E": sp.E}

_LHS_RE = re.compile(r"^[A-Za-z_]\w*$")


class Unsupported(Exception):
    """Derivation stopped on a construct outside the supported subset."""


class _Budget(BaseException):
    """Raised by the alarm handler; a BaseException so library code cannot swallow it."""


def param_names(node: ast.FunctionDef | ast.AsyncFunctionDef, is_method: bool = False) -> list[str]:
    args = node.args
    names = [a.arg for a in args.posonlyargs + args.args + args.kwonlyargs]
    if is_method and names and names[0] in ("self", "cls"):
        names = names[1:]
    return names


def _node_desc(node: ast.AST) -> str:
    kind = type(node).__name__
    try:
        text = ast.unparse(node)
    except Exception:
        text = ""
    if len(text) > 40:
        text = text[:37] + "..."
    return f"{kind} `{text}`" if text else kind


class _Deriver:
    """Whitelisting AST walker shared by tier A (function bodies) and ``Math:`` expressions.

    ``math_mode`` is for declared expressions: unknown names become symbols (reported later
    as unknown), ``pi`` and ``E`` are constants, and the reductions are callable.
    """

    def __init__(self, params: list[str], math_mode: bool = False) -> None:
        self.symbols = {p: sp.Symbol(p) for p in params}
        self.locals: dict[str, Any] = {}
        self.math_mode = math_mode

    def derive(self, node: ast.FunctionDef | ast.AsyncFunctionDef):
        body = list(node.body)
        if body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant) and isinstance(body[0].value.value, str):
            body = body[1:]
        if not body:
            raise Unsupported(f"empty body at line {node.lineno}")
        for stmt in body[:-1]:
            self._assign(stmt)
        last = body[-1]
        if not isinstance(last, ast.Return) or last.value is None:
            raise Unsupported(f"{_node_desc(last)} at line {last.lineno} (expected a final return)")
        return self.expr(last.value)

    def _assign(self, stmt: ast.stmt) -> None:
        if isinstance(stmt, ast.Assign) and len(stmt.targets) == 1 and isinstance(stmt.targets[0], ast.Name):
            name, value = stmt.targets[0].id, stmt.value
        elif isinstance(stmt, ast.AnnAssign) and isinstance(stmt.target, ast.Name) and stmt.value is not None:
            name, value = stmt.target.id, stmt.value
        else:
            raise Unsupported(f"{_node_desc(stmt)} at line {stmt.lineno}")
        self.locals[name] = self.expr(value)

    def expr(self, node: ast.expr):
        if isinstance(node, ast.Name):
            if node.id in self.locals:
                return self.locals[node.id]
            if node.id in self.symbols:
                return self.symbols[node.id]
            if self.math_mode:
                if node.id in MATH_NAME_CONSTANTS:
                    return MATH_NAME_CONSTANTS[node.id]
                return sp.Symbol(node.id)
            raise Unsupported(f"unknown name `{node.id}` at line {node.lineno}")
        if isinstance(node, ast.Constant):
            if isinstance(node.value, bool) or not isinstance(node.value, (int, float)):
                raise Unsupported(f"{_node_desc(node)} at line {node.lineno}")
            if isinstance(node.value, int):
                return sp.Integer(node.value)
            if not math.isfinite(node.value):
                raise Unsupported(f"non-finite constant at line {node.lineno}")
            if float(node.value).is_integer() and abs(node.value) < 1e15:
                return sp.Integer(int(node.value))
            return sp.Float(node.value)
        if isinstance(node, ast.UnaryOp):
            if isinstance(node.op, ast.USub):
                return -self.expr(node.operand)
            if isinstance(node.op, ast.UAdd):
                return self.expr(node.operand)
            raise Unsupported(f"{_node_desc(node)} at line {node.lineno}")
        if isinstance(node, ast.BinOp):
            left, right = self.expr(node.left), self.expr(node.right)
            op = node.op
            if isinstance(op, ast.Add):
                return left + right
            if isinstance(op, ast.Sub):
                return left - right
            if isinstance(op, ast.Mult):
                return left * right
            if isinstance(op, ast.Div):
                if right.is_number and right == 0:
                    raise Unsupported(f"division by zero at line {node.lineno}")
                return left / right
            if isinstance(op, ast.Pow):
                return self._pow(left, right, node)
            raise Unsupported(f"operator {type(op).__name__} at line {node.lineno}")
        if isinstance(node, ast.Attribute):
            if isinstance(node.value, ast.Name) and node.value.id in MODULE_PREFIXES and node.attr in CONSTANTS:
                return getattr(sp, CONSTANTS[node.attr])
            raise Unsupported(f"{_node_desc(node)} at line {node.lineno}")
        if isinstance(node, ast.Call):
            return self._call(node)
        raise Unsupported(f"{_node_desc(node)} at line {node.lineno}")

    def _pow(self, left, right, node: ast.BinOp):
        """``left ** right`` with a budget: SymPy evaluates constant powers eagerly."""
        if right.is_number:
            try:
                magnitude = abs(float(right))
            except (TypeError, ValueError, OverflowError):
                magnitude = math.inf
            if not magnitude <= MAX_CONST_EXPONENT:
                raise Unsupported(f"constant exponent above {MAX_CONST_EXPONENT} at line {node.lineno}")
            if left.is_number:
                if left.is_Rational:
                    digits = max(_digits(left.p), _digits(left.q))
                else:
                    try:
                        digits = abs(math.log10(abs(float(left)) or 1.0))
                    except (TypeError, ValueError, OverflowError):
                        digits = math.inf
                if not digits * max(magnitude, 1.0) <= MAX_CONST_DIGITS:
                    raise Unsupported(f"constant power too large to evaluate at line {node.lineno}")
        return left**right

    def _call(self, node: ast.Call):
        func = node.func
        if isinstance(func, ast.Name):
            name = func.id
        elif isinstance(func, ast.Attribute) and isinstance(func.value, ast.Name) and func.value.id in MODULE_PREFIXES:
            name = func.attr
        else:
            raise Unsupported(f"call to {_node_desc(func)} at line {node.lineno}")
        if node.keywords or any(isinstance(a, ast.Starred) for a in node.args):
            raise Unsupported(f"call to `{name}` at line {node.lineno}")
        if self.math_mode and name in REDUCTIONS:
            if not node.args:
                raise Unsupported(f"call to `{name}` at line {node.lineno}")
            args = [self.expr(a) for a in node.args]
            if len(args) >= 2:
                if name == "min":
                    return sp.Min(*args)
                if name == "max":
                    return sp.Max(*args)
                raise Unsupported(f"call to `{name}` with {len(args)} arguments at line {node.lineno}")
            return sp.Function(name)(args[0])
        if name not in SUPPORTED_FUNCS or len(node.args) != 1:
            raise Unsupported(f"call to `{name}` at line {node.lineno}")
        arg = self.expr(node.args[0])
        target = SUPPORTED_FUNCS[name]
        if target == "log10":
            return sp.log(arg, 10)
        if target == "log2":
            return sp.log(arg, 2)
        return getattr(sp, target)(arg)


def _digits(n: int) -> float:
    n = abs(int(n))
    return math.log10(n) if n > 1 else 0.0


def simplify_with_budget(expr, seconds: float = SIMPLIFY_BUDGET_S):
    """``sympy.simplify`` under an interval timer; the unsimplified form when the budget runs out.

    The timer interrupts the computation itself, so nothing keeps running afterwards. It
    needs the main thread; anywhere else the expression is returned as is. (The equation
    worker runs on its main thread, and the kernel kills the whole worker as a backstop.)
    """
    if threading.current_thread() is not threading.main_thread() or not hasattr(signal, "setitimer"):
        return expr

    def on_alarm(signum, frame):
        raise _Budget()

    previous = signal.signal(signal.SIGALRM, on_alarm)
    signal.setitimer(signal.ITIMER_REAL, max(seconds, 0.001))
    try:
        return sp.simplify(expr)
    except _Budget:
        return expr
    except Exception:
        return expr
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def derive_function(node: ast.FunctionDef | ast.AsyncFunctionDef, params: list[str]):
    """Tier A: returns a simplified SymPy expression or raises Unsupported."""
    raw = _Deriver(params).derive(node)
    return simplify_with_budget(raw)


# ---- tier B --------------------------------------------------------------------


def parse_math_text(text: str, deriver: _Deriver):
    """One ``Math:`` right-hand side -> SymPy expression, through the whitelisting walker."""
    source = text.strip().replace("^", "**")  # SymPy-style power; ``^^`` stays a syntax error
    try:
        tree = ast.parse(source, mode="eval")
    except (SyntaxError, ValueError, RecursionError, MemoryError) as exc:
        raise ValueError(f"cannot parse {text!r}: {getattr(exc, 'msg', None) or exc}") from None
    try:
        return deriver.expr(tree.body)
    except Unsupported as exc:
        raise ValueError(f"{re.sub(r' at line [0-9]+', '', str(exc))} is not allowed in {text!r}") from None
    except RecursionError:
        raise ValueError(f"expression too deeply nested: {text[:40]!r}") from None


def parse_math_section(lines: list[tuple[str, str]], params: list[str]):
    """Parse ``lhs = expr`` lines; returns (lhs, expr) of the last line.

    Earlier lines define intermediate symbols that are substituted into later ones.
    """
    deriver = _Deriver(params, math_mode=True)
    result = None
    lhs_final = ""
    for lhs, rhs in lines:
        expr = parse_math_text(rhs, deriver)
        if lhs:
            if not _LHS_RE.match(lhs):
                raise ValueError(f"invalid left-hand side {lhs!r}")
            deriver.locals[lhs] = expr
        result = expr
        lhs_final = lhs
    if result is None:
        raise ValueError("empty Math: section")
    allowed = set(params)
    unknown = sorted(str(s) for s in result.free_symbols if str(s) not in allowed)
    if unknown:
        raise ValueError(f"unknown symbol(s) in Math: section: {', '.join(unknown)}")
    return lhs_final, result


# ---- numeric form for eq.check ---------------------------------------------------

_TREE_FUNCS = {
    "exp": "exp", "log": "log", "sin": "sin", "cos": "cos", "tan": "tan",
    "asin": "asin", "acos": "acos", "atan": "atan", "sinh": "sinh", "cosh": "cosh", "tanh": "tanh",
    "Abs": "abs",
}


def to_tree(expr) -> list:
    """A SymPy expression as nested JSON lists that the check driver evaluates without ``eval``.

    Raises ValueError for node types outside the closed set the driver knows.
    """
    if expr.is_Symbol:
        return ["sym", str(expr)]
    if expr.is_number and not expr.args:
        value = float(expr)
        if not math.isfinite(value):
            raise ValueError("non-finite constant")
        return ["num", value]
    if expr.is_Rational:
        return ["num", float(expr)]
    if expr.is_Add:
        return ["add", *[to_tree(a) for a in expr.args]]
    if expr.is_Mul:
        return ["mul", *[to_tree(a) for a in expr.args]]
    if expr.is_Pow:
        return ["pow", to_tree(expr.base), to_tree(expr.exp)]
    name = type(expr).__name__
    if isinstance(expr, (sp.Min, sp.Max)):
        return ["call", "min" if isinstance(expr, sp.Min) else "max", *[to_tree(a) for a in expr.args]]
    if name in _TREE_FUNCS and len(expr.args) == 1:
        return ["call", _TREE_FUNCS[name], to_tree(expr.args[0])]
    if isinstance(expr, sp.core.function.AppliedUndef) and name in REDUCTIONS and len(expr.args) == 1:
        return ["reduce", name, to_tree(expr.args[0])]
    raise ValueError(f"cannot evaluate {name} numerically")


# ---- one function -----------------------------------------------------------------


def analyze(source: str, line_offset: int = 0, is_method: bool = False) -> dict[str, Any]:
    """Analyze one function given its (dedented) source text.

    Returns the Equation fields that depend on analysis plus what ``eq.check`` needs:
    ``params``, ``declared_error``, ``check_tree`` (numeric form of the declared expression
    when one exists, else of the derived one), ``varargs``.
    """
    tree = ast.parse(textwrap.dedent(source))
    node = tree.body[0]
    if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
        raise ValueError("not a function definition")
    if line_offset:
        ast.increment_lineno(tree, line_offset)
    params = param_names(node, is_method)
    doc = function_docstring(node)
    out: dict[str, Any] = {
        "tier": "none",
        "latex": None,
        "source_expr": None,
        "status": "unsupported",
        "reason": None,
        "params": params,
        "declared_error": None,
        "check_tree": None,
        "varargs": bool(node.args.vararg or node.args.kwarg),
    }

    declared_expr = None
    declared_lhs = ""
    ms = math_section(doc)
    if ms is not None:
        try:
            declared_lhs, declared_expr = parse_math_section(ms.equations, params)
        except Exception as exc:
            out["declared_error"] = f"Math: section: {exc}"

    derived = None
    derived_raw = None
    try:
        derived_raw = _Deriver(params).derive(node)
        derived = simplify_with_budget(derived_raw)
    except Unsupported as exc:
        reason_a = str(exc)
    except RecursionError:
        reason_a = "expression too deeply nested"
    except Exception as exc:
        reason_a = f"{type(exc).__name__}: {exc}"

    check_expr = declared_expr if declared_expr is not None else derived_raw
    if check_expr is not None:
        for candidate in (check_expr, derived if declared_expr is None else None):
            if candidate is None:
                continue
            try:
                out["check_tree"] = to_tree(candidate)
                break
            except (ValueError, TypeError, RecursionError, OverflowError) as exc:
                out["check_error"] = str(exc)

    if derived is not None:
        out.update(tier="derived", status="ok", source_expr=str(derived), latex=sp.latex(derived))
        # A malformed Math: section is reported even though the code itself was derivable.
        out["reason"] = out["declared_error"]
        return out
    if ms is not None:
        out["tier"] = "declared"
        if declared_expr is not None:
            lhs_sym = sp.Symbol(declared_lhs) if declared_lhs else None
            rendered = sp.latex(declared_expr)
            out.update(
                status="ok",
                source_expr=str(declared_expr),
                latex=f"{sp.latex(lhs_sym)} = {rendered}" if lhs_sym is not None else rendered,
            )
        else:
            out.update(status="error", reason=out["declared_error"])
        return out
    ls = latex_section(doc)
    if ls is not None:
        out.update(tier="latex", status="ok", latex=" \\\\\n".join(ls.lines), reason="display only; not verifiable")
        return out
    out["reason"] = reason_a
    return out
