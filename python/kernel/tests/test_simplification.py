"""Regression tests for the simplification round (docs/DESIGN-REVIEW.md, docs/IPC.md section 8).

Run completion is signalled instead of polled, output streams with low latency, the
Matplotlib cache is shared, figures are saved in one format, the equation worker is
prewarmed, and ``env.packages`` is cached.
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import statistics
import sys
import threading
import time
from pathlib import Path

import pytest
from conftest import Notes, wait_until

from fibration_kernel import envinfo
from fibration_kernel.config import ProjectConfig, config_from_dict, load_config
from fibration_kernel.kernel import Kernel
from fibration_kernel.paths import parse_iso
from fibration_kernel.sandbox import SandboxSpec, bwrap_argv, sandbox_exec_profile


class TimedNotes(Notes):
    """Notification sink that also records when each notification arrived (wall clock)."""

    def __init__(self) -> None:
        super().__init__()
        self.times: list[float] = []

    def __call__(self, method: str, params: dict) -> None:
        with self._cv:
            self.times.append(time.time())
        super().__call__(method, params)

    def arrival(self, method: str, run_id: str) -> float:
        with self._cv:
            for (m, p), t in zip(self.items, self.times, strict=True):
                if m == method and p.get("run_id") == run_id:
                    return t
        raise AssertionError(f"no {method} for {run_id}")


@pytest.fixture
def timed(repo: Path, tmp_path: Path):
    notes = TimedNotes()
    k = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="limits", notify=notes, prewarm=False)
    yield k, notes
    k.close()


def write_script(repo: Path, name: str, body: str) -> str:
    (repo / "scripts" / name).write_text(body)
    return f"scripts/{name}"


def exit_to_done(kernel: Kernel, notes: TimedNotes, run_id: str) -> float:
    """Seconds between the supervisor recording the script's exit and ``run.done`` arriving."""
    notes.wait_for("run.done", run_id)
    finished = parse_iso(kernel.call("run.status", {"run_id": run_id})["finished_at"])
    assert finished is not None
    return notes.arrival("run.done", run_id) - finished


# ---- 1. completion is signalled, output streams promptly -----------------------------------


def test_run_done_is_signalled_by_the_supervisor_not_found_by_polling(timed, repo: Path):
    kernel, notes = timed
    # With every polling interval at 30 s, only the supervisor's exit signal can end the run in time.
    kernel.runs.poll_s = 30.0
    kernel.runs.tail_min_s = 30.0
    script = write_script(repo, "noop.py", "print('bye')\n")
    run_id = kernel.call("run.start", {"script": script})["run_id"]
    done = notes.wait_for("run.done", run_id, timeout=20)[0]
    assert done["status"] == "done" and done["exit_code"] == 0
    assert exit_to_done(kernel, notes, run_id) < 1.0  # measured: about 5 ms
    # the final flush ran at exit: output is complete and precedes run.done
    assert "".join(p["chunk"] for p in notes.of("run.stdout", run_id)) == "bye\n"
    assert notes.items[-1][0] == "run.done"


def test_final_artifact_scan_runs_once_at_exit_without_a_stability_wait(timed, repo: Path):
    kernel, notes = timed
    kernel.runs.poll_s = 30.0  # no live scan ever happens: everything comes from the scan at exit
    kernel.runs.tail_min_s = 30.0
    run_id = kernel.call("run.start", {"script": "scripts/fit_decay.py"})["run_id"]
    notes.wait_for("run.done", run_id, timeout=60)
    assert exit_to_done(kernel, notes, run_id) < 1.0  # measured: about 10 ms
    arts = {a["artifact"]["path"].rsplit("/", 1)[-1]: a["artifact"] for a in notes.of("run.artifact", run_id)}
    assert set(arts) == {"decay-fit.png", "fit.json"}
    assert arts["decay-fit.png"]["sidecar"]["title"] == "Decay fit" and arts["decay-fit.png"]["size"] > 0
    methods = [m for m, p in notes.items if p.get("run_id") == run_id]
    assert methods[-1] == "run.done" and methods.count("run.done") == 1
    assert "done 50" in "".join(p["chunk"] for p in notes.of("run.stdout", run_id))


def test_run_done_latency_with_default_settings(timed, repo: Path):
    kernel, notes = timed
    script = write_script(repo, "noop.py", "pass\n")
    latencies = []
    for _ in range(5):
        run_id = kernel.call("run.start", {"script": script})["run_id"]
        latencies.append(exit_to_done(kernel, notes, run_id))
    # Target is about 60 ms; the old 250 ms poll plus its 50 ms settle loop had a median near 170 ms.
    assert statistics.median(latencies) < 0.12, latencies


