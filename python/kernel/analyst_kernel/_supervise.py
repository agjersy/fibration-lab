"""Detached run supervisor.

Started by the kernel with ``start_new_session=True`` so it becomes the process-group
leader of the run. It launches the (sandbox-wrapped) script with stdout/stderr on the
run's log files, waits for it, and writes the final ``status`` line and ``meta.json``
in the run directory. Because it is independent of the kernel, a run keeps its
bookkeeping even when the kernel restarts (PROTOCOL.md 1.4 step 7).

Usage:
    python -m analyst_kernel._supervise --run-dir DIR [--timeout S]
        [--rlimit-as BYTES] [--rlimit-nofile N] [--exit-fd FD] -- ARGV...

``--exit-fd`` is the write end of a pipe whose read end the launching kernel holds. The
supervisor never writes to it: it closes it the moment the result is on disk (``status``
and ``meta.json``), and the kernel sees end-of-file at once instead of finding out on its
next poll. If the supervisor dies first the kernel sees the same end-of-file and marks
the run failed. The descriptor is never inherited by the script.

Everything printed goes to stderr.log; this process never touches the kernel's stdio.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

from .paths import utc_now_iso
from .runstate import limits_preexec, proc_start_token, read_status_line, transition, write_atomic

GRACE_S = 5.0


def read_status(run_dir: Path) -> str:
    return read_status_line(run_dir)


def finish(run_dir: Path, status_line: str, exit_code: int | None) -> None:
    """Record the final state unless the kernel already recorded a cancel."""
    status_line = transition(run_dir, status_line)
    meta_path = run_dir / "meta.json"
    try:
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        meta = {}
    meta["status"] = status_line.split(":", 1)[0]
    meta["exit_code"] = exit_code if status_line.startswith("done:") else None
    meta["finished_at"] = utc_now_iso()
    meta.setdefault("pid", os.getpid())
    meta.setdefault("pid_start", proc_start_token(os.getpid(), allow_zombie=True))
    write_atomic(meta_path, json.dumps(meta, indent=2) + "\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="analyst_kernel._supervise")
    parser.add_argument("--run-dir", required=True)
    parser.add_argument("--timeout", type=float, default=None)
    parser.add_argument("--rlimit-as", type=int, default=None)
    parser.add_argument("--rlimit-nofile", type=int, default=None)
    parser.add_argument("--exit-fd", type=int, default=None)
    parser.add_argument("cmd", nargs=argparse.REMAINDER)
    ns = parser.parse_args(argv)
    cmd = ns.cmd[1:] if ns.cmd and ns.cmd[0] == "--" else ns.cmd
    run_dir = Path(ns.run_dir)
    timeout = ns.timeout if ns.timeout and ns.timeout > 0 else None
    exit_fd = ns.exit_fd
    if exit_fd is not None:
        try:
            os.set_inheritable(exit_fd, False)
        except OSError:
            exit_fd = None

    # The supervisor must outlive a SIGTERM aimed at the group so it can record the result.
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, signal.SIG_IGN)

    def child_setup() -> None:
        # The script must not inherit the ignored signals, or cancel would not reach it.
        for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
            signal.signal(sig, signal.SIG_DFL)
        if ns.rlimit_as or ns.rlimit_nofile:
            limits_preexec(ns.rlimit_as, ns.rlimit_nofile)()

    stdout = open(run_dir / "stdout.log", "ab", buffering=0)  # noqa: SIM115
    stderr = open(run_dir / "stderr.log", "ab", buffering=0)  # noqa: SIM115
    try:
        proc = subprocess.Popen(cmd, stdout=stdout, stderr=stderr, stdin=subprocess.DEVNULL, preexec_fn=child_setup)
    except OSError as exc:
        stderr.write(f"analyst: could not start {cmd[0] if cmd else '?'}: {exc}\n".encode())
        finish(run_dir, "failed", None)
        _signal_exit(exit_fd)
        return 1
    _write_child_pid(run_dir, proc.pid)
    try:
        rc = proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        stderr.write(f"analyst: run exceeded its {timeout:g}s time limit and was stopped\n".encode())
        finish(run_dir, "timeout", None)
        # The exit pipe stays open until the group is gone (the final SIGKILL closes it), so
        # the kernel flushes the logs only after the last process has stopped writing.
        _kill_group(proc)
        return 0
    if rc >= 0:
        finish(run_dir, f"done:{rc}", rc)
    else:
        finish(run_dir, "failed", rc)
    _signal_exit(exit_fd)
    return 0


def _signal_exit(fd: int | None) -> None:
    """Tell the kernel the result is recorded: closing the pipe is the whole message."""
    if fd is None:
        return
    try:
        os.close(fd)
    except OSError:
        pass


def _write_child_pid(run_dir: Path, pid: int) -> None:
    try:
        write_atomic(run_dir / "child.pid", f"{pid}\n")
    except OSError:
        pass


def _kill_group(proc: subprocess.Popen) -> None:
    """Terminate the run: SIGTERM to the group, a grace period, then SIGKILL to the whole group.

    This process leads the group and ignores SIGTERM. The final SIGKILL cannot be ignored
    and takes this process with it too, which is fine: every record has been written by
    then. Killing only the direct child would leave workers that ignore SIGTERM running.
    """
    try:
        os.killpg(os.getpgid(0), signal.SIGTERM)
    except OSError:
        pass
    deadline = time.monotonic() + GRACE_S
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            break
        time.sleep(0.1)
    try:
        proc.kill()
    except OSError:
        pass
    try:
        os.killpg(os.getpgid(0), signal.SIGKILL)
    except OSError:
        pass
    try:  # only reached when the group kill was not permitted
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass


if __name__ == "__main__":
    sys.exit(main())
