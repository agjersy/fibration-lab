# The bwrap argv, sandbox-exec profile, and limits logic are ported from Personal Analyst
# python/kernel/analyst_kernel/sandbox.py at commit f229e4d and adapted to deck builds.
"""Code sandbox (PROTOCOL.md section 9): one wrapper for builds, generated slides, and skill scripts.

macOS uses ``sandbox-exec`` with a generated profile, Linux uses ``bwrap``, and Windows uses the
``present-sandbox`` helper from the ``wb-sandbox`` crate (AppContainer + job object). Where none
is usable the run falls back to resource limits only and is reported as ``limits``.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ..errors import SANDBOX_UNAVAILABLE, RpcError

DEFAULT_MEMORY = 2 * 1024**3
DEFAULT_PROCESSES = 64
_ENV_ALLOW = ("PATH", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "TMPDIR")


@dataclass
class SandboxSpec:
    cwd: Path
    scratch: Path  # writable; also HOME for the child
    read_only: list[Path] = field(default_factory=list)
    writable: list[Path] = field(default_factory=list)
    memory: int = DEFAULT_MEMORY
    processes: int = DEFAULT_PROCESSES

    def ro(self) -> list[Path]:
        return _dedupe(_with_realpaths([d for d in self.read_only if d.exists()]))

    def rw(self) -> list[Path]:
        return _dedupe(_with_realpaths([self.scratch, *[d for d in self.writable if d.exists()]]))


def _with_realpaths(paths: list[Path]) -> list[Path]:
    out: list[Path] = []
    for p in paths:
        out.append(p)
        real = Path(os.path.realpath(p))
        if real != p:
            out.append(real)
    return out


def _dedupe(paths: list[Path]) -> list[Path]:
    seen: set[str] = set()
    out = []
    for p in paths:
        if str(p) not in seen:
            seen.add(str(p))
            out.append(p)
    return out


def interpreter_dirs(python: Path) -> list[Path]:
    """The interpreter's installation and, for a venv, its base installation."""
    prefixes = [Path(sys.prefix), Path(sys.base_prefix)]
    real = Path(os.path.realpath(python))
    prefixes.append(real.parent.parent if real.parent.name in ("bin", "Scripts") else real.parent)
    return [p for p in _dedupe(prefixes) if p.exists() and p != Path(p.anchor)]


def code_dirs() -> list[Path]:
    """Where ``present_kernel`` and ``present_sdk`` live (editable installs in a dev checkout)."""
    import present_sdk

    import present_kernel

    return _dedupe([Path(present_kernel.__file__).resolve().parents[1], Path(present_sdk.__file__).resolve().parents[1]])


# ---- detection -------------------------------------------------------------------------------


def _userns_usable() -> bool:
    try:
        for name, bad in (("/proc/sys/kernel/unprivileged_userns_clone", "0"), ("/proc/sys/user/max_user_namespaces", "0")):
            p = Path(name)
            if p.exists() and p.read_text().strip() == bad:
                return False
    except OSError:
        return False
    return True


def bwrap_available() -> bool:
    if sys.platform != "linux" or shutil.which("bwrap") is None or not _userns_usable():
        return False
    try:
        proc = subprocess.run(["bwrap", "--ro-bind", "/", "/", "--unshare-all", "--", "/bin/true"], capture_output=True, timeout=5)
        return proc.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def helper_path(home: Path | None = None) -> str | None:
    """The ``present-sandbox`` helper (Windows AppContainer launcher)."""
    exe = "present-sandbox.exe" if sys.platform == "win32" else "present-sandbox"
    candidates = []
    if os.environ.get("PRESENT_SANDBOX_HELPER"):
        candidates.append(Path(os.environ["PRESENT_SANDBOX_HELPER"]))
    if home is not None:
        candidates.append(home / "runtime" / exe)
    repo = Path(__file__).resolve().parents[4]
    candidates += [repo / "target" / "release" / exe, repo / "target" / "debug" / exe]
    for c in candidates:
        if c.is_file():
            return str(c)
    return None