def test_output_streams_with_low_latency_while_it_flows(timed, repo: Path):
    kernel, notes = timed
    script = write_script(
        repo, "ticker.py",
        "import time\nfor i in range(60):\n    print('%.6f' % time.time(), flush=True)\n    time.sleep(0.01)\n",
    )
    run_id = kernel.call("run.start", {"script": script})["run_id"]
    notes.wait_for("run.done", run_id)
    delays = []
    with notes._cv:
        pairs = [(p, t) for (m, p), t in zip(notes.items, notes.times, strict=True) if m == "run.stdout" and p["run_id"] == run_id]
    for params, arrived in pairs:
        for line in params["chunk"].split():
            delays.append(arrived - float(line))
    assert len(delays) == 60
    assert len(pairs) >= 8  # delivered as it is produced, not in a few 250 ms lumps
    # Tailing every 30 ms while output flows: a line waits about 15 ms on average (old poll: 125 ms).
    assert statistics.median(delays) < 0.08, statistics.median(delays)


def test_a_dead_supervisor_is_noticed_through_the_pipe_and_the_run_fails(timed, repo: Path, tmp_path: Path):
    kernel, notes = timed
    kernel.runs.poll_s = 30.0
    kernel.runs.tail_min_s = 30.0
    script = write_script(repo, "sleepy.py", "import time\nprint('up', flush=True)\ntime.sleep(120)\n")
    run_id = kernel.call("run.start", {"script": script})["run_id"]
    run_dir = tmp_path / "runs" / run_id
    wait_until(lambda: (run_dir / "child.pid").exists() and (run_dir / "stdout.log").stat().st_size > 0)
    child = int((run_dir / "child.pid").read_text())
    supervisor = json.loads((run_dir / "meta.json").read_text())["pid"]
    try:
        os.kill(supervisor, signal.SIGKILL)  # the supervisor only, before it could record a result
        done = notes.wait_for("run.done", run_id, timeout=15)[0]
        assert done["status"] == "failed" and done["exit_code"] is None
        assert "supervisor process is gone" in (run_dir / "stderr.log").read_text()
        assert "".join(p["chunk"] for p in notes.of("run.stdout", run_id)) == "up\n"
    finally:
        try:
            os.kill(child, signal.SIGKILL)
        except OSError:
            pass


def test_the_exit_pipe_is_not_inherited_by_the_script(timed, repo: Path):
    kernel, notes = timed
    # A script that leaves a background process behind must not keep the run "live": only the
    # supervisor holds the pipe, so run.done still follows the script's own exit at once.
    script = write_script(
        repo, "leaver.py",
        "import subprocess, sys\n"
        "subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(20)'], close_fds=False,\n"
        "                 stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)\n"
        "print(sorted(int(n) for n in __import__('os').listdir('/proc/self/fd')) if sys.platform == 'linux' else [])\n",
    )
    kernel.runs.poll_s = 30.0
    kernel.runs.tail_min_s = 30.0
    t0 = time.monotonic()
    run_id = kernel.call("run.start", {"script": script})["run_id"]
    done = notes.wait_for("run.done", run_id, timeout=15)[0]
    assert done["status"] == "done" and time.monotonic() - t0 < 10
    if sys.platform == "linux":
        fds = json.loads("".join(p["chunk"] for p in notes.of("run.stdout", run_id)))
        assert [fd for fd in fds if fd > 3] == [], fds  # 0-2 plus the listing's own directory fd
    kernel.call("run.cancel", {"run_id": run_id})


def test_watchers_release_their_descriptors(timed, repo: Path):
    if not os.path.isdir("/proc/self/fd"):
        pytest.skip("needs /proc")
    kernel, notes = timed
    script = write_script(repo, "noop.py", "pass\n")
    first = kernel.call("run.start", {"script": script})["run_id"]
    notes.wait_for("run.done", first)
    wait_until(lambda: not kernel.runs._watchers)
    before = len(os.listdir("/proc/self/fd"))
    for _ in range(5):
        run_id = kernel.call("run.start", {"script": script})["run_id"]
        notes.wait_for("run.done", run_id)
    wait_until(lambda: not kernel.runs._watchers and len(os.listdir("/proc/self/fd")) <= before, timeout=10)


# ---- 2. shared Matplotlib cache ---------------------------------------------------------------


