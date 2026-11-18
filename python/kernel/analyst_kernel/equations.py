"""``eq.extract`` and ``eq.check`` (PROTOCOL.md 1.3, section 3, and section 9).

The kernel process never imports SymPy and never evaluates repository text. Parsing of
``Math:`` sections, tier A derivation, simplification, and rendering happen in the kernel's
helper process (``helper.Helper`` running ``eqcore``), which is killed when a request exceeds
its budget. ``eq.check`` evaluates both the expression and
the real function in the sandboxed session interpreter; the expression travels there as a
small JSON tree interpreted by a closed set of operations, not as code.
"""

from __future__ import annotations

import json
import logging
import math
import os
import random
import signal
import subprocess
import textwrap
import threading
from collections.abc import Callable
from pathlib import Path
from typing import Any

from .astutil import FuncDef, iter_functions, module_name, parse_file, read_source
from .errors import SCRIPT_NOT_FOUND, RpcError, invalid_params
from .helper import Helper
from .paths import rel_to_root, resolve_in_root
from .sandbox import SandboxSpec, rlimit_prologue, wrap

log = logging.getLogger("analyst_kernel.equations")

DEFAULT_TRIALS = 32
DEFAULT_TOL = 1e-6
ZERO_EPS = 1e-300
DEFAULT_RANGE = (0.1, 10.0)
CHECK_TIMEOUT_S = 60.0

# The equation analysis runs in the kernel's helper process; ``EqWorker`` is its client.
EqWorker = Helper

_default_worker: Helper | None = None
_default_lock = threading.Lock()


def default_worker() -> Helper:
    """An unsandboxed helper in this interpreter, for ``eq_extract`` outside a kernel (tests)."""
    global _default_worker
    with _default_lock:
        if _default_worker is None or _default_worker._closed:
            _default_worker = Helper()
        return _default_worker


# ---- extraction ----------------------------------------------------------------


def _analyze_function(worker: Helper, f: FuncDef, source: str) -> dict[str, Any]:
    """Equation fields plus the private keys ``_params``, ``_declared_error``, ``_check_tree``, ``_varargs``."""
    import ast

    out: dict[str, Any] = {
        "function": f.name,
        "qualname": f.qualname,
        "line": f.line,
        "tier": "none",
        "latex": None,
        "source_expr": None,
        "status": "error",
        "reason": None,
        "_params": [],
        "_declared_error": None,
        "_check_tree": None,
        "_varargs": False,
    }
    segment = ast.get_source_segment(source, f.node)
    if segment is None:
        out["reason"] = "could not read the function source"
        return out
    # get_source_segment starts at the ``def`` column; restore indentation so the body dedents.
    segment = " " * f.node.col_offset + segment
    res = worker.analyze(textwrap.dedent(segment), f.line - 1, f.is_method)
    if "error" in res:
        out["reason"] = res["error"]
        return out
    for key in ("tier", "latex", "source_expr", "status", "reason"):
        out[key] = res.get(key)
    out["_params"] = list(res.get("params") or [])
    out["_declared_error"] = res.get("declared_error")
    out["_check_tree"] = res.get("check_tree")
    out["_varargs"] = bool(res.get("varargs"))
    return out


def extract_file(root: Path, path: str, function: str | None = None, worker: Helper | None = None) -> list[dict[str, Any]]:
    file = resolve_in_root(root, path)
    if not file.is_file() or file.suffix != ".py":
        raise RpcError(SCRIPT_NOT_FOUND, f"{path} is not an existing Python file")
    tree = parse_file(file)
    if tree is None:
        raise RpcError(SCRIPT_NOT_FOUND, f"{path} could not be parsed")
    funcs = iter_functions(tree, file, root)
    if function:
        funcs = [f for f in funcs if f.qualname == function or f.name == function]
    if not funcs:
        return []
    source = read_source(file)
    worker = worker or default_worker()
    return [_analyze_function(worker, f, source) for f in funcs]