def _probe_helper(helper: str | None) -> tuple[bool, str]:
    """Start a trivial command in the AppContainer once: policy can forbid creating profiles."""
    if helper is None:
        return False, "the present-sandbox helper was not found"
    try:
        scratch = Path(os.environ.get("TEMP") or os.environ.get("TMP") or ".")
        proc = subprocess.run([helper, "--cwd", str(scratch), "--memory", str(DEFAULT_MEMORY), "--processes", "8", "--",
                               os.environ.get("COMSPEC", "cmd.exe"), "/c", "exit", "0"], capture_output=True, timeout=30)  # fmt: skip
    except (OSError, subprocess.SubprocessError) as exc:
        return False, f"the AppContainer sandbox could not be started: {exc}"
    if proc.returncode != 0:
        detail = proc.stderr.decode("utf-8", "replace").strip()[-200:]
        return False, f"the AppContainer sandbox is not usable on this machine (exit {proc.returncode}): {detail}"
    return True, ""


def detect(requested: str = "auto", home: Path | None = None) -> dict[str, Any]:
    """Resolve the sandbox preference to ``SandboxInfo`` (PROTOCOL 1.3)."""
    if sys.platform == "darwin" and shutil.which("sandbox-exec"):
        native = ("sandbox-exec", True, "macOS sandbox-exec with a generated profile")
    elif sys.platform == "linux":
        ok = bwrap_available()
        native = ("bwrap", ok, "bubblewrap" if ok else "bubblewrap is not installed or user namespaces are disabled")
    elif sys.platform == "win32":
        helper = helper_path(home)
        ok, why = _probe_helper(helper)
        native = ("appcontainer", ok, "AppContainer with a job object" if ok else why)
    else:
        native = ("limits", False, "no native sandbox on this platform")
    if requested == "limits":
        return {"kind": "limits", "available": True, "detail": "resource limits only (requested)"}
    kind, ok, detail = native
    if ok:
        return {"kind": kind, "available": True, "detail": detail}
    if requested == "native":
        raise RpcError(SANDBOX_UNAVAILABLE, detail)
    return {"kind": "limits", "available": True, "detail": f"resource limits only: {detail}"}


# ---- wrappers --------------------------------------------------------------------------------


def _q(path: Path | str) -> str:
    return '"' + str(path).replace("\\", "\\\\").replace('"', '\\"') + '"'


def _ancestors_under(path: Path, home: Path) -> list[Path]:
    try:
        rel = path.relative_to(home)
    except ValueError:
        return []
    out = [home]
    cur = home
    for part in rel.parts[:-1]:
        cur = cur / part
        out.append(cur)
    return out


def sandbox_exec_profile(spec: SandboxSpec, home: Path | None = None) -> str:
    """SBPL profile: read everywhere except the home directory, then re-allow the listed subtrees.

    The macOS sandbox checks ``file-read-metadata`` on every path component during lookup, so each
    re-allowed subtree under home needs metadata access on its chain of ancestors.
    """
    home = Path(os.path.realpath(home or Path.home()))
    lines = ["(version 1)", "(deny default)", '(allow file-read* (subpath "/"))', f"(deny file-read* (subpath {_q(home)}))"]
    ro, rw = spec.ro(), spec.rw()
    ancestors: list[Path] = []
    for d in ro + rw:
        ancestors += _ancestors_under(Path(os.path.realpath(d)), home)
        ancestors += _ancestors_under(d, home)
    for a in _dedupe(ancestors):
        lines.append(f"(allow file-read-metadata (literal {_q(a)}))")
    for d in ro:
        lines.append(f"(allow file-read* (subpath {_q(d)}))")
    for d in rw:
        lines.append(f"(allow file-read* file-write* (subpath {_q(d)}))")
    lines += ['(allow file-write* (literal "/dev/null"))', "(allow process*)", "(allow sysctl-read)", "(allow mach-lookup)",
              "(deny network*)"]  # fmt: skip
    return "\n".join(lines) + "\n"


