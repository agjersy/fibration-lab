"""The kernel's one long-lived helper process: the ``exec.snippet`` REPL and the equation analysis.

It runs in the session interpreter under the same sandbox as runs (``Kernel._tool_spec``).
Requests and responses are JSON lines over two private pipes whose descriptor numbers are
passed on the command line; descriptors 0, 1 and 2 belong to snippets (stdin is
``/dev/null``, fd 1 and fd 2 are capture files), so output written by subprocesses or C
libraries can never corrupt the protocol.

Two kinds of request share the process, one at a time, both on its main thread (the
simplification budget of ``eqcore`` is an interval timer, which needs the main thread):

- ``{"id", "op": "snippet", "code"}`` runs in one persistent namespace.
- ``{"id", "op": "analyze", "source", "line_offset", "is_method"}`` runs ``eqcore.analyze``.
  SymPy is imported from the tooling environment the first time (the prewarm does this in the
  background); it is put on ``sys.path`` only for that import, and no repository code is
  involved.

Each request has a time budget. When one is exceeded the whole helper is killed (no thread
or process keeps computing) and the next request starts a fresh one: the REPL's state is lost,
as it always was on a snippet timeout, and the equation analysis starts cold.
"""

from __future__ import annotations

import importlib.util
import json
import logging
import os
import select
import signal
import subprocess
import sys
import tempfile
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

from .errors import TIMEOUT, RpcError, invalid_params
from .sandbox import SandboxSpec, rlimit_prologue, wrap

log = logging.getLogger("analyst_kernel.helper")

SNIPPET_TIMEOUT_S = 30.0
ANALYZE_BUDGET_S = 10.0  # per function
START_S = 60.0  # a cold interpreter under a sandbox on a slow disk
OUTPUT_CAP = 64 * 1024  # stdout, stderr and value_repr are each capped at this many characters

# Analyzed once by ``Helper.prewarm``: a declared and a derived expression with a function call.
_WARMUP_SOURCE = (
    "def warmup(t, a, tau):\n"
    '    """Decay.\n\n    Math:\n        y = a * exp(-t / tau)\n    """\n'
    "    return a * np.exp(-t / tau) + np.sqrt(a) ** 2\n"
)

# Runs inside the session python: ``python -u -c DRIVER <request fd> <response fd> <eq paths>``.
DRIVER = r'''
import ast, json, os, sys, tempfile, traceback
_req = os.fdopen(int(sys.argv[1]), "r", encoding="utf-8")
_resp = os.fdopen(int(sys.argv[2]), "w", encoding="utf-8")
_eq_paths = json.loads(sys.argv[3])
del sys.argv[1:]
sys.setrecursionlimit(2000)
def _send(obj):
    _resp.write(json.dumps(obj) + "\n")
    _resp.flush()
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
ns = {"__name__": "__analyst_snippet__"}
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
_eqcore = []
def _analyze(req):
    if not _eqcore:
        added = [p for p in _eq_paths if p not in sys.path]
        sys.path[:0] = added
        try:
            from analyst_kernel import eqcore
        finally:
            for p in added:
                try:
                    sys.path.remove(p)
                except ValueError:
                    pass
        _eqcore.append(eqcore)
    return _eqcore[0].analyze(req["source"], int(req.get("line_offset", 0)), bool(req.get("is_method", False)))
def _analyze_request(req):
    res = {"id": req.get("id")}
    try:
        res["result"] = _analyze(req)
    except MemoryError:
        res["error"] = "out of memory while analyzing the expression"
    except RecursionError:
        res["error"] = "expression too deeply nested"
    except BaseException as exc:
        res["error"] = "%s: %s" % (type(exc).__name__, exc)
    return res
_send({"ready": True})
for line in _req:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except ValueError:
        continue
    if req.get("op") == "analyze":
        _send(_analyze_request(req))
        continue
    res = run(req.get("code", ""))
    res["id"] = req.get("id")
    _send(res)
'''.replace("__CAP__", str(OUTPUT_CAP))


def equation_paths() -> list[str]:
    """Directories the helper puts on ``sys.path`` to import ``analyst_kernel.eqcore`` and SymPy
    from the tooling environment, found without importing SymPy here."""
    paths = [str(Path(__file__).resolve().parent.parent)]
    for name in ("sympy", "mpmath"):
        spec = importlib.util.find_spec(name)
        if spec is not None and spec.submodule_search_locations:
            parent = str(Path(next(iter(spec.submodule_search_locations))).resolve().parent)
            if parent not in paths:
                paths.append(parent)
    return paths