def public(eq: dict[str, Any]) -> dict[str, Any]:
    return {k: v for k, v in eq.items() if not k.startswith("_")}


def eq_extract(root: Path, path: str, function: str | None = None, worker: Helper | None = None) -> list[dict[str, Any]]:
    if not isinstance(path, str) or not path:
        raise invalid_params("path must be a non-empty string")
    return [public(e) for e in extract_file(root, path, function, worker)]


# ---- eq.check --------------------------------------------------------------------

# Runs in the session interpreter, inside the sandbox. ``ev`` interprets the expression tree
# produced by the worker: a closed set of arithmetic operations and functions, no eval.
CHECK_DRIVER = r'''
import importlib, importlib.util, json, math, sys
req = json.loads(sys.stdin.read())
sys.path.insert(0, req["root"])
sys.path.insert(0, req["dir"])
try:
    import numpy as _np
except Exception:
    _np = None
FUNCS = {"exp": math.exp, "log": math.log, "sin": math.sin, "cos": math.cos, "tan": math.tan,
         "asin": math.asin, "acos": math.acos, "atan": math.atan, "sinh": math.sinh,
         "cosh": math.cosh, "tanh": math.tanh, "abs": abs, "min": min, "max": max}
if _np is not None:
    REDUCE = {"mean": _np.mean, "sum": _np.sum, "min": _np.min, "max": _np.max}
else:
    REDUCE = {"mean": float, "sum": float, "min": float, "max": float}
def ev(t, env):
    k = t[0]
    if k == "num":
        return t[1]
    if k == "sym":
        return env[t[1]]
    if k == "add":
        s = ev(t[1], env)
        for u in t[2:]:
            s = s + ev(u, env)
        return s
    if k == "mul":
        s = ev(t[1], env)
        for u in t[2:]:
            s = s * ev(u, env)
        return s
    if k == "pow":
        return ev(t[1], env) ** ev(t[2], env)
    if k == "call":
        return FUNCS[t[1]](*[ev(u, env) for u in t[2:]])
    if k == "reduce":
        return REDUCE[t[1]](ev(t[2], env))
    raise ValueError("unknown node %r" % (k,))
def scalar(v):
    if _np is not None:
        a = _np.asarray(v)
        if a.size != 1:
            raise ValueError("non-scalar result of shape %s" % (a.shape,))
        return float(a.reshape(-1)[0])
    return float(v)
expected = []
for inputs in req["inputs"]:
    try:
        v = scalar(ev(req["tree"], inputs))
        expected.append(v if math.isfinite(v) else None)
    except Exception:
        expected.append(None)
def load():
    name = req.get("module")
    if name:
        try:
            return importlib.import_module(name)
        except Exception:
            pass
    spec = importlib.util.spec_from_file_location(req["stem"], req["path"])
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod
out = sys.stdout
sys.stdout = sys.stderr  # prints of the user's module must not reach the result line
try:
    mod = load()
    fn = mod
    for part in req["function"].split("."):
        fn = getattr(fn, part)
except Exception as exc:
    out.write(json.dumps({"error": "%s: %s" % (type(exc).__name__, exc)}) + "\n")
    sys.exit(0)
results = []
for inputs in req["inputs"]:
    try:
        results.append({"value": scalar(fn(**inputs)), "error": None})
    except Exception as exc:
        results.append({"value": None, "error": "%s: %s" % (type(exc).__name__, exc)})
out.write(json.dumps({"expected": expected, "results": results}) + "\n")
'''


