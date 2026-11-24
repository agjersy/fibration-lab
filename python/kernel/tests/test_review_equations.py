"""Regression tests for the November 2026 review: G1, K1, K6, K7 (docs/REVIEW.md, PROTOCOL.md 9)."""

from __future__ import annotations

import ast
import os
import threading
import time
from pathlib import Path

import pytest

from analyst_kernel.equations import EqWorker, compare, eq_extract

KERNEL_PKG = Path(__file__).resolve().parent.parent / "analyst_kernel"


def by_name(kernel, path):
    return {e["function"]: e for e in kernel.call("eq.extract", {"path": path})["equations"]}


def chr_sum(text: str) -> str:
    return "+".join(f"chr({ord(c)})" for c in text)


# ---- G1 ------------------------------------------------------------------------------


def test_g1_math_docstring_never_executes_code(kernel, repo: Path, tmp_path: Path):
    marker = tmp_path / "pwned"
    payload = f"open({str(marker)!r},'w').write('x')"
    exploit = f"0*eval({chr_sum(payload)})"
    assert set(exploit) <= set("0123456789*+()chrevalx"), "the exploit only needs characters the old filter allowed"
    (repo / "analysis" / "evil.py").write_text(
        f'def loud(x):\n    """Doc.\n\n    Math:\n        y = {exploit}\n    """\n    return [x]\n\n'
        f'def quiet(x):\n    """Doc.\n\n    Math:\n        y = {exploit}\n    """\n    return x + 1\n\n'
        'def dunder(x):\n    """Doc.\n\n    Math:\n        y = x.__class__.__mro__\n    """\n    return [x]\n\n'
        'def call_attr(x):\n    """Doc.\n\n    Math:\n        y = x.real(1)\n    """\n    return [x]\n\n'
        'def lam(x):\n    """Doc.\n\n    Math:\n        y = (lambda: 1)()\n    """\n    return [x]\n\n'
        'def sub(x):\n    """Doc.\n\n    Math:\n        y = x[0]\n    """\n    return [x]\n'
    )
    eqs = by_name(kernel, "analysis/evil.py")
    assert not marker.exists(), "the Math: expression was executed"
    loud = eqs["loud"]
    assert loud["tier"] == "declared" and loud["status"] == "error" and loud["latex"] is None
    assert "eval" in loud["reason"] and "not allowed" in loud["reason"]
    # the code itself is derivable: the malformed Math: section is still reported (K7)
    quiet = eqs["quiet"]
    assert quiet["tier"] == "derived" and quiet["source_expr"] == "x + 1" and "eval" in quiet["reason"]
    for name in ("dunder", "call_attr", "lam", "sub"):
        assert eqs[name]["status"] == "error", name
    for name in ("loud", "quiet"):
        res = kernel.call("eq.check", {"path": "analysis/evil.py", "function": name})
        assert res["status"] == "unavailable" and "eval" in res["reason"]
    assert not marker.exists()


def test_g1_no_eval_based_parsers_in_the_kernel_sources():
    banned = {"parse_expr", "sympify", "lambdify"}
    for file in KERNEL_PKG.glob("*.py"):
        tree = ast.parse(file.read_text())
        for node in ast.walk(tree):
            name = node.id if isinstance(node, ast.Name) else node.attr if isinstance(node, ast.Attribute) else None
            assert name not in banned, f"{file.name} uses {name}"
            if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
                assert node.func.id not in ("eval", "exec"), f"{file.name}:{node.lineno} calls {node.func.id}"


def test_g1_kernel_process_never_imports_sympy(repo: Path, tmp_path: Path):
    import subprocess
    import sys

    code = (
        "import sys\n"
        "from analyst_kernel.kernel import Kernel\n"
        "k = Kernel(sys.executable, sys.argv[1], sys.argv[2], sandbox='limits')\n"
        "eqs = k.call('eq.extract', {'path': 'analysis/models.py'})['equations']\n"
        "k.close()\n"
        "print(len(eqs), 'sympy' in sys.modules)\n"
    )
    out = subprocess.run(
        [sys.executable, "-c", code, str(repo), str(tmp_path / "runs")],
        capture_output=True, text=True, cwd=str(KERNEL_PKG.parent), check=True,
    ).stdout
    assert out.split() == ["5", "False"]


