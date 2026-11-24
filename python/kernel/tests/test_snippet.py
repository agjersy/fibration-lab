from __future__ import annotations

import resource
import sys

import pytest

from analyst_kernel.errors import TIMEOUT, RpcError
from analyst_kernel.kernel import Kernel


def test_snippet_persistent_state(kernel):
    r = kernel.call("exec.snippet", {"code": "x = 21\nx * 2"})
    assert r == {"ok": True, "stdout": "", "stderr": "", "value_repr": "42"}
    r = kernel.call("exec.snippet", {"code": "print('hi'); x + 1"})
    assert r["ok"] and r["stdout"] == "hi\n" and r["value_repr"] == "22"
    r = kernel.call("exec.snippet", {"code": "import sys; sys.stderr.write('warn'); None"})
    assert r["ok"] and r["stderr"] == "warn" and r["value_repr"] is None
    r = kernel.call("exec.snippet", {"code": "y = 1"})
    assert r["ok"] and r["value_repr"] is None


def test_snippet_error(kernel):
    r = kernel.call("exec.snippet", {"code": "1/0"})
    assert r["ok"] is False and "ZeroDivisionError" in r["stderr"] and r["value_repr"] is None
    r = kernel.call("exec.snippet", {"code": "def (:"})
    assert r["ok"] is False and "SyntaxError" in r["stderr"]


def test_snippet_runs_in_session_python_with_env(kernel, repo):
    r = kernel.call("exec.snippet", {"code": "import os, sys; (os.getcwd(), os.environ['MPLBACKEND'], sys.executable)"})
    assert r["ok"]
    assert str(repo.resolve()) in r["value_repr"] and "module://analyst_mpl" in r["value_repr"]
    assert kernel.python in r["value_repr"]
    assert kernel.call("exec.snippet", {"code": "from analysis.models import double_exp; double_exp(0, 1, 1, 2, 1)"})["value_repr"].endswith("3.0)")


def test_snippet_timeout_restarts_repl(kernel):
    kernel.call("exec.snippet", {"code": "z = 5"})
    with pytest.raises(RpcError) as exc:
        kernel.call("exec.snippet", {"code": "import time; time.sleep(30)", "timeout_s": 0.5})
    assert exc.value.code == TIMEOUT
    r = kernel.call("exec.snippet", {"code": "'z' in dir()"})
    assert r["ok"] and r["value_repr"] == "False"  # fresh REPL after the kill


def test_snippet_has_no_address_space_limit_by_default(kernel):
    # B10: RLIMIT_AS is opt-in; the open-files limit still applies
    code = "import resource; (resource.getrlimit(resource.RLIMIT_AS)[0], resource.getrlimit(resource.RLIMIT_NOFILE)[0])"
    r = kernel.call("exec.snippet", {"code": code})
    as_limit, nofile = eval(r["value_repr"])
    assert r["ok"] and as_limit == resource.getrlimit(resource.RLIMIT_AS)[0] and nofile <= 4096


def test_snippet_limits_from_project_config(repo, tmp_path):
    with (repo / ".analyst" / "project.toml").open("a") as fh:
        fh.write("\n[limits]\naddress_space_gb = 3\nopen_files = 512\n")
    k = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="limits")
    try:
        code = "import resource; (resource.getrlimit(resource.RLIMIT_AS)[0], resource.getrlimit(resource.RLIMIT_NOFILE)[0])"
        r = k.call("exec.snippet", {"code": code})
        # macOS accepts the address-space limit and does not apply it (ARCHITECTURE 5.6).
        as_limit = resource.getrlimit(resource.RLIMIT_AS)[0] if sys.platform == "darwin" else 3 * 1024**3
        assert r["ok"] and eval(r["value_repr"]) == (as_limit, 512)
    finally:
        k.close()


def test_snippet_params(kernel):
    with pytest.raises(RpcError):
        kernel.call("exec.snippet", {})


def test_snippet_repl_and_equation_worker_are_separate_processes(kernel):
    """Agent-run snippet code must not share a process with the equation checker: tampering with
    SymPy in a snippet cannot make a wrong equation report "agree"."""
    before = kernel.call("eq.extract", {"path": "analysis/models.py"})["equations"]
    r = kernel.call("exec.snippet", {"code": (
        "import os, sympy\n"
        "sympy.simplify = lambda *a, **k: sympy.Integer(0)\n"
        "sympy.Expr.equals = lambda *a, **k: True\n"
        "os.getpid()"
    )})
    assert r["ok"], r["stderr"]
    repl_pid = int(r["value_repr"])
    eq_pid = kernel._eq._proc.pid
    assert repl_pid == kernel._snippets._proc.pid
    assert repl_pid != eq_pid and repl_pid != __import__("os").getpid()
    # the patch is real in the REPL ...
    assert kernel.call("exec.snippet", {"code": "sympy.simplify(sympy.Symbol('x') + 1)"})["value_repr"] == "0"
    # ... and invisible to the equation analysis and the check
    after = kernel.call("eq.extract", {"path": "analysis/models.py"})["equations"]
    assert [(e["function"], e["status"], e.get("latex")) for e in after] == [
        (e["function"], e["status"], e.get("latex")) for e in before
    ]
    assert kernel._eq._proc.pid == eq_pid
    assert kernel.call("eq.check", {"path": "analysis/models.py", "function": "damped_cos"})["status"] == "mismatch"
    assert kernel.call("eq.check", {"path": "analysis/models.py", "function": "double_exp"})["status"] == "agree"