def _sample_inputs(params: list[str], ranges: dict[str, Any] | None, trials: int) -> list[dict[str, float]]:
    rng = random.Random(0)
    bounds: dict[str, tuple[float, float]] = {}
    for p in params:
        lo, hi = DEFAULT_RANGE
        if ranges and p in ranges:
            r = ranges[p]
            if not (isinstance(r, (list, tuple)) and len(r) == 2):
                raise invalid_params(f"ranges[{p!r}] must be [lo, hi]")
            try:
                lo, hi = float(r[0]), float(r[1])
            except (TypeError, ValueError):
                raise invalid_params(f"ranges[{p!r}] must be [lo, hi]") from None
        bounds[p] = (lo, hi)
    return [{p: rng.uniform(*bounds[p]) for p in params} for _ in range(trials)]


def compare(
    inputs: list[dict[str, float]],
    expected: list[float | None],
    results: list[dict[str, Any]],
    tolerance: float,
) -> dict[str, Any]:
    """Apply the tolerance rule of PROTOCOL.md section 9 to the sampled values.

    Agree when ``|actual - expected| <= tol * max(|expected|, s)`` for every trial, where
    ``s`` is ``tol`` times the largest ``|expected|`` in the sample. The floor scales with
    the data, so quantities far below 1 are compared as strictly as quantities near 1.
    """
    rows: list[tuple[dict[str, float], float, float]] = []
    first_error: str | None = None
    for row, exp_v, res in zip(inputs, expected, results):
        if res.get("error"):
            first_error = first_error or res["error"]
            continue
        act = res.get("value")
        if exp_v is None or act is None:
            continue
        rows.append((row, float(exp_v), float(act)))
    if not rows:
        return {"evaluated": 0, "first_error": first_error}
    scale = max(abs(e) for _, e, _ in rows)
    if scale == 0.0:
        if any(not math.isfinite(a) or abs(a) > ZERO_EPS for _, _, a in rows):
            return {
                "evaluated": len(rows),
                "unavailable": "every expected value is exactly zero, so no relative tolerance applies; "
                "choose ranges where the expression is non-zero",
            }
        return {
            "evaluated": len(rows),
            "first_error": first_error,
            "max_abs": max(abs(a) for _, _, a in rows),
            "max_rel": 0.0,
            "counterexample": None,
        }
    floor = tolerance * scale
    max_abs = 0.0
    max_rel = 0.0
    counterexample = None
    for row, exp_v, act in rows:
        abs_err = abs(exp_v - act) if math.isfinite(act) else math.inf
        max_abs = max(max_abs, abs_err)
        if exp_v != 0:
            max_rel = max(max_rel, abs_err / abs(exp_v))
        if abs_err > tolerance * max(abs(exp_v), floor) and counterexample is None:
            counterexample = {"inputs": row, "expected": exp_v, "actual": act}
    return {
        "evaluated": len(rows),
        "first_error": first_error,
        "max_abs": max_abs,
        "max_rel": max_rel,
        "counterexample": counterexample,
    }


