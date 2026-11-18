"""The sandbox wrappers executed for real: bwrap on Linux, sandbox-exec on macOS.

Everything else in the suite checks the generated argv or profile as text. These tests run
a command under the wrapper and observe what it can and cannot do. They skip when the
tool is unavailable (bwrap missing or user namespaces restricted; neither on other systems).
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest
from conftest import Notes, wait_until

from analyst_kernel import sandbox
from analyst_kernel.kernel import Kernel
from analyst_kernel.sandbox import SandboxSpec, interpreter_dirs, wrap


def real_kind() -> str | None:
    if sys.platform == "linux" and sandbox.bwrap_available():
        return "bwrap"
    if sys.platform == "darwin" and sandbox.sandbox_exec_available():
        return "sandbox-exec"
    return None


KIND = real_kind()
needs_sandbox = pytest.mark.skipif(KIND is None, reason="no working bwrap (Linux) or sandbox-exec (macOS) on this machine")
needs_bwrap = pytest.mark.skipif(KIND != "bwrap", reason="bwrap is not usable on this machine")

PROBE = r"""
import json, os, sys, tempfile
out_dir, root, secret, run_dir = sys.argv[1:5]
res = {"executable": sys.executable, "cwd": os.getcwd(), "tmpdir": tempfile.gettempdir()}
with open(os.path.join(out_dir, "ok.txt"), "w") as fh:
    fh.write("written inside the output directory")
res["out_write"] = True
fd, tmp_file = tempfile.mkstemp()
os.close(fd)
res["tmp_file_in_run_dir"] = os.path.realpath(tmp_file).startswith(os.path.realpath(run_dir))
def attempt(fn):
    try:
        fn()
    except OSError as exc:
        return "denied: %s" % exc.strerror
    return "ALLOWED"