def test_mplconfigdir_is_one_directory_per_runs_dir(kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path):
    shared = tmp_path / "runs" / "_mplconfig"
    assert shared.is_dir() and kernel.runs.mpl_config_dir == shared
    script = write_script(repo, "mplenv.py", "import os\nprint(os.environ['MPLCONFIGDIR'])\n")
    seen = []
    for _ in range(2):
        run_id = kernel.call("run.start", {"script": script})["run_id"]
        notes.wait_for("run.done", run_id)
        seen.append("".join(p["chunk"] for p in notes.of("run.stdout", run_id)).strip())
        assert sorted(p.name for p in (tmp_path / "runs" / run_id).iterdir() if p.is_dir()) == ["tmp"]
    assert seen == [str(shared), str(shared)]
    # the REPL and eq.check use the same directory, and it is writable in their sandbox too
    for name in ("repl", "eqcheck"):
        assert kernel._tool_env(name)["MPLCONFIGDIR"] == str(shared)
        assert shared in kernel._tool_spec(name).writable_dirs()
    assert shared in kernel.runs.sandbox_spec(tmp_path / "runs" / "x", repo / "out" / "x").writable_dirs()
    # it is not a run
    assert all(r["run_id"] != "_mplconfig" for r in kernel.call("runs.list")["runs"])
    snippet = kernel.call("exec.snippet", {"code": "import os; os.environ['MPLCONFIGDIR']"})
    assert snippet["ok"] and snippet["value_repr"] == repr(str(shared))


def test_cache_dirs_are_writable_in_every_sandbox_kind(tmp_path: Path):
    cache = tmp_path / "runs" / "_mplconfig"
    cache.mkdir(parents=True)
    spec = SandboxSpec(
        root=tmp_path / "repo", venv=None, run_dir=tmp_path / "runs" / "r1", out_dir=tmp_path / "repo" / "out",
        cache_dirs=[cache], home=tmp_path / "home",
    )
    assert cache in spec.writable_dirs()
    argv = bwrap_argv(spec, ["true"])
    assert any(argv[i : i + 3] == ["--bind", str(cache), str(cache)] for i in range(len(argv)))
    assert f'(allow file-read* file-write* (subpath "{cache}"))' in sandbox_exec_profile(spec)


def test_matplotlib_font_cache_is_built_once_and_reused(kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path):
    shared = tmp_path / "runs" / "_mplconfig"
    run_id = kernel.call("run.start", {"script": "scripts/fit_decay.py"})["run_id"]
    assert notes.wait_for("run.done", run_id)[0]["exit_code"] == 0
    caches = sorted(shared.glob("fontlist-*.json"))
    assert caches, sorted(p.name for p in shared.iterdir())
    stamp = caches[0].stat().st_mtime_ns
    run_id = kernel.call("run.start", {"script": "scripts/fit_decay.py"})["run_id"]
    assert notes.wait_for("run.done", run_id)[0]["exit_code"] == 0
    assert caches[0].stat().st_mtime_ns == stamp  # the second run read the cache, it did not rebuild it


# ---- 3. one figure format ---------------------------------------------------------------------


def test_figure_formats_config():
    assert ProjectConfig().figure_formats == ["png"]
    assert config_from_dict({}).figure_formats == ["png"]
    assert config_from_dict({"figures": {"formats": ["svg", "PNG", ".pdf", "svg", "jpg", 3]}}).figure_formats == ["svg", "png", "pdf"]
    for bad in ("png", [], ["jpg"], None, {"png": True}):
        assert config_from_dict({"figures": {"formats": bad}}).figure_formats == ["png"]
    assert config_from_dict({"figures": "png"}).figure_formats == ["png"]
    assert config_from_dict({"figures": {"formats": ["pdf"]}}).to_json()["figures"] == {"formats": ["pdf"]}


def test_figure_formats_reach_the_run_from_project_toml(repo: Path, tmp_path: Path, notes: Notes):
    toml = repo / ".fibration" / "project.toml"
    toml.write_text(toml.read_text() + '\n[figures]\nformats = ["png", "svg"]\n')
    assert load_config(repo).figure_formats == ["png", "svg"]
    k = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="limits", notify=notes, prewarm=False)
    try:
        # the kernel's value wins over one passed with the run
        run_id = k.call("run.start", {"script": "scripts/fit_decay.py", "env": {"FIBRATION_FIGURE_FORMATS": "pdf"}})["run_id"]
        notes.wait_for("run.done", run_id)
        arts = {a["artifact"]["path"].rsplit("/", 1)[-1]: a["artifact"] for a in notes.of("run.artifact", run_id)}
        assert set(arts) == {"decay-fit.png", "decay-fit.svg", "fit.json"}
        assert arts["decay-fit.svg"]["sidecar"]["title"] == "Decay fit"
    finally:
        k.close()


# ---- 4. equation worker prewarm -----------------------------------------------------------------


def _worker_pid(kernel: Kernel) -> int | None:
    eq = kernel._eq
    proc = eq._proc if eq is not None else None
    return proc.pid if proc is not None and proc.poll() is None else None