def test_reductions_render_and_check_numerically(kernel, repo: Path):
    (repo / "analysis" / "stats.py").write_text(
        "import numpy as np\n\n"
        'def rmse(y, yhat):\n    """Root mean square error.\n\n    Math:\n        rmse = sqrt(mean((y - yhat)**2))\n    """\n'
        "    return float(np.sqrt(np.mean((y - yhat) ** 2)))\n\n"
        'def wrong(y, yhat):\n    """Doc.\n\n    Math:\n        r = sqrt(sum((y - yhat)**2)) + max(y, yhat) + abs(min(y))\n    """\n'
        "    return float(np.sqrt(np.sum((y - yhat) ** 2)) + max(y, yhat) + abs(y) + 1.0)\n"
    )
    eqs = by_name(kernel, "analysis/stats.py")
    assert eqs["rmse"]["tier"] == "declared" and eqs["rmse"]["status"] == "ok"
    assert eqs["rmse"]["source_expr"] == "sqrt(mean((y - yhat)**2))"
    assert "mean" in eqs["rmse"]["latex"] and eqs["rmse"]["latex"].startswith("rmse = ")
    res = kernel.call("eq.check", {"path": "analysis/stats.py", "function": "rmse"})
    assert res["status"] == "agree" and res["trials"] == 32
    res = kernel.call("eq.check", {"path": "analysis/stats.py", "function": "wrong"})
    assert res["status"] == "mismatch" and abs(res["counterexample"]["actual"] - res["counterexample"]["expected"] - 1.0) < 1e-9


# ---- K1 ------------------------------------------------------------------------------


def test_k1_huge_constant_powers_are_rejected_quickly(kernel, repo: Path):
    (repo / "analysis" / "pow.py").write_text(
        "def tower(x):\n    return x * 9 ** 9 ** 9\n\n"
        "def big_symbolic(x):\n    return x ** 1001\n\n"
        "def ok(x):\n    return x ** 1000 + 2 ** 100\n\n"
        "def digits(x):\n    return x + (10 ** 999) ** 999\n\n"
        'def declared(x):\n    """Doc.\n\n    Math:\n        y = x * 9 ** 9 ** 9\n    """\n    return [x]\n'
    )
    t0 = time.monotonic()
    eqs = by_name(kernel, "analysis/pow.py")
    assert time.monotonic() - t0 < 20
    assert eqs["tower"]["status"] == "unsupported" and "exponent above 1000" in eqs["tower"]["reason"]
    assert eqs["big_symbolic"]["status"] == "unsupported" and "exponent above 1000" in eqs["big_symbolic"]["reason"]
    assert eqs["ok"]["status"] == "ok" and "x**1000" in eqs["ok"]["source_expr"]
    assert eqs["digits"]["status"] == "unsupported" and "too large" in eqs["digits"]["reason"]
    assert eqs["declared"]["status"] == "error" and "exponent above 1000" in eqs["declared"]["reason"]
    # the kernel stays responsive
    assert kernel.call("eq.extract", {"path": "analysis/models.py", "function": "gaussian"})["equations"][0]["status"] == "ok"


def _alive(pid: int) -> bool:
    try:
        with open(f"/proc/{pid}/stat") as fh:
            return fh.read().rsplit(")", 1)[1].split()[0] != "Z"
    except OSError:
        try:
            os.kill(pid, 0)
        except OSError:
            return False
        return True


def test_k1_worker_is_killed_on_budget_and_leaves_nothing_behind(repo: Path):
    # 60 distinct trigonometric products: simplify needs its whole 2 s budget, the request budget is shorter
    terms = " + ".join(f"np.sin({i}*x + y)**2 * np.cos(x - {i}*y)**3 / (1 + np.tan(x*{i})**2)" for i in range(1, 61))
    (repo / "analysis" / "slow.py").write_text(f"import numpy as np\n\ndef slow(x, y):\n    return {terms}\n\ndef fast(x):\n    return 2 * x\n")
    threads_before = threading.active_count()
    worker = EqWorker(budget_s=0.4)
    try:
        assert eq_extract(repo, "analysis/slow.py", "fast", worker)[0]["source_expr"] == "2*x"
        pid = worker._proc.pid
        t0 = time.monotonic()
        slow = eq_extract(repo, "analysis/slow.py", "slow", worker)[0]
        assert time.monotonic() - t0 < 5
        assert slow["status"] == "error" and slow["tier"] == "none" and "exceeded 0.4s" in slow["reason"]
        assert not _alive(pid), "the worker kept running after its budget"
        assert threading.active_count() == threads_before, "an abandoned thread was left behind"
        # a fresh worker is started for the next request
        assert eq_extract(repo, "analysis/slow.py", "fast", worker)[0]["status"] == "ok"
        assert worker._proc.pid != pid
    finally:
        worker.close()
    assert worker._proc is None


def test_k1_simplify_budget_interrupts_instead_of_abandoning_a_thread(monkeypatch):
    import sympy

    from analyst_kernel.eqcore import simplify_with_budget

    x = sympy.Symbol("x")
    expr = (x + 1) ** 2 - x**2 - 2 * x
    finished = []

    def slow(e):
        time.sleep(1.0)
        finished.append(True)
        return sympy.Integer(1)

    monkeypatch.setattr(sympy, "simplify", slow)
    before = threading.active_count()
    t0 = time.monotonic()
    assert simplify_with_budget(expr, seconds=0.05) == expr
    assert time.monotonic() - t0 < 0.8 and threading.active_count() == before
    time.sleep(1.2)
    assert finished == [], "the computation kept running after the budget"


