"""Run-directory state shared by the kernel and the detached supervisor. Standard library only.

Two things live here because both sides must agree on them exactly:

- the ``status`` file, which is written by the supervisor (final result) and by the kernel
  (cancel, failure detection). Writes use unique temporary names and transitions out of
  ``running`` are serialized with an advisory lock, so the first terminal state wins;
- process identity. A bare pid is not an identity (pids are reused), so a run records the
  supervisor's start time next to its pid and both are checked before the pid is trusted
  or signalled.
"""

from __future__ import annotations

import contextlib
import os
import subprocess
import threading
from collections.abc import Callable
from pathlib import Path

try:  # pragma: no cover - always present on the supported platforms
    import fcntl
except ImportError:  # pragma: no cover - windows
    fcntl = None  # type: ignore[assignment]

TERMINAL_PREFIXES = ("done:", "cancelled", "timeout", "failed")


def limits_preexec(rlimit_as: int | None, rlimit_nofile: int | None) -> Callable[[], None]:
    """Return a preexec_fn that applies RLIMIT_AS and RLIMIT_NOFILE in the child."""

    def _apply() -> None:
        try:
            import resource
        except ImportError:  # pragma: no cover - windows
            return
        if rlimit_as:
            try:
                _, hard = resource.getrlimit(resource.RLIMIT_AS)
                soft = rlimit_as if hard == resource.RLIM_INFINITY else min(rlimit_as, hard)
                resource.setrlimit(resource.RLIMIT_AS, (soft, hard))
            except (ValueError, OSError):
                pass
        if rlimit_nofile:
            try:
                _, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
                soft = rlimit_nofile if hard == resource.RLIM_INFINITY else min(rlimit_nofile, hard)
                resource.setrlimit(resource.RLIMIT_NOFILE, (soft, hard))
            except (ValueError, OSError):
                pass

    return _apply


def write_atomic(path: Path, text: str) -> None:
    """Replace ``path`` atomically. The temporary name is unique per process and thread."""
    tmp = path.with_name(f"{path.name}.{os.getpid()}.{threading.get_ident()}.tmp")
    try:
        tmp.write_text(text, encoding="utf-8")
        os.replace(tmp, path)
    finally:
        with contextlib.suppress(OSError):
            tmp.unlink()


def read_status_line(run_dir: Path) -> str:
    try:
        return (run_dir / "status").read_text(encoding="utf-8").strip()
    except OSError:
        return ""


@contextlib.contextmanager
def status_lock(run_dir: Path):
    """Advisory lock around read-modify-write of the status file."""
    fh = None
    try:
        try:
            fh = open(run_dir / "status.lock", "a+b")  # noqa: SIM115
            if fcntl is not None:
                fcntl.flock(fh.fileno(), fcntl.LOCK_EX)
        except OSError:
            pass
        yield
    finally:
        if fh is not None:
            with contextlib.suppress(OSError):
                fh.close()  # closing releases the lock


def transition(run_dir: Path, new_line: str) -> str:
    """Move the run out of ``running`` to ``new_line``; returns the line now in force.

    If a terminal state is already recorded it is kept, so a cancel recorded by the kernel
    is not overwritten by the supervisor's ``done:<code>`` and the other way round.
    """
    with status_lock(run_dir):
        current = read_status_line(run_dir)
        if current and current != "running":
            return current
        write_atomic(run_dir / "status", new_line + "\n")
        return new_line


# ---- process identity ----------------------------------------------------------


def pid_alive(pid: int | None) -> bool:
    if not pid:
        return False
    try:
        os.kill(int(pid), 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except (OSError, ValueError, OverflowError):
        return False
    return True


def proc_start_token(pid: int | None, allow_zombie: bool = False) -> str | None:
    """An opaque token for the start time of ``pid``; None when the process does not exist.

    Linux reads field 22 of ``/proc/<pid>/stat`` (start time in clock ticks since boot);
    elsewhere ``ps -o lstart=`` is used. A zombie counts as gone unless ``allow_zombie``.
    """
    if not pid:
        return None
    if os.path.exists("/proc/self/stat"):
        try:
            with open(f"/proc/{int(pid)}/stat", "rb") as fh:
                data = fh.read().decode("ascii", "replace")
            rest = data[data.rindex(")") + 2 :].split()
            if rest[0] == "Z" and not allow_zombie:
                return None
            return "proc:" + rest[19]
        except (OSError, ValueError, IndexError):
            return None
    try:
        proc = subprocess.run(
            ["ps", "-o", "stat=,lstart=", "-p", str(int(pid))],
            capture_output=True,
            text=True,
            timeout=5,
            env={"LC_ALL": "C", "PATH": os.environ.get("PATH", "/bin:/usr/bin")},
        )
    except (OSError, subprocess.SubprocessError, ValueError):
        return None
    line = proc.stdout.strip()
    if proc.returncode != 0 or not line:
        return None
    stat, _, lstart = line.partition(" ")
    if stat.startswith("Z") and not allow_zombie:
        return None
    return "ps:" + " ".join(lstart.split())


def same_process(pid: int | None, token: str | None) -> bool:
    """True when ``pid`` is alive and is the process that ``token`` was taken from.

    Runs recorded without a token (the token could not be read at launch) fall back to
    the bare pid check.
    """
    if not pid_alive(pid):
        return False
    if not token:
        return True
    return proc_start_token(pid) == token
