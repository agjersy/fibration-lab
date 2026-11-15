"""Regression tests for the November 2026 review: G9, B7, B10, K2, K4, K5, K9, K10 and the status-file race."""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest
from conftest import Notes, wait_until
from test_e2e import KernelProc

from analyst_kernel import __main__ as kernel_main
from analyst_kernel.config import ConfigError, load_config, project_data_dirs, validate_out_dir
from analyst_kernel.errors import INVALID_PARAMS, RpcError
from analyst_kernel.kernel import Kernel
from analyst_kernel.runs import CHUNK_BYTES, _ArtifactScanner, utf8_cut
from analyst_kernel.runstate import proc_start_token, read_status_line, same_process, transition, write_atomic


def write_script(repo: Path, name: str, body: str) -> str:
    (repo / "scripts" / name).write_text(body)
    return f"scripts/{name}"


def _alive(pid: int) -> bool:
    return proc_start_token(pid) is not None


# ---- G9: out_dir and data_dirs --------------------------------------------------------


@pytest.mark.parametrize("bad", ["../x", "/abs/out", "~/out", ".", "out/../../x", ".git/out", ".analyst"])
def test_g9_out_dir_must_stay_inside_the_root(repo: Path, tmp_path: Path, bad: str):
    with pytest.raises(ConfigError):
        validate_out_dir(repo, bad)
    with pytest.raises(ConfigError):
        Kernel(sys.executable, repo, tmp_path / "runs", out_dir=bad, sandbox="limits")
    assert not (tmp_path / "x").exists()


def test_g9_out_dir_from_project_toml_is_rejected_with_exit_code_2(repo: Path, tmp_path: Path):
    (repo / ".analyst" / "project.toml").write_text('[project]\nout_dir = "../escaped"\n')
    argv = ["--python", sys.executable, "--root", str(repo), "--runs-dir", str(tmp_path / "runs"), "--sandbox", "limits"]
    assert kernel_main.main(argv) == 2
    assert not (tmp_path / "escaped").exists() and not (tmp_path / "runs").exists()
    # a symlink that leaves the repo is refused too
    (repo / ".analyst" / "project.toml").write_text('[project]\nout_dir = "results"\n')
    outside = tmp_path / "outside"
    outside.mkdir()
    (repo / "results").symlink_to(outside)
    assert kernel_main.main(argv) == 2
    # a relative --data-dir is a usage error as well
    (repo / "results").unlink()
    assert kernel_main.main([*argv, "--data-dir", "relative/dir"]) == 2
    assert validate_out_dir(repo, "results/") == "results" and validate_out_dir(repo, "a/b") == "a/b"


def test_g9_project_data_dirs_are_confined_and_data_dir_arg_is_honoured(repo: Path, tmp_path: Path, caplog):
    external = tmp_path / "shared"
    external.mkdir()
    (repo / "link").symlink_to(external)
    (repo / ".analyst" / "project.toml").write_text(
        '[project]\ndata_dirs = ["data", "~/.ssh", "/etc", "../shared", "link", "data/sub"]\n'
        '[targets.lab]\ndata_dirs = ["/data/shared", "data"]\n'
    )
    dirs, warnings = project_data_dirs(repo, load_config(repo).data_dirs)
    assert dirs == [repo.resolve() / "data", repo.resolve() / "data" / "sub"]
    assert len(warnings) == 4 and all("outside the repo root" in w for w in warnings)
    with caplog.at_level("WARNING"):
        k = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="limits", data_dirs=[str(external)])
    try:
        assert "'~/.ssh'" in caplog.text and "'/etc'" in caplog.text
        assert k.data_dirs == [repo.resolve() / "data", repo.resolve() / "data" / "sub", external]
        assert k.config.targets["lab"]["data_dirs"] == [str(repo.resolve() / "data")]
        spec = k.runs.sandbox_spec(tmp_path / "runs" / "r", repo / "out" / "s" / "r", False)
        assert spec.data_dirs == k.data_dirs
        ro = [str(p) for p in spec.read_only_dirs()]
        assert str(external) in ro and "/etc" not in ro and not any(".ssh" in p for p in ro)
        assert k.call("kernel.hello", {"protocol_version": 1})["sandbox"] == "limits"
    finally:
        k.close()
    parsed = kernel_main.build_parser().parse_args(
        ["--python", "p", "--root", "r", "--runs-dir", "d", "--data-dir", "/a", "--data-dir", "/b"]
    )
    assert parsed.data_dir == ["/a", "/b"]