# ---- K6 ------------------------------------------------------------------------------


def _results(values):
    return [{"value": v, "error": None} for v in values]


def test_k6_tolerance_scales_with_the_data():
    inputs = [{"x": float(i)} for i in range(1, 5)]
    planck = [6.62607015e-34 * i for i in range(1, 5)]
    # relative error 95: used to "agree" because everything was under the fixed 1e-9 floor
    off = compare(inputs, planck, _results([v * 96 for v in planck]), 1e-6)
    assert off["counterexample"] is not None and off["max_rel"] > 90
    close = compare(inputs, planck, _results([v * (1 + 1e-9) for v in planck]), 1e-6)
    assert close["counterexample"] is None
    # near a zero crossing the floor is tol * tol * largest |expected|, not an absolute number
    expected = [1e-30, 1.0, 2.0, 3.0]
    assert compare(inputs, expected, _results([3e-12, 1.0, 2.0, 3.0]), 1e-6)["counterexample"] is None
    assert compare(inputs, expected, _results([1e-9, 1.0, 2.0, 3.0]), 1e-6)["counterexample"] is not None
    # every expected value exactly zero: no relative statement is possible
    zeros = [0.0] * 4
    assert "unavailable" in compare(inputs, zeros, _results([0.0, 0.0, 1e-20, 0.0]), 1e-6)
    agree = compare(inputs, zeros, _results(zeros), 1e-6)
    assert "unavailable" not in agree and agree["counterexample"] is None


def test_k6_eq_check_on_tiny_quantities(kernel, repo: Path):
    (repo / "analysis" / "tiny.py").write_text(
        'def energy(nu):\n    """Photon energy in joules.\n\n    Math:\n        E_ph = 6.62607015e-34 * nu\n    """\n'
        "    return 6.62607015e-32 * nu\n\n"
        'def energy_ok(nu):\n    """Photon energy in joules.\n\n    Math:\n        E_ph = 6.62607015e-34 * nu\n    """\n'
        "    return nu * 6.62607015e-34\n\n"
        'def zero(x):\n    """Doc.\n\n    Math:\n        y = 0 * x\n    """\n    return [x][0] * 1e-12\n'
    )
    res = kernel.call("eq.check", {"path": "analysis/tiny.py", "function": "energy"})
    assert res["status"] == "mismatch" and res["max_rel_err"] > 90
    assert kernel.call("eq.check", {"path": "analysis/tiny.py", "function": "energy_ok"})["status"] == "agree"
    res = kernel.call("eq.check", {"path": "analysis/tiny.py", "function": "zero"})
    assert res["status"] == "unavailable" and "exactly zero" in res["reason"]


# ---- K7 ------------------------------------------------------------------------------


def test_k7_malformed_math_is_reported_even_when_derivation_succeeds(kernel, repo: Path):
    (repo / "analysis" / "k7.py").write_text(
        'def area(r):\n    """Doc.\n\n    Math:\n        A = pi * r ^^ 2\n    """\n    return 3.0 * r ** 2\n'
    )
    eq = by_name(kernel, "analysis/k7.py")["area"]
    assert eq["tier"] == "derived" and eq["status"] == "ok" and eq["source_expr"] == "3*r**2"
    assert eq["reason"] and eq["reason"].startswith("Math: section:")
    res = kernel.call("eq.check", {"path": "analysis/k7.py", "function": "area"})
    # it used to compare the derived expression with the code it was derived from and say "agree"
    assert res["status"] == "unavailable" and res["reason"] == eq["reason"] and res["trials"] == 0


def test_math_constants_and_power_syntax(kernel, repo: Path):
    (repo / "analysis" / "consts.py").write_text(
        "import math\n\n"
        'def circle(r):\n    """Doc.\n\n    Math:\n        A = pi * r^2 + 0*E\n    """\n    return [math.pi * r * r][0]\n'
    )
    eq = by_name(kernel, "analysis/consts.py")["circle"]
    assert eq["status"] == "ok" and eq["source_expr"] == "pi*r**2"
    assert kernel.call("eq.check", {"path": "analysis/consts.py", "function": "circle"})["status"] == "agree"


@pytest.mark.parametrize("bad", ["../x.py", "/etc/passwd"])
def test_eq_paths_stay_in_root(kernel, bad):
    from analyst_kernel.errors import RpcError

    with pytest.raises(RpcError) as exc:
        kernel.call("eq.extract", {"path": bad})
    assert exc.value.code == 1002
