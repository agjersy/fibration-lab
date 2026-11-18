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
        assert r["ok"] and eval(r["value_repr"]) == (3 * 1024**3, 512)
    finally:
        k.close()


def test_snippet_params(kernel):
    with pytest.raises(RpcError):
        kernel.call("exec.snippet", {})


def test_snippets_and_equations_share_one_helper_and_a_timeout_restarts_it(kernel):
    """The REPL and the equation analysis are one process; killing it on a budget overrun
    restarts both: the REPL state is gone, and equations are served by the new process."""
    kernel.call("exec.snippet", {"code": "z = 5"})
    pid = kernel._helper._proc.pid
    eqs = kernel.call("eq.extract", {"path": "analysis/models.py"})["equations"]
    assert eqs and kernel._helper._proc.pid == pid
    assert kernel.call("exec.snippet", {"code": "z"})["value_repr"] == "5"
    with pytest.raises(RpcError) as exc:
        kernel.call("exec.snippet", {"code": "import time; time.sleep(30)", "timeout_s": 0.5})
    assert exc.value.code == TIMEOUT
    again = kernel.call("eq.extract", {"path": "analysis/models.py"})["equations"]
    assert [e["status"] for e in again] == [e["status"] for e in eqs]
    assert kernel._helper._proc.pid != pid
    assert kernel.call("exec.snippet", {"code": "'z' in dir()"})["value_repr"] == "False"