def test_hello_prewarms_the_equation_worker_without_waiting_for_it(repo: Path, tmp_path: Path):
    k = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="limits")
    try:
        assert k._eq is None
        t0 = time.monotonic()
        hello = k.call("kernel.hello", {"protocol_version": 1})
        t_hello = time.monotonic() - t0
        assert hello["protocol_version"] == 1
        assert t_hello < 0.5, t_hello  # the SymPy import (seconds) happens in the background
        # other requests are served while the worker starts
        t0 = time.monotonic()
        assert k.call("scripts.index")["scripts"]
        assert time.monotonic() - t0 < 1.5
        wait_until(lambda: _worker_pid(k) is not None, timeout=60)
        with k._eq._lock:  # the prewarm holds this until the worker has said it is ready
            pid = _worker_pid(k)
        assert pid is not None
        t0 = time.monotonic()
        eqs = k.call("eq.extract", {"path": "analysis/models.py"})["equations"]
        warm = time.monotonic() - t0
        assert eqs and _worker_pid(k) == pid  # served by the prewarmed worker, no second start
        assert warm < 1.5, warm
        k.call("kernel.hello", {"protocol_version": 1})  # a second hello starts nothing new
        wait_until(lambda: not any(t.name == "eq-prewarm" for t in threading.enumerate()), timeout=30)
        assert _worker_pid(k) == pid and k._prewarmed
    finally:
        k.close()
    assert _pid_gone(pid)


def test_prewarm_can_be_disabled_and_close_during_prewarm_leaves_no_worker(repo: Path, tmp_path: Path):
    k = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="limits", prewarm=False)
    try:
        k.call("kernel.hello", {"protocol_version": 1})
        time.sleep(0.2)
        assert k._eq is None
    finally:
        k.close()

    k = Kernel(sys.executable, repo, tmp_path / "runs2", sandbox="limits")
    k.call("kernel.hello", {"protocol_version": 1})
    wait_until(lambda: k._eq is not None and k._eq._proc is not None, timeout=10, interval=0.005)
    worker = k._eq
    pid = worker._proc.pid
    k.close()  # while the worker is still importing SymPy
    wait_until(lambda: not any(t.name == "eq-prewarm" for t in threading.enumerate()), timeout=20)
    assert worker._proc is None and _pid_gone(pid)
    assert k._eq is None


def _pid_gone(pid: int) -> bool:
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        except OSError:
            return False
        time.sleep(0.05)
    return False


# ---- 6. env.packages cache ------------------------------------------------------------------------


def test_env_packages_is_cached_until_its_inputs_change(kernel: Kernel, repo: Path, monkeypatch):
    calls = []
    real = envinfo.env_packages

    def counting(python, root):
        calls.append(1)
        return real(python, root)

    monkeypatch.setattr(envinfo, "env_packages", counting)
    first = kernel.call("env.packages")["packages"]
    assert len(calls) == 1 and any(p["name"].lower() == "numpy" for p in first)
    t0 = time.monotonic()
    for _ in range(20):
        again = kernel.call("env.packages")["packages"]
    assert time.monotonic() - t0 < 0.5  # repeat calls do not spawn uv (0.9 s each before)
    assert len(calls) == 1 and again == first
    again[0]["name"] = "mutated"  # callers get copies
    assert kernel.call("env.packages")["packages"] == first

    (repo / "uv.lock").write_text('version = 1\n\n[[package]]\nname = "numpy"\nversion = "2.0.0"\n')
    locked = {p["name"].lower(): p["locked"] for p in kernel.call("env.packages")["packages"]}
    assert len(calls) == 2 and locked["numpy"] is True
    (repo / "pyproject.toml").write_text('[project]\nname = "x"\nversion = "0"\n')
    kernel.call("env.packages")
    assert len(calls) == 3
    (repo / "uv.lock").unlink()
    assert all(p["locked"] is False for p in kernel.call("env.packages")["packages"])
    assert len(calls) == 4
    kernel.call("env.packages")
    assert len(calls) == 4


def test_packages_stamp_follows_site_packages(tmp_path: Path):
    venv = tmp_path / "venv"
    site = venv / "lib" / "python3.12" / "site-packages"
    site.mkdir(parents=True)
    (venv / "bin").mkdir()
    python = venv / "bin" / "python"
    shutil.copy(sys.executable, python) if not os.path.islink(sys.executable) else python.symlink_to(sys.executable)
    assert envinfo.site_packages_dirs(str(python)) == [site]
    root = tmp_path / "repo"
    root.mkdir()
    before = envinfo.packages_stamp(str(python), root)
    assert envinfo.packages_stamp(str(python), root) == before
    (site / "newpkg-1.0.dist-info").mkdir()  # what an install does
    os.utime(site, ns=(time.time_ns(), site.stat().st_mtime_ns + 1_000_000))
    after = envinfo.packages_stamp(str(python), root)
    assert after != before
    (root / "uv.lock").write_text("version = 1\n")
    assert envinfo.packages_stamp(str(python), root) != after