def eq_check(
    root: Path,
    python: str,
    sandbox_kind: str,
    spec_factory: Callable[[], SandboxSpec],
    env: dict[str, str],
    path: str,
    function: str,
    trials: int | None = None,
    tol: float | None = None,
    ranges: dict[str, Any] | None = None,
    worker: Helper | None = None,
) -> dict[str, Any]:
    if not isinstance(function, str) or not function:
        raise invalid_params("function must be a non-empty string")
    n = int(trials) if trials is not None else DEFAULT_TRIALS
    tolerance = float(tol) if tol is not None else DEFAULT_TOL
    if n <= 0 or n > 100000:
        raise invalid_params("trials must be between 1 and 100000")
    if not (tolerance > 0 and math.isfinite(tolerance)):
        raise invalid_params("tol must be a positive number")

    def unavailable(reason: str) -> dict[str, Any]:
        return {
            "status": "unavailable",
            "trials": 0,
            "max_abs_err": None,
            "max_rel_err": None,
            "counterexample": None,
            "reason": reason,
        }

    found = extract_file(root, path, function, worker)
    if not found:
        return unavailable(f"function {function!r} not found in {path}")
    eq = found[0]
    if eq["_declared_error"]:
        # Never fall back to checking the derived expression against the code it came from.
        return unavailable(eq["_declared_error"])
    tree = eq["_check_tree"]
    if tree is None:
        return unavailable(eq["reason"] or "no declared or derived equation")
    params = eq["_params"]
    if "." in eq["qualname"]:
        return unavailable("methods are not supported by eq.check")
    if eq["_varargs"]:
        return unavailable("functions with *args/**kwargs are not supported by eq.check")
    file = resolve_in_root(root, path)

    inputs = _sample_inputs(params, ranges, n)
    req = {
        "root": str(root),
        "dir": str(file.parent),
        "path": str(file),
        "stem": file.stem,
        "module": module_name(root, file) if _in_package(root, file) else None,
        "function": eq["qualname"],
        "inputs": inputs,
        "tree": tree,
    }
    spec = spec_factory()
    probe = wrap(sandbox_kind, spec, [])
    driver = rlimit_prologue(probe.rlimit_as, probe.rlimit_nofile) + CHECK_DRIVER
    cmd = wrap(sandbox_kind, spec, [python, "-I", "-c", driver])
    try:
        proc = subprocess.Popen(
            cmd.argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            cwd=str(root),
            env=env,
            start_new_session=True,
        )
    except OSError as exc:
        return unavailable(f"could not start session python: {exc}")
    try:
        stdout, stderr = proc.communicate(json.dumps(req), timeout=CHECK_TIMEOUT_S)
    except subprocess.TimeoutExpired:
        _kill_group(proc)
        return unavailable(f"evaluation exceeded {CHECK_TIMEOUT_S:g}s")
    try:
        data = json.loads(stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        err = (stderr or "").strip().splitlines()
        return unavailable("driver produced no result: " + (err[-1] if err else f"exit {proc.returncode}"))
    if not isinstance(data, dict):
        return unavailable("driver produced no result")
    if "error" in data:
        return unavailable(f"could not evaluate {function}: {data['error']}")
    expected = data.get("expected") or []
    results = data.get("results") or []
    if all(v is None for v in expected):
        return unavailable("the declared expression could not be evaluated on the sampled inputs")

    cmp = compare(inputs, expected, results, tolerance)
    if cmp["evaluated"] == 0:
        return unavailable(f"could not evaluate {function}: {cmp.get('first_error') or 'no finite results'}")
    if cmp.get("unavailable"):
        return unavailable(cmp["unavailable"])
    counterexample = cmp["counterexample"]
    first_error = cmp["first_error"]
    return {
        "status": "mismatch" if counterexample else "agree",
        "trials": cmp["evaluated"],
        "max_abs_err": _finite(cmp["max_abs"]),
        "max_rel_err": _finite(cmp["max_rel"]),
        "counterexample": counterexample,
        "reason": first_error if first_error and counterexample is None else None,
    }


def _kill_group(proc: subprocess.Popen) -> None:
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except OSError:
        try:
            proc.kill()
        except OSError:
            pass
    try:
        proc.communicate(timeout=5)
    except (subprocess.TimeoutExpired, OSError, ValueError):
        pass


def _finite(v: float) -> float | None:
    return v if math.isfinite(v) else None


def _in_package(root: Path, file: Path) -> bool:
    rel = file.resolve().relative_to(root.resolve())
    parts = rel.parts[:-1]
    if not parts:
        return False
    cur = root
    for part in parts:
        cur = cur / part
        if not (cur / "__init__.py").is_file():
            return False
    return True


def __getattr__(name: str) -> Any:
    # The SymPy-backed pieces live in eqcore; re-exported lazily so that importing this
    # module (and the kernel) never imports SymPy.
    if name in ("Unsupported", "derive_function", "simplify_with_budget", "parse_math_section"):
        from . import eqcore

        return getattr(eqcore, name)
    raise AttributeError(name)


__all__ = ["EqWorker", "compare", "eq_check", "eq_extract", "extract_file", "rel_to_root"]