# ---- B10: limits ------------------------------------------------------------------------


def test_b10_limits_config(tmp_path: Path):
    assert load_config(tmp_path).rlimit_as() is None and load_config(tmp_path).open_files == 4096
    (tmp_path / ".analyst").mkdir()
    (tmp_path / ".analyst" / "project.toml").write_text("[limits]\naddress_space_gb = 16\nopen_files = 1024\n")
    cfg = load_config(tmp_path)
    assert cfg.rlimit_as() == 16 * 1024**3 and cfg.open_files == 1024
    assert cfg.to_json()["limits"] == {"address_space_gb": 16.0, "open_files": 1024}
    (tmp_path / ".analyst" / "project.toml").write_text('[limits]\naddress_space_gb = 0\nopen_files = "many"\n')
    cfg = load_config(tmp_path)
    assert cfg.rlimit_as() is None and cfg.open_files == 4096


def test_b10_runs_have_no_address_space_limit_and_can_map_large_files(kernel: Kernel, repo: Path, notes: Notes):
    script = write_script(
        repo, "memmap.py",
        "import mmap, os, resource\n"
        "print('as', resource.getrlimit(resource.RLIMIT_AS)[0] == resource.RLIM_INFINITY)\n"
        "p = os.path.join(os.environ['ANALYST_OUT_DIR'], 'big.bin')\n"
        "with open(p, 'wb') as fh:\n    fh.truncate(10 * 1024**3)\n"  # sparse: 10 GiB of address space, no disk
        "with open(p, 'rb') as fh:\n    m = mmap.mmap(fh.fileno(), 0, access=mmap.ACCESS_READ)\n    print('mapped', len(m))\n    m.close()\n"
        "os.remove(p)\n",
    )
    if resource_as_limited():
        pytest.skip("this environment already runs under an address-space limit")
    run_id = kernel.call("run.start", {"script": script})["run_id"]
    done = notes.wait_for("run.done", run_id)[0]
    out = "".join(p["chunk"] for p in notes.of("run.stdout", run_id))
    assert done["exit_code"] == 0 and "as True" in out and f"mapped {10 * 1024**3}" in out


def resource_as_limited() -> bool:
    import resource

    return resource.getrlimit(resource.RLIMIT_AS)[0] != resource.RLIM_INFINITY


# ---- B7: chunked output ---------------------------------------------------------------


def test_utf8_cut():
    assert utf8_cut(b"abc") == 3 and utf8_cut(b"") == 0
    euro = "€".encode()  # 3 bytes
    assert utf8_cut(b"ab" + euro) == 5
    assert utf8_cut(b"ab" + euro[:1]) == 2 and utf8_cut(b"ab" + euro[:2]) == 2
    assert utf8_cut(b"ab" + "😀".encode()[:3]) == 2
    assert utf8_cut(b"ab\xff") == 3  # invalid byte: passed through, replaced on decode
    assert utf8_cut(b"\x80\x80\x80\x80\x80") == 5


def test_b7_output_is_chunked_and_decoded_incrementally(kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path):
    script = write_script(
        repo, "loud.py",
        "import sys\n"
        "line = ('€ü' * 40 + '\\n').encode()\n"
        "sys.stdout.buffer.write(line * 3000)\n"  # ~600 KB of multi-byte text
        "sys.stdout.buffer.write(b'tail \\xff\\xe2\\x82')\n"  # invalid byte, then a sequence that never completes
        "sys.stdout.buffer.flush()\n"
        "sys.stderr.write('e' * 200000)\n",
    )
    run_id = kernel.call("run.start", {"script": script})["run_id"]
    notes.wait_for("run.done", run_id)
    raw = (tmp_path / "runs" / run_id / "stdout.log").read_bytes()
    chunks = notes.of("run.stdout", run_id)
    assert len(chunks) >= len(raw) // CHUNK_BYTES
    ends = [c["offset"] for c in chunks[1:]] + [len(raw)]
    assert chunks[0]["offset"] == 0
    for c, end in zip(chunks, ends):
        # each chunk is exactly the bytes [offset, next offset) of the log, at most 64 KiB of them
        assert 0 < end - c["offset"] <= CHUNK_BYTES
        assert c["chunk"] == raw[c["offset"] : end].decode("utf-8", errors="replace")
    text = "".join(c["chunk"] for c in chunks)
    assert text == raw.decode("utf-8", errors="replace")
    assert text.endswith("tail \ufffd\ufffd"), "invalid UTF-8 at the end of output must be flushed"
    assert "\ufffd" not in text[:-2], "a multi-byte character was split across chunks"
    err = notes.of("run.stderr", run_id)
    assert max(len(c["chunk"]) for c in err) <= CHUNK_BYTES and sum(len(c["chunk"]) for c in err) == 200000

    # replay after a restart uses the same chunk size
    notes2 = Notes()
    k2 = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="limits", notify=notes2)
    try:
        k2.call("run.attach", {"run_id": run_id, "stdout_offset": 7})
        replay = notes2.of("run.stdout", run_id)
        assert len(replay) >= len(raw) // CHUNK_BYTES and replay[0]["offset"] == 7
        assert all(len(c["chunk"].encode("utf-8", "replace")) <= CHUNK_BYTES + 8 for c in replay)
        assert "".join(c["chunk"] for c in replay) == raw[7:].decode("utf-8", errors="replace")
    finally:
        k2.close()