def bwrap_argv(spec: SandboxSpec, cmd: list[str], home: Path | None = None) -> list[str]:
    home = Path(os.path.realpath(home or Path.home()))
    argv = ["bwrap", "--ro-bind", "/", "/", "--tmpfs", str(home)]
    if home != Path("/tmp"):
        argv += ["--tmpfs", "/tmp"]
    for d in spec.ro():
        argv += ["--ro-bind", str(d), str(d)]
    for d in spec.rw():
        argv += ["--bind", str(d), str(d)]
    argv += ["--unshare-net", "--die-with-parent", "--new-session", "--unshare-pid", "--proc", "/proc", "--dev", "/dev",
             "--chdir", str(spec.cwd), "--"]  # fmt: skip
    return argv + list(cmd)


def helper_argv(helper: str, spec: SandboxSpec, cmd: list[str]) -> list[str]:
    argv = [helper, "--cwd", str(spec.cwd), "--memory", str(spec.memory), "--processes", str(spec.processes)]
    for d in spec.ro():
        argv += ["--read", str(d)]
    for d in spec.rw():
        argv += ["--write", str(d)]
    return [*argv, "--", *cmd]


def _limits_prologue(memory: int) -> str:
    """Applied by the runner inside the child (``preexec_fn`` is unsafe in the threaded kernel)."""
    return str(memory)


def child_env(spec: SandboxSpec, extra: dict[str, str]) -> dict[str, str]:
    env = {k: os.environ[k] for k in _ENV_ALLOW if k in os.environ}
    env.update({"HOME": str(spec.scratch), "USERPROFILE": str(spec.scratch), "TMPDIR": str(spec.scratch),
                "TEMP": str(spec.scratch), "TMP": str(spec.scratch), "MPLCONFIGDIR": str(spec.scratch / "mpl"),
                "MPLBACKEND": "Agg", "PYTHONDONTWRITEBYTECODE": "1", "PYTHONIOENCODING": "utf-8",
                "PRESENT_RLIMIT_AS": str(spec.memory)})  # fmt: skip
    # Text measurement inside the sandbox must see the same fonts as the kernel does.
    if os.environ.get("PRESENT_FONT_DIRS"):
        env["PRESENT_FONT_DIRS"] = os.environ["PRESENT_FONT_DIRS"]
    env.update(extra)
    return env


@dataclass
class Result:
    kind: str
    exit_code: int
    stdout: str
    stderr: str
    timed_out: bool


def run(kind: str, spec: SandboxSpec, cmd: list[str], env: dict[str, str], timeout: float, home: Path | None = None) -> Result:
    if kind == "sandbox-exec":
        argv = ["sandbox-exec", "-p", sandbox_exec_profile(spec), *cmd]
    elif kind == "bwrap":
        argv = bwrap_argv(spec, cmd)
    elif kind == "appcontainer":
        helper = helper_path(home)
        if helper is None:
            raise RpcError(SANDBOX_UNAVAILABLE, "the present-sandbox helper was not found")
        argv = helper_argv(helper, spec, cmd)
    else:
        argv = list(cmd)
    kwargs: dict[str, Any] = {}
    if sys.platform != "win32":
        kwargs["start_new_session"] = True
    proc = subprocess.Popen(argv, cwd=str(spec.cwd), env=child_env(spec, env), stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, stdin=subprocess.DEVNULL, **kwargs)  # fmt: skip
    timed_out = False
    try:
        out, err = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        _kill_tree(proc)
        out, err = proc.communicate()
    return Result(kind, proc.returncode if proc.returncode is not None else -1, out.decode("utf-8", "replace"),
                  err.decode("utf-8", "replace"), timed_out)  # fmt: skip


def _kill_tree(proc: subprocess.Popen) -> None:
    try:
        if sys.platform == "win32":
            subprocess.run(["taskkill", "/F", "/T", "/PID", str(proc.pid)], capture_output=True)
        else:
            import signal

            os.killpg(proc.pid, signal.SIGKILL)
    except (OSError, ProcessLookupError):
        proc.kill()
