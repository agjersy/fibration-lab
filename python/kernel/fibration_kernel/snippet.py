"""``exec.snippet``: a persistent REPL subprocess in the session interpreter.

The request/response protocol runs over two private pipes whose descriptor numbers are
passed on the command line. Descriptors 0, 1 and 2 belong to the snippet: stdin is
``/dev/null`` and fd 1 / fd 2 point at capture files, so output written by subprocesses or
C libraries is captured like ``print`` output and can never corrupt the protocol.
"""

from __future__ import annotations

import json
import logging
import os
import select
import signal
import subprocess
import threading
import time
from pathlib import Path
from typing import Any

from .errors import TIMEOUT, RpcError, invalid_params
from .sandbox import SandboxCommand, SandboxSpec, rlimit_prologue, wrap
from .spawner import spawn

log = logging.getLogger("fibration_kernel.snippet")

DEFAULT_TIMEOUT_S = 30.0
OUTPUT_CAP = 64 * 1024  # stdout, stderr and value_repr are each capped at this many characters

# Runs inside the session python: ``python -u -c DRIVER <request fd> <response fd>``.
REPL_DRIVER = r'''
import ast, json, os, sys, tempfile, traceback
_req = os.fdopen(int(sys.argv[1]), "r", encoding="utf-8")
_resp = os.fdopen(int(sys.argv[2]), "w", encoding="utf-8")
del sys.argv[1:]
_CAP = __CAP__
_MARK = "\n... [output truncated]\n"
_files = []
for _fd in (1, 2):
    _f = tempfile.TemporaryFile()
    os.dup2(_f.fileno(), _fd)  # fd-level capture: subprocesses and C code land here too
    _files.append(_f)
def _reset():
    for fd in (1, 2):
        os.ftruncate(fd, 0)
        os.lseek(fd, 0, os.SEEK_SET)
def _clip(text):
    if len(text) <= _CAP:
        return text
    return text[: _CAP - len(_MARK)] + _MARK
def _take(i):
    f = _files[i]
    f.seek(0)
    data = f.read(_CAP * 4 + 4)
    return _clip(data.decode("utf-8", "replace"))
ns = {"__name__": "__fibration_snippet__"}
def run(code):
    value = None
    ok = True
    _reset()
    try:
        tree = ast.parse(code, mode="exec")
        last = None
        if tree.body and isinstance(tree.body[-1], ast.Expr):
            last = ast.Expression(tree.body.pop().value)
        if tree.body:
            exec(compile(tree, "<snippet>", "exec"), ns)
        if last is not None:
            value = eval(compile(last, "<snippet>", "eval"), ns)
            ns["_"] = value
    except SystemExit:
        ok = False
    except BaseException:
        ok = False
        try:
            traceback.print_exc(file=sys.stderr)
        except Exception:
            os.write(2, traceback.format_exc().encode("utf-8", "replace"))
    rep = None
    if value is not None:
        try:
            rep = _clip(repr(value))
        except Exception as exc:
            rep = "<repr failed: %s>" % exc
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.flush()
        except Exception:
            pass
    res = {"ok": ok, "stdout": _take(0), "stderr": _take(1), "value_repr": rep}
    _reset()
    return res
for line in _req:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except ValueError:
        continue
    res = run(req.get("code", ""))
    res["id"] = req.get("id")
    _resp.write(json.dumps(res) + "\n")
    _resp.flush()
'''.replace("__CAP__", str(OUTPUT_CAP))