# ---- K2: artifacts written late ---------------------------------------------------------


def test_k2_scanner_reannounces_changed_files_and_skips_empty_ones(tmp_path: Path):
    root = tmp_path / "root"
    run_dir = tmp_path / "run"
    out = root / "out" / "s" / "r"
    out.mkdir(parents=True)
    run_dir.mkdir()
    (run_dir / "artifacts.jsonl").touch()
    sc = _ArtifactScanner(root, run_dir, out)
    early = out / "result.txt"
    early.touch()  # opened early, written late
    for _ in range(4):
        assert sc.scan() == [], "a zero-byte file must not be announced while the run is live"
    grow = out / "grow.csv"
    grow.write_text("a,b\n")
    sc.scan()
    first = sc.scan()
    assert [(a["path"], a["size"]) for a in first] == [("out/s/r/grow.csv", 4)]
    for a in first:
        sc.record(a)
    grow.write_text("a,b\n1,2\n3,4\n")
    early.write_text("final content")
    assert sc.scan() == [], "an announced file is not re-examined mid-run; a new non-empty file waits one poll"
    final = sc.scan(final=True)
    assert {a["path"]: a["size"] for a in final} == {"out/s/r/grow.csv": 12, "out/s/r/result.txt": 13}
    sc.record_all(final)
    recorded = [json.loads(line) for line in (run_dir / "artifacts.jsonl").read_text().splitlines()]
    assert {a["path"]: a["size"] for a in recorded} == {"out/s/r/grow.csv": 12, "out/s/r/result.txt": 13}
    assert len(recorded) == 2, "artifacts.jsonl holds one final record per path"
    assert sc.scan(final=True) == []
    # an empty file is still listed once the run is over
    (out / "empty.log").touch()
    assert [(a["path"], a["size"]) for a in sc.scan(final=True)] == [("out/s/r/empty.log", 0)]


def test_k2_file_opened_early_and_written_late_has_its_final_size(kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path):
    script = write_script(
        repo, "late.py",
        "import os, time\nout = os.environ['ANALYST_OUT_DIR']\n"
        "fh = open(os.path.join(out, 'late.txt'), 'w')\n"
        "with open(os.path.join(out, 'steps.txt'), 'w') as g:\n    g.write('x' * 10)\n"
        "time.sleep(1.5)\n"
        "with open(os.path.join(out, 'steps.txt'), 'a') as g:\n    g.write('y' * 90)\n"
        "fh.write('z' * 500)\nfh.close()\n",
    )
    run_id = kernel.call("run.start", {"script": script})["run_id"]
    notes.wait_for("run.done", run_id)
    arts = [a["artifact"] for a in notes.of("run.artifact", run_id)]
    late = [a["size"] for a in arts if a["path"].endswith("late.txt")]
    assert late == [500], "the file used to be recorded at 0 bytes"
    assert [a["size"] for a in arts if a["path"].endswith("steps.txt")] == [10, 100]
    recorded = [json.loads(ln) for ln in (tmp_path / "runs" / run_id / "artifacts.jsonl").read_text().splitlines()]
    assert {a["path"].rsplit("/", 1)[-1]: a["size"] for a in recorded} == {"late.txt": 500, "steps.txt": 100}
    assert len(recorded) == 2


# ---- K4: run identity -----------------------------------------------------------------