res["source_write"] = attempt(lambda: open(os.path.join(root, "scripts", "hacked.py"), "w").close())
res["source_append"] = attempt(lambda: open(os.path.join(root, "scripts", "tool.py"), "a").close())
res["source_read"] = open(os.path.join(root, "scripts", "tool.py")).read().strip()
res["secret_read"] = attempt(lambda: open(secret).read())
print("PROBE " + json.dumps(res))
"""


def make_repo(tmp_path: Path) -> tuple[Path, Path]:
    """A repo with a uv-style venv: ``.venv/bin/python`` links to a base interpreter elsewhere."""
    root = tmp_path / "repo"
    (root / "scripts").mkdir(parents=True)
    (root / "scripts" / "tool.py").write_text("print('tool')\n")
    made = subprocess.run(
        [sys.executable, "-m", "venv", "--without-pip", str(root / ".venv")], capture_output=True, text=True
    )
    assert made.returncode == 0, made.stderr
    python = root / ".venv" / "bin" / "python"
    assert python.exists()
    return root, python


def describe(cmd, proc, spec) -> str:
    """Everything needed to debug a sandbox failure from a CI log."""
    wrapper = cmd.profile if cmd.profile else " ".join(cmd.argv[: cmd.argv.index("--") + 1] if "--" in cmd.argv else cmd.argv)
    return (
        f"\nsandbox kind: {cmd.kind}\nexit code: {proc.returncode}\n"
        f"--- stdout ---\n{proc.stdout}\n--- stderr ---\n{proc.stderr}\n"
        f"--- interpreter ---\npython: {spec.python} -> {os.path.realpath(spec.python)}\n"
        f"interpreter dirs: {[str(p) for p in interpreter_dirs(spec.python, spec.home)]}\n"
        f"home: {sandbox._home()} -> {os.path.realpath(sandbox._home())}\n"
        f"--- wrapper ({'profile' if cmd.profile else 'argv'}) ---\n{wrapper}\n"
    )


@needs_sandbox
def test_trivial_command_runs_under_the_real_wrapper(tmp_path: Path):
    root, python = make_repo(tmp_path)
    run_dir = tmp_path / "runs" / "r1"
    (run_dir / "tmp").mkdir(parents=True)
    out_dir = root / "out" / "tool" / "r1"
    out_dir.mkdir(parents=True)
    home = sandbox._home()
    secret = home / f".analyst-sandbox-test-{os.getpid()}"
    try:
        secret.write_text("secret in the home directory\n")
        have_secret = True
    except OSError:
        have_secret = False  # read-only home: the check below is skipped
    spec = SandboxSpec(
        root=root, venv=root / ".venv", python=python, run_dir=run_dir, out_dir=out_dir,
        tooling_dir=sandbox.tooling_dir(),
    )
    cmd = wrap(KIND, spec, [str(python), "-c", PROBE, str(out_dir), str(root), str(secret), str(run_dir)])
    env = {
        "HOME": str(home), "PATH": os.environ.get("PATH", "/usr/bin:/bin"), "TMPDIR": str(run_dir / "tmp"),
        "LANG": os.environ.get("LANG", "C.UTF-8"), "PYTHONDONTWRITEBYTECODE": "1",
    }
    try:
        proc = subprocess.run(cmd.argv, cwd=str(root), env=env, capture_output=True, text=True, timeout=120)
    finally:
        if have_secret:
            secret.unlink()
    info = describe(cmd, proc, spec)
    assert proc.returncode == 0, "the interpreter did not run under the sandbox" + info
    lines = [ln for ln in proc.stdout.splitlines() if ln.startswith("PROBE ")]
    assert lines, "no probe result" + info
    res = json.loads(lines[-1][6:])
    assert os.path.realpath(res["cwd"]) == os.path.realpath(root), info
    assert res["out_write"] is True and (out_dir / "ok.txt").read_text().startswith("written"), info
    assert res["tmp_file_in_run_dir"] is True, "TMPDIR must be the per-run directory" + info
    assert res["source_read"] == "print('tool')", info
    assert res["source_write"].startswith("denied"), "the source tree is writable inside the sandbox" + info
    assert res["source_append"].startswith("denied"), "the source tree is writable inside the sandbox" + info
    assert not (root / "scripts" / "hacked.py").exists(), info
    if have_secret:
        assert res["secret_read"].startswith("denied"), "the home directory is readable inside the sandbox" + info


@needs_sandbox
def test_network_is_denied_under_the_real_wrapper(tmp_path: Path):
    root, python = make_repo(tmp_path)
    run_dir = tmp_path / "runs" / "r1"
    out_dir = root / "out" / "tool" / "r1"
    for d in (run_dir, out_dir):
        d.mkdir(parents=True)
    code = (
        "import socket\n"
        "s = socket.socket()\ns.settimeout(3)\n"
        "try:\n    s.connect(('1.1.1.1', 53)); print('NET open')\n"
        "except OSError as exc:\n    print('NET denied', type(exc).__name__)\n"
    )
    spec = SandboxSpec(root=root, venv=root / ".venv", python=python, run_dir=run_dir, out_dir=out_dir)
    cmd = wrap(KIND, spec, [str(python), "-c", code])
    env = {"HOME": str(sandbox._home()), "PATH": os.environ.get("PATH", "/usr/bin:/bin")}
    proc = subprocess.run(cmd.argv, cwd=str(root), env=env, capture_output=True, text=True, timeout=60)
    assert proc.returncode == 0 and "NET denied" in proc.stdout, describe(cmd, proc, spec)


def test_interpreter_prefix_that_exposes_home_is_refused(tmp_path: Path, caplog):
    home = tmp_path / "home"
    # ~/bin/python: the "prefix" would be the home directory itself
    (home / "bin").mkdir(parents=True)
    (home / "bin" / "python3").write_text("")
    venv = tmp_path / "repo" / ".venv"
    (venv / "bin").mkdir(parents=True)
    (venv / "bin" / "python").symlink_to(home / "bin" / "python3")
    # pyvenv.cfg naming a directory above home
    (venv / "pyvenv.cfg").write_text(f"home = {tmp_path / 'bin'}\n")
    (tmp_path / "bin").mkdir()
    with caplog.at_level("WARNING"):
        dirs = interpreter_dirs(venv / "bin" / "python", home)
    assert home not in dirs and tmp_path not in dirs and dirs == []
    assert "would expose the home directory" in caplog.text
    spec = SandboxSpec(
        root=tmp_path / "repo", venv=venv, run_dir=tmp_path / "run", out_dir=tmp_path / "repo" / "out",
        home=home, python=venv / "bin" / "python",
    )
    argv = sandbox.bwrap_argv(spec, ["/py"])
    binds = [argv[i + 1] for i, a in enumerate(argv) if a in ("--ro-bind", "--bind")]
    assert str(home) not in binds and str(tmp_path) not in binds
    assert f'(allow file-read* (subpath "{home}"))' not in sandbox.sandbox_exec_profile(spec)
    # an ordinary uv layout below home is still granted
    base = home / ".local" / "share" / "uv" / "python" / "cpython-3.12"
    (base / "bin").mkdir(parents=True)
    (base / "bin" / "python3.12").write_text("")
    (venv / "bin" / "python").unlink()
    (venv / "bin" / "python").symlink_to(base / "bin" / "python3.12")
    (venv / "pyvenv.cfg").write_text(f"home = {base / 'bin'}\n")
    assert interpreter_dirs(venv / "bin" / "python", home) == [base]


def test_profile_keeps_the_source_tree_read_only_under_temp_dirs(tmp_path: Path):
    root = tmp_path / "repo"
    out = root / "out" / "s" / "r1"
    out.mkdir(parents=True)
    run_dir = tmp_path / "runs" / "r1"
    run_dir.mkdir(parents=True)
    spec = SandboxSpec(root=root, venv=None, run_dir=run_dir, out_dir=out, home=tmp_path / "home")
    lines = sandbox.sandbox_exec_profile(spec).splitlines()
    tmp_allow = lines.index('(allow file-write* (subpath "/private/tmp"))')
    deny = lines.index(f'(deny file-write* (subpath "{root}"))')
    allow_out = lines.index(f'(allow file-read* file-write* (subpath "{out}"))')
    # later rules win: temp dirs writable, then the repo denied, then the output directory granted
    assert tmp_allow < deny < allow_out
    assert lines.index(f'(allow file-read* file-write* (subpath "{run_dir}"))') > deny


# ---- the kernel under bwrap -----------------------------------------------------------------


@pytest.fixture
def bwrap_kernel(repo: Path, tmp_path: Path, notes: Notes):
    k = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="bwrap", notify=notes)
    yield k
    k.close()


def write_script(repo: Path, name: str, body: str) -> str:
    (repo / "scripts" / name).write_text(body)
    return f"scripts/{name}"


def processes_with(token: str) -> list[int]:
    found = []
    for entry in os.listdir("/proc"):
        if entry.isdigit() and int(entry) != os.getpid():
            try:
                with open(f"/proc/{entry}/cmdline", "rb") as fh:
                    if token.encode() in fh.read():
                        found.append(int(entry))
            except OSError:
                pass
    return found


@needs_bwrap
def test_bwrap_run_end_to_end(bwrap_kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path):
    run_id = bwrap_kernel.call("run.start", {"script": "scripts/fit_decay.py", "args": ["--verbose"]})["run_id"]
    done = notes.wait_for("run.done", run_id)[0]
    err = (tmp_path / "runs" / run_id / "stderr.log").read_text()
    assert done["status"] == "done" and done["exit_code"] == 0, err
    assert bwrap_kernel.call("run.status", {"run_id": run_id})["sandbox"] == "bwrap"
    arts = {a["artifact"]["path"].rsplit("/", 1)[-1]: a["artifact"] for a in notes.of("run.artifact", run_id)}
    assert set(arts) == {"decay-fit.png", "fit.json"}, err
    # the shared Matplotlib cache is writable inside the sandbox: the font cache landed there
    assert any((tmp_path / "runs" / "_mplconfig").iterdir()), err
    assert arts["decay-fit.png"]["sidecar"]["run_id"] == run_id


@needs_bwrap
def test_bwrap_run_cannot_write_the_source_tree_or_reach_the_network(bwrap_kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path):
    script = write_script(
        repo, "escape.py",
        "import os, socket, tempfile\n"
        "root = os.environ['ANALYST_ROOT']\n"
        "for target in (os.path.join(root, 'analysis', 'models.py'), os.path.join(root, 'new.py')):\n"
        "    try:\n        open(target, 'a').close(); print('WROTE', target)\n"
        "    except OSError as exc:\n        print('denied', os.path.basename(target), exc.errno)\n"
        "open(os.path.expanduser('~/analyst-bwrap-test-marker'), 'w').close()\n"  # lands in the throwaway tmpfs
        "print('home', sorted(os.listdir(os.path.expanduser('~'))))\n"
        "fd, p = tempfile.mkstemp(); os.close(fd); print('tmp', os.path.dirname(p) == os.environ['TMPDIR'])\n"
        "open(os.path.join(os.environ['ANALYST_OUT_DIR'], 'fine.txt'), 'w').write('ok')\n"
        "try:\n    socket.create_connection(('1.1.1.1', 53), timeout=3); print('NET open')\n"
        "except OSError:\n    print('NET denied')\n",
    )
    before = (repo / "analysis" / "models.py").read_bytes()
    res = bwrap_kernel.call("run.start", {"script": script})
    notes.wait_for("run.done", res["run_id"])
    out = "".join(p["chunk"] for p in notes.of("run.stdout", res["run_id"]))
    err = (tmp_path / "runs" / res["run_id"] / "stderr.log").read_text()
    assert not (sandbox._home() / "analyst-bwrap-test-marker").exists(), "the real home directory was written"
    assert "home ['analyst-bwrap-test-marker']" in out, "the home directory is not hidden: " + out
    assert "WROTE" not in out and out.count("denied") == 3 and "NET denied" in out and "tmp True" in out, out + err
    assert (repo / "analysis" / "models.py").read_bytes() == before and not (repo / "new.py").exists()
    assert (repo / res["out_dir"] / "fine.txt").read_text() == "ok"


@needs_bwrap
def test_bwrap_cancel(bwrap_kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path):
    token = f"cancel-token-{os.getpid()}-{time.monotonic_ns()}"
    script = write_script(
        repo, "sleepy.py",
        "import subprocess, sys, time\n"
        "subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(300)', sys.argv[1]])\n"
        "print('start', flush=True)\ntime.sleep(300)\n",
    )
    run_id = bwrap_kernel.call("run.start", {"script": script, "args": [token]})["run_id"]
    notes.wait_for("run.stdout", run_id)
    assert len(processes_with(token)) >= 3  # supervisor, bwrap, script, worker
    t0 = time.monotonic()
    assert bwrap_kernel.call("run.cancel", {"run_id": run_id}) == {"ok": True}
    done = notes.wait_for("run.done", run_id, timeout=30)[0]
    assert done["status"] == "cancelled" and time.monotonic() - t0 < 10
    wait_until(lambda: processes_with(token) == [], timeout=15)
    assert bwrap_kernel.call("run.status", {"run_id": run_id})["status"] == "cancelled"
    assert bwrap_kernel.call("run.cancel", {"run_id": run_id}) == {"ok": False}


@needs_bwrap
def test_bwrap_timeout(bwrap_kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path):
    token = f"timeout-token-{os.getpid()}-{time.monotonic_ns()}"
    script = write_script(
        repo, "forever.py",
        "import subprocess, sys, time\n"
        "code = 'import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(300)'\n"
        "subprocess.Popen([sys.executable, '-c', code, sys.argv[1]])\n"
        "print('go', flush=True)\ntime.sleep(300)\n",
    )
    run_id = bwrap_kernel.call("run.start", {"script": script, "args": [token], "timeout_s": 2})["run_id"]
    done = notes.wait_for("run.done", run_id, timeout=40)[0]
    assert done["status"] == "timeout"
    assert (tmp_path / "runs" / run_id / "status").read_text().strip() == "timeout"
    wait_until(lambda: processes_with(token) == [], timeout=15)
    assert bwrap_kernel.call("run.status", {"run_id": run_id})["status"] == "timeout"


@needs_bwrap
def test_bwrap_snippet_and_eq_check(bwrap_kernel: Kernel, repo: Path):
    # the REPL's private descriptors pass through bwrap
    r = bwrap_kernel.call("exec.snippet", {"code": "import os; print('hi'); os.write(1, b'raw\\n'); 6 * 7"})
    assert r == {"ok": True, "stdout": "hi\nraw\n", "stderr": "", "value_repr": "42"}
    r = bwrap_kernel.call("exec.snippet", {"code": (
        "import os\n"
        "try:\n    open(os.path.join(os.environ['ANALYST_ROOT'], 'analysis', 'models.py'), 'a').close(); res = 'WROTE'\n"
        "except OSError:\n    res = 'denied'\n"
        "res"
    )})
    assert r["ok"] and r["value_repr"] == "'denied'"
    res = bwrap_kernel.call("eq.check", {"path": "analysis/models.py", "function": "double_exp"})
    assert res["status"] == "agree" and res["trials"] == 32
    assert bwrap_kernel.call("eq.check", {"path": "analysis/models.py", "function": "damped_cos"})["status"] == "mismatch"


def test_bwrap_helper_outlives_the_thread_that_started_it(bwrap_kernel: Kernel):
    """`bwrap --die-with-parent` kills a child when the thread that forked it exits; the helper is
    often started by a short-lived thread (the prewarm, a request), and must survive it."""
    import threading

    started = threading.Thread(target=lambda: bwrap_kernel.call("exec.snippet", {"code": "kept = 7"}))
    started.start()
    started.join()
    time.sleep(0.5)
    assert bwrap_kernel.call("exec.snippet", {"code": "kept"})["value_repr"] == "7"
    eqs = bwrap_kernel.call("eq.extract", {"path": "analysis/models.py"})["equations"]
    assert any(e["tier"] == "derived" for e in eqs)
    assert not any("helper" in (e.get("reason") or "") for e in eqs)
    assert bwrap_kernel.call("exec.snippet", {"code": "kept"})["value_repr"] == "7"