def equation_dirs() -> list[Path]:
    """The package directories behind ``equation_paths``, for the sandbox to make readable."""
    dirs = [Path(__file__).resolve().parent]
    for name in ("sympy", "mpmath"):
        spec = importlib.util.find_spec(name)
        if spec is not None and spec.submodule_search_locations:
            dirs.append(Path(next(iter(spec.submodule_search_locations))).resolve())
    return dirs


class Helper:
    """Client of the helper process: started on first use (or by ``prewarm``), one request at a
    time, restarted on demand."""

    def __init__(
        self,
        python: str | None = None,
        root: Path | None = None,
        sandbox_kind: str | None = None,
        spec_factory: Callable[[], SandboxSpec] | None = None,
        env: dict[str, str] | None = None,
        budget_s: float = ANALYZE_BUDGET_S,
    ) -> None:
        self.python = python or sys.executable
        self.root = root
        self.sandbox_kind = sandbox_kind
        self.spec_factory = spec_factory  # None: unsandboxed (tests, standalone ``eq_extract``)
        self.env = env
        self.budget_s = budget_s
        self._proc: subprocess.Popen | None = None
        self._req_w: int | None = None
        self._reader: _LineReader | None = None
        self._lock = threading.Lock()  # one request at a time; held while starting
        self._seq = 0
        self._closed = False

    # ---- process management -------------------------------------------------------
    def _argv(self, req_fd: int, resp_fd: int) -> list[str]:
        tail = [str(req_fd), str(resp_fd), json.dumps(equation_paths())]
        if self.spec_factory is None or self.sandbox_kind is None:
            return [self.python, "-u", "-c", DRIVER, *tail]
        spec = self.spec_factory()
        probe = wrap(self.sandbox_kind, spec, [])
        driver = rlimit_prologue(probe.rlimit_as, probe.rlimit_nofile) + DRIVER
        return wrap(self.sandbox_kind, spec, [self.python, "-u", "-c", driver, *tail]).argv

    def _start(self) -> None:
        """Spawns the helper and waits for its ready line. Caller holds ``_lock``."""
        req_r, req_w = os.pipe()
        resp_r, resp_w = os.pipe()
        try:
            env = self.env
            if env is None:
                env = {k: v for k, v in os.environ.items() if k in ("PATH", "HOME", "LANG", "TMPDIR", "SYSTEMROOT")}
            proc = subprocess.Popen(
                self._argv(req_r, resp_w),
                cwd=str(self.root) if self.root is not None else tempfile.gettempdir(),
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                pass_fds=(req_r, resp_w),
                start_new_session=True,
            )
        except BaseException:
            for fd in (req_r, req_w, resp_r, resp_w):
                os.close(fd)
            raise
        os.close(req_r)
        os.close(resp_w)
        self._proc, self._req_w, self._reader = proc, req_w, _LineReader(resp_r)
        ready = self._reader.line(time.monotonic() + START_S, lambda: self._closed)
        if not (isinstance(ready, dict) and ready.get("ready")):
            self._kill()
            raise RuntimeError("the helper process did not start")

    def _kill(self) -> None:
        proc, self._proc = self._proc, None
        fd, self._req_w = self._req_w, None
        reader, self._reader = self._reader, None
        if fd is not None:
            try:
                os.close(fd)
            except OSError:
                pass
        if reader is not None:
            reader.close()
        if proc is not None:
            _kill_group(proc)
            _reap(proc)

    def _request(self, payload: dict[str, Any], timeout: float) -> tuple[dict[str, Any] | None, str | None]:
        """(response, None), or (None, reason) with reason ``timeout``, ``exited`` or ``closed``.
        Caller holds ``_lock``."""
        if self._closed:
            return None, "closed"
        try:
            if self._proc is None or self._proc.poll() is not None:
                self._kill()
                self._start()
            self._seq += 1
            req_id = self._seq
            fd = self._req_w
            assert fd is not None
            view = memoryview((json.dumps({**payload, "id": req_id}) + "\n").encode("utf-8"))
            while view:
                view = view[os.write(fd, view) :]
        except (OSError, ValueError, AssertionError):
            self._kill()
            return None, "closed" if self._closed else "exited"
        deadline = time.monotonic() + timeout
        while True:
            reader = self._reader
            res = reader.line(deadline, lambda: self._closed) if reader is not None else None
            if res is None:
                timed_out = time.monotonic() >= deadline
                self._kill()  # no thread or process keeps computing after this
                if self._closed:
                    return None, "closed"
                return None, "timeout" if timed_out else "exited"
            if res.get("id") == req_id:
                return res, None

    # ---- requests -------------------------------------------------------------------
    def run(self, code: str, timeout_s: float | None = None) -> dict[str, Any]:
        """One ``exec.snippet``: ``{ok, stdout, stderr, value_repr}``."""
        timeout = float(timeout_s) if timeout_s is not None else SNIPPET_TIMEOUT_S
        if not timeout > 0:
            raise invalid_params("timeout_s must be greater than zero")

        def failed(message: str) -> dict[str, Any]:
            return {"ok": False, "stdout": "", "stderr": message, "value_repr": None}

        with self._lock:
            try:
                res, why = self._request({"op": "snippet", "code": code}, timeout)
            except RuntimeError as exc:
                return failed(f"REPL unavailable: {exc}")
        if why == "timeout":
            raise RpcError(TIMEOUT, f"snippet exceeded {timeout:g}s; REPL restarted")
        if why == "closed":
            return failed("REPL unavailable: the kernel is shutting down")
        if res is None:
            return failed("REPL process exited; its state was lost and it restarts on the next call")
        res.pop("id", None)
        return res

    def analyze(self, source: str, line_offset: int, is_method: bool) -> dict[str, Any]:
        """Analysis of one function; ``{"error": reason}`` when the helper failed or timed out."""
        payload = {"op": "analyze", "source": source, "line_offset": line_offset, "is_method": is_method}
        with self._lock:
            try:
                res, why = self._request(payload, self.budget_s)
            except RuntimeError as exc:
                return {"error": f"equation analysis unavailable: {exc}"}
        if why == "timeout":
            return {"error": f"equation analysis exceeded {self.budget_s:g}s and was stopped"}
        if why == "closed":
            return {"error": "kernel is shutting down"}
        if res is None:
            return {"error": "the helper process exited while analyzing this function"}
        if "error" in res:
            return {"error": str(res["error"])}
        result = res.get("result")
        return result if isinstance(result, dict) else {"error": "helper protocol error"}

    def prewarm(self) -> bool:
        """Start the helper now and analyze one small function, which imports SymPy and the parts
        it loads lazily, so the first ``eq.extract`` (and the first snippet) does not wait.
        Safe from a background thread. True when the analysis worked."""
        if self._closed:
            return False
        return "error" not in self.analyze(_WARMUP_SOURCE, 0, False)

    def close(self) -> None:
        """Stop the helper now: a request in flight is failed, not waited for."""
        self._closed = True
        proc = self._proc
        if proc is not None:
            _kill_group(proc)  # the request in flight sees EOF and returns
        with self._lock:
            self._kill()


class _LineReader:
    """JSON lines from a pipe descriptor."""

    def __init__(self, fd: int) -> None:
        self.fd = fd
        self.buf = b""

    def line(self, deadline: float | None, stop: Callable[[], bool]) -> dict[str, Any] | None:
        while b"\n" not in self.buf:
            wait = 0.25 if deadline is None else min(0.25, deadline - time.monotonic())
            if wait <= 0 or stop():
                return None
            try:
                ready, _, _ = select.select([self.fd], [], [], wait)
            except (OSError, ValueError):
                return None
            if not ready:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return None
            if not data:
                return None
            self.buf += data
        raw, _, self.buf = self.buf.partition(b"\n")
        try:
            obj = json.loads(raw)
        except ValueError:
            return {}
        return obj if isinstance(obj, dict) else {}

    def close(self) -> None:
        try:
            os.close(self.fd)
        except OSError:
            pass


def _kill_group(proc: subprocess.Popen) -> None:
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except OSError:
        try:
            proc.kill()
        except OSError:
            pass


def _reap(proc: subprocess.Popen) -> None:
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:  # pragma: no cover
        pass