def test_proc_start_token_identifies_a_process():
    me = proc_start_token(os.getpid())
    assert me and me == proc_start_token(os.getpid())
    assert same_process(os.getpid(), me) and not same_process(os.getpid(), "proc:1")
    child = subprocess.Popen([sys.executable, "-c", "pass"])
    child_token = proc_start_token(child.pid, allow_zombie=True)
    time.sleep(0.5)  # exited, not yet reaped: a zombie is not a running supervisor
    assert child_token and not same_process(child.pid, child_token)
    child.wait()
    assert proc_start_token(child.pid) is None and not same_process(child.pid, child_token)


def _fake_running_run(runs_dir: Path, repo: Path, run_id: str, pid: int, token: str | None) -> Path:
    run_dir = runs_dir / run_id
    run_dir.mkdir(parents=True)
    out_rel = f"out/quick/{run_id}"
    (repo / out_rel).mkdir(parents=True)
    meta = {
        "run_id": run_id, "script": "scripts/quick.py", "args": [], "status": "running", "exit_code": None,
        "started_at": "2026-01-01T00:00:00.000Z", "finished_at": None, "out_dir": out_rel, "sandbox": "limits",
        "tree_hash": None, "pid": pid, "pid_start": token, "host": "h",
    }
    (run_dir / "meta.json").write_text(json.dumps(meta))
    (run_dir / "status").write_text("running\n")
    for name in ("stdout.log", "stderr.log", "artifacts.jsonl"):
        (run_dir / name).touch()
    return run_dir


def test_k4_reused_pid_is_not_trusted_or_signalled(kernel: Kernel, repo: Path, tmp_path: Path):
    # A process that is alive but is not the run's supervisor: same pid, different start time.
    bystander = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"], start_new_session=True)
    try:
        run_dir = _fake_running_run(tmp_path / "runs", repo, "20260101-000000-aaaaaa", bystander.pid, "proc:12345")
        assert kernel.call("run.cancel", {"run_id": "20260101-000000-aaaaaa"}) == {"ok": False}
        time.sleep(0.3)
        assert bystander.poll() is None, "cancel signalled a process group that only shares the pid"
        meta = kernel.call("run.status", {"run_id": "20260101-000000-aaaaaa"})
        assert meta["status"] == "failed" and meta["finished_at"]
        assert read_status_line(run_dir) == "failed"
        assert "supervisor" in (run_dir / "stderr.log").read_text()

        # the same holds for runs.list and attach
        _fake_running_run(tmp_path / "runs", repo, "20260101-000001-bbbbbb", bystander.pid, "proc:12345")
        listed = {r["run_id"]: r["status"] for r in kernel.call("runs.list")["runs"]}
        assert listed["20260101-000001-bbbbbb"] == "failed"
        _fake_running_run(tmp_path / "runs", repo, "20260101-000002-cccccc", bystander.pid, "proc:12345")
        assert kernel.call("run.attach", {"run_id": "20260101-000002-cccccc"})["status"] == "failed"
        assert bystander.poll() is None
    finally:
        bystander.kill()
        bystander.wait()


def test_k4_run_whose_supervisor_died_is_failed_after_restart(kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path):
    script = write_script(repo, "sleepy.py", "import time\nprint('start', flush=True)\ntime.sleep(60)\n")
    run_id = kernel.call("run.start", {"script": script})["run_id"]
    notes.wait_for("run.stdout", run_id)
    run_dir = tmp_path / "runs" / run_id
    meta = json.loads((run_dir / "meta.json").read_text())
    assert meta["pid_start"] == proc_start_token(meta["pid"]), "meta.json records the supervisor's start time"
    kernel.runs.shutdown()  # the kernel "restarts": no watcher is left for this run
    kernel.runs._watchers.clear()
    os.killpg(meta["pid"], signal.SIGKILL)  # supervisor and script die without recording a result
    wait_until(lambda: not _alive(meta["pid"]), timeout=10)
    k2 = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="limits")
    try:
        assert read_status_line(run_dir) == "running"
        assert k2.call("runs.list")["runs"][0]["status"] == "failed"
        assert k2.call("run.status", {"run_id": run_id})["status"] == "failed"
        assert k2.call("run.cancel", {"run_id": run_id}) == {"ok": False}
    finally:
        k2.close()


# ---- status-file writes -----------------------------------------------------------------