class SnippetRunner:
    def __init__(
        self,
        python: str,
        root: Path,
        sandbox_kind: str,
        spec_factory,
        env: dict[str, str],
    ) -> None:
        self.python = python
        self.root = root
        self.sandbox_kind = sandbox_kind
        self.spec_factory = spec_factory  # () -> SandboxSpec
        self.env = env
        self._proc: subprocess.Popen | None = None
        self._req_w: int | None = None
        self._resp_r: int | None = None
        self._buf = b""
        self._lock = threading.Lock()  # one snippet at a time
        self._state = threading.Lock()  # guards _proc and the pipe ends
        self._seq = 0
        self._closed = False

    def _command(self, req_fd: int, resp_fd: int) -> SandboxCommand:
        spec: SandboxSpec = self.spec_factory()
        probe = wrap(self.sandbox_kind, spec, [])
        driver = rlimit_prologue(probe.rlimit_as, probe.rlimit_nofile) + REPL_DRIVER
        return wrap(self.sandbox_kind, spec, [self.python, "-u", "-c", driver, str(req_fd), str(resp_fd)])

    def _ensure(self) -> subprocess.Popen:
        if self._proc is not None and self._proc.poll() is None:
            return self._proc
        self._kill()
        req_r, req_w = os.pipe()
        resp_r, resp_w = os.pipe()
        try:
            cmd = self._command(req_r, resp_w)
            # Forked on the long-lived spawner thread so `bwrap --die-with-parent` does not kill
            # the REPL when the thread that asked for it exits.
            proc = spawn(
                lambda: subprocess.Popen(
                    cmd.argv,
                    cwd=str(self.root),
                    env=self.env,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    pass_fds=(req_r, resp_w),
                    start_new_session=True,
                )
            )
        except BaseException:
            for fd in (req_r, req_w, resp_r, resp_w):
                os.close(fd)
            raise
        os.close(req_r)
        os.close(resp_w)
        with self._state:
            self._proc, self._req_w, self._resp_r, self._buf = proc, req_w, resp_r, b""
        return proc

    def _kill(self) -> None:
        with self._state:
            proc, self._proc = self._proc, None
            fds = (self._req_w, self._resp_r)
            self._req_w = self._resp_r = None
            self._buf = b""
        for fd in fds:
            if fd is not None:
                try:
                    os.close(fd)
                except OSError:
                    pass
        if proc is None:
            return
        _kill_group(proc)
        try:
            proc.wait(timeout=2)
        except subprocess.TimeoutExpired:
            pass

    def _read_line(self, fd: int, deadline: float) -> tuple[bytes | None, bool]:
        """(line, timed_out). ``line`` is None on EOF or timeout."""
        while b"\n" not in self._buf:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None, True
            try:
                ready, _, _ = select.select([fd], [], [], min(remaining, 0.25))
            except (OSError, ValueError):
                return None, False
            if self._closed:
                return None, False
            if not ready:
                continue
            try:
                data = os.read(fd, 65536)
            except OSError:
                return None, False
            if not data:
                return None, False
            self._buf += data
        line, _, self._buf = self._buf.partition(b"\n")
        return line, False

    def run(self, code: str, timeout_s: float | None = None) -> dict[str, Any]:
        timeout = float(timeout_s) if timeout_s is not None else DEFAULT_TIMEOUT_S
        if not timeout > 0:
            raise invalid_params("timeout_s must be greater than zero")

        def failed(message: str) -> dict[str, Any]:
            return {"ok": False, "stdout": "", "stderr": message, "value_repr": None}

        with self._lock:
            if self._closed:
                return failed("REPL unavailable: the kernel is shutting down")
            try:
                self._ensure()
                self._seq += 1
                req_id = self._seq
                req_w, resp_r = self._req_w, self._resp_r
                assert req_w is not None and resp_r is not None
                payload = (json.dumps({"id": req_id, "code": code}) + "\n").encode("utf-8")
                view = memoryview(payload)
                while view:
                    view = view[os.write(req_w, view) :]
            except (OSError, ValueError, AssertionError) as exc:
                self._kill()
                return failed(f"REPL unavailable: {exc}")
            deadline = time.monotonic() + timeout
            while True:
                line, timed_out = self._read_line(resp_r, deadline)
                if line is None:
                    self._kill()
                    if timed_out:
                        raise RpcError(TIMEOUT, f"snippet exceeded {timeout:g}s; REPL restarted")
                    if self._closed:
                        return failed("REPL stopped: the kernel is shutting down")
                    return failed("REPL process exited; its state was lost and it restarts on the next call")
                try:
                    result = json.loads(line)
                except ValueError:
                    self._kill()
                    return failed("REPL protocol error")
                if isinstance(result, dict) and result.get("id") == req_id:
                    break
            result.pop("id", None)
            return result

    def shutdown(self) -> None:
        """Stop the REPL now. A running snippet is killed, not waited for."""
        self._closed = True
        with self._state:
            proc = self._proc
        if proc is not None:
            _kill_group(proc)
        with self._lock:
            self._kill()


def _kill_group(proc: subprocess.Popen) -> None:
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except OSError:
        try:
            proc.kill()
        except OSError:
            pass