def test_status_writes_do_not_race(tmp_path: Path):
    target = tmp_path / "status"
    errors: list[BaseException] = []

    def hammer(tag: str) -> None:
        try:
            for i in range(300):
                write_atomic(target, f"{tag}-{i}\n")
        except BaseException as exc:
            errors.append(exc)

    threads = [threading.Thread(target=hammer, args=(f"t{n}",)) for n in range(6)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    assert errors == [], "writers sharing one temporary name used to fail with FileNotFoundError"
    assert target.read_text().strip().split("-")[1] == "299"
    assert [p.name for p in tmp_path.iterdir()] == ["status"], "temporary files are cleaned up"

    # the first terminal state wins, whoever writes it
    write_atomic(target, "running\n")
    assert transition(tmp_path, "cancelled") == "cancelled"
    assert transition(tmp_path, "done:0") == "cancelled" and read_status_line(tmp_path) == "cancelled"


# ---- K10: timeout, attach, validation -----------------------------------------------------


def test_k10_timeout_kills_workers_that_ignore_sigterm(kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path):
    script = write_script(
        repo, "stubborn.py",
        "import os, subprocess, sys, time\n"
        "code = 'import signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); print(\"up\", flush=True); time.sleep(120)'\n"
        "w = subprocess.Popen([sys.executable, '-c', code], stdout=subprocess.PIPE)\n"
        "w.stdout.readline()\n"
        "open(os.path.join(os.environ['ANALYST_OUT_DIR'], 'worker.pid'), 'w').write(str(w.pid))\n"
        "print('go', flush=True)\ntime.sleep(120)\n",
    )
    res = kernel.call("run.start", {"script": script, "timeout_s": 2})
    run_id = res["run_id"]
    done = notes.wait_for("run.done", run_id, timeout=40)[0]
    assert done["status"] == "timeout"
    worker = int((repo / res["out_dir"] / "worker.pid").read_text())
    child = int((tmp_path / "runs" / run_id / "child.pid").read_text())
    wait_until(lambda: not _alive(worker) and not _alive(child), timeout=15)
    assert "time limit" in (tmp_path / "runs" / run_id / "stderr.log").read_text()
    assert kernel.call("run.status", {"run_id": run_id})["finished_at"]


def test_cancel_is_prompt(kernel: Kernel, repo: Path, notes: Notes):
    # the script used to inherit the supervisor's ignored SIGTERM, so cancel took the 5 s kill fallback
    script = write_script(repo, "sleepy.py", "import time\nprint('start', flush=True)\ntime.sleep(60)\n")
    run_id = kernel.call("run.start", {"script": script})["run_id"]
    notes.wait_for("run.stdout", run_id)
    t0 = time.monotonic()
    assert kernel.call("run.cancel", {"run_id": run_id}) == {"ok": True}
    assert notes.wait_for("run.done", run_id, timeout=20)[0]["status"] == "cancelled"
    assert time.monotonic() - t0 < 4


@pytest.mark.parametrize("timeout", [0, -1, 0.0])
def test_k10_timeout_must_be_positive(kernel: Kernel, timeout):
    with pytest.raises(RpcError) as exc:
        kernel.call("run.start", {"script": "scripts/fit_decay.py", "timeout_s": timeout})
    assert exc.value.code == INVALID_PARAMS
    assert kernel.call("runs.list")["runs"] == []


def test_k10_attach_to_a_live_run_delivers_each_byte_once(kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path):
    script = write_script(
        repo, "ticker.py",
        "import time\nfor i in range(400):\n    print('line %04d' % i, flush=True)\n    time.sleep(0.005)\n",
    )
    run_id = kernel.call("run.start", {"script": script})["run_id"]
    notes.wait_for("run.stdout", run_id)
    notes2 = Notes()
    k2 = Kernel(sys.executable, repo, tmp_path / "runs", sandbox="limits", notify=notes2)
    k2.runs.poll_s = 0.01  # a fast watcher makes the old race (watcher ahead of the replay) near certain
    try:
        assert k2.call("run.attach", {"run_id": run_id})["status"] == "running"
        notes2.wait_for("run.done", run_id, timeout=60)
        position = 0
        for c in notes2.of("run.stdout", run_id):
            assert c["offset"] == position, f"output delivered twice or out of order at byte {c['offset']}"
            position += len(c["chunk"].encode())
        assert position == (tmp_path / "runs" / run_id / "stdout.log").stat().st_size == 400 * 10
        assert notes2.items[-1][0] == "run.done"
    finally:
        k2.close()
    notes.wait_for("run.done", run_id, timeout=60)


def test_run_tmpdir_is_per_run(kernel: Kernel, repo: Path, notes: Notes, tmp_path: Path, monkeypatch):
    monkeypatch.setenv("TMPDIR", "/somewhere/else")
    script = write_script(
        repo, "tmp.py",
        "import os, tempfile\nfd, p = tempfile.mkstemp()\nos.close(fd)\nprint(os.environ['TMPDIR'], os.path.dirname(p))\n",
    )
    run_id = kernel.call("run.start", {"script": script})["run_id"]
    notes.wait_for("run.done", run_id)
    expected = str(tmp_path / "runs" / run_id / "tmp")
    assert "".join(p["chunk"] for p in notes.of("run.stdout", run_id)).split() == [expected, expected]


# ---- K5 / K9: snippets -----------------------------------------------------------------


def test_k5_fd_level_output_is_captured_and_state_survives(kernel: Kernel):
    assert kernel.call("exec.snippet", {"code": "state = 41"})["ok"]
    r = kernel.call("exec.snippet", {"code": (
        "import os, subprocess, sys\n"
        "print('before')\n"
        "subprocess.run([sys.executable, '-c', \"import sys; print('from child'); sys.stderr.write('child err')\"])\n"
        "os.write(1, b'raw fd write\\n')\n"
        "os.system('echo from shell')\n"
        "state + 1\n"
    )})
    assert r["ok"] and r["value_repr"] == "42"
    assert r["stdout"] == "before\nfrom child\nraw fd write\nfrom shell\n" and r["stderr"] == "child err"
    # the REPL is still the same process with the same state, and old output does not leak forward
    r = kernel.call("exec.snippet", {"code": "print('next'); state"})
    assert r == {"ok": True, "stdout": "next\n", "stderr": "", "value_repr": "41"}
    # reading stdin does not consume the protocol
    r = kernel.call("exec.snippet", {"code": "import sys; sys.stdin.read()"})
    assert r["ok"] and r["value_repr"] == "''"
    assert kernel.call("exec.snippet", {"code": "state"})["value_repr"] == "41"


def test_k5_snippet_output_is_capped(kernel: Kernel):
    r = kernel.call("exec.snippet", {"code": "import sys; print('a' * 300000); sys.stderr.write('b' * 300000); 'c' * 300000"})
    assert r["ok"]
    for key in ("stdout", "stderr", "value_repr"):
        assert len(r[key]) <= 64 * 1024, key
    assert r["stdout"].startswith("aaaa") and r["stdout"].endswith("[output truncated]\n")
    assert kernel.call("exec.snippet", {"code": "1 + 1"}) == {"ok": True, "stdout": "", "stderr": "", "value_repr": "2"}


def test_k5_repl_death_is_reported_and_recovers(kernel: Kernel):
    kernel.call("exec.snippet", {"code": "x = 1"})
    r = kernel.call("exec.snippet", {"code": "import os; os._exit(3)"})
    assert r["ok"] is False and "exited" in r["stderr"]
    assert kernel.call("exec.snippet", {"code": "'x' in dir()"})["value_repr"] == "False"


def test_k9_snippets_do_not_starve_other_requests_and_shutdown_kills_them(repo: Path, tmp_path: Path):
    k = KernelProc(repo, tmp_path / "runs")
    try:
        k.request("kernel.hello", {"protocol_version": 1})
        assert k.request("exec.snippet", {"code": "import os; os.getpid()"})["result"]["ok"]
        # one snippet that runs for a long time and eleven queued behind it: more than the request pool holds
        for i in range(12):
            msg = {"jsonrpc": "2.0", "id": f"s{i}", "method": "exec.snippet",
                   "params": {"code": "import time; time.sleep(120)", "timeout_s": 300}}
            k.proc.stdin.write(json.dumps(msg) + "\n")
        k.proc.stdin.flush()
        time.sleep(0.5)
        t0 = time.monotonic()
        assert "runs" in k.request("runs.list", timeout=10)["result"]
        assert k.request("scripts.index", timeout=10)["result"]["scripts"]
        assert k.request("contract.check", {"paths": ["scripts/fit_decay.py"]}, timeout=10)["result"]["diagnostics"] == []
        assert time.monotonic() - t0 < 8, "other requests were blocked behind queued snippets"
        t0 = time.monotonic()
        assert k.request("kernel.shutdown", timeout=10)["result"] == {"ok": True}
        assert k.proc.wait(timeout=10) == 0
        assert time.monotonic() - t0 < 8, "shutdown waited for a running snippet"
    finally:
        if k.proc.poll() is None:
            k.proc.kill()
