"""Run execution (PROTOCOL.md 1.4): detached runs, log tailing, artifact discovery, attach.

Completion is signalled, not polled: the kernel hands each supervisor the write end of a
pipe, and the supervisor closes it the instant the result is on disk. The watcher sleeps
in ``select`` on the read end, so ``run.done`` follows process exit by a few milliseconds.
Only runs adopted after a kernel restart (no pipe) fall back to polling the status file.
"""

from __future__ import annotations

import json
import logging
import os
import secrets
import select
import signal
import subprocess
import sys
import threading
import time
from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from .config import ProjectConfig, project_data_dirs, validate_out_dir
from .errors import RUN_NOT_FOUND, SCRIPT_NOT_FOUND, RpcError, invalid_params
from .paths import mtime_iso, parse_iso, rel_to_root, resolve_in_root, utc_now_iso
from .rpc import Deferred
from .runstate import proc_start_token, read_status_line, same_process, transition, write_atomic
from .sandbox import SandboxSpec, tooling_dir, venv_of, wrap

log = logging.getLogger("fibration_kernel.runs")

POLL_S = 0.25  # idle tail interval and live artifact-scan interval
TAIL_MIN_S = 0.03  # tail interval while output is flowing
TAIL_BACKOFF = 1.5  # growth of the tail interval per quiet tick, up to POLL_S
MPLCONFIG_DIR = "_mplconfig"  # under runs_dir: one Matplotlib cache shared by every run
CHUNK_BYTES = 64 * 1024  # largest run.stdout / run.stderr chunk
MAX_CHUNKS_PER_POLL = 64  # a fast producer cannot hold the watcher in one tail forever
PASSTHROUGH_ENV = ("HOME", "PATH", "LANG", "VIRTUAL_ENV")
FIGURE_EXTS = (".png", ".svg", ".pdf")
SIDECAR_SUFFIX = ".meta.json"
STARTUP_GRACE_S = 10.0
ARTIFACT_KINDS = {
    ".png": "figure",
    ".svg": "figure",
    ".pdf": "figure",
    ".csv": "table",
    ".tsv": "table",
    ".parquet": "table",
    ".npy": "array",
    ".npz": "array",
    ".json": "json",
    ".txt": "text",
    ".md": "text",
    ".log": "text",
}
# Named results (``fibration_results.record``) are a bounded extract: they travel like rendered output.
RENDERED_KINDS = {"figure", "json", "text", "results"}
RESULTS_FILE = "results.json"
META_KEYS = (
    "run_id",
    "script",
    "args",
    "status",
    "exit_code",
    "started_at",
    "finished_at",
    "out_dir",
    "sandbox",
    "tree_hash",
    "pid",
    "host",
)

Notify = Callable[[str, dict[str, Any]], None]


def artifact_kind(path: Path) -> str:
    return ARTIFACT_KINDS.get(path.suffix.lower(), "file")


def is_results_file(path: Path) -> bool:
    """Whether ``results.json`` is the file ``fibration_results.record`` writes (a user's own
    ``results.json`` stays a plain JSON artifact)."""
    try:
        if path.stat().st_size > 1_000_000:
            return False
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    return isinstance(data, dict) and data.get("schema") == 1 and isinstance(data.get("results"), list)


def new_run_id() -> str:
    stamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
    return f"{stamp}-{secrets.token_hex(3)}"


def status_from_line(line: str) -> tuple[str, int | None]:
    if line.startswith("done:"):
        try:
            return "done", int(line.split(":", 1)[1])
        except ValueError:
            return "done", None
    if line in ("running", "cancelled", "timeout", "failed"):
        return line, None
    return "failed", None


def read_meta(run_dir: Path) -> dict[str, Any]:
    try:
        meta = json.loads((run_dir / "meta.json").read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise RpcError(RUN_NOT_FOUND, f"run {run_dir.name} has no readable meta.json") from exc
    # The status file is the source of truth for the state; meta may lag behind.
    status, code = status_from_line(read_status_line(run_dir))
    if status != "running" or meta.get("status") == "running":
        meta["status"] = status
        if status == "done":
            meta["exit_code"] = code
        elif meta.get("exit_code") is None:
            meta["exit_code"] = None
    return {k: meta.get(k) for k in META_KEYS}


def sidecar_path(figure: Path) -> Path:
    """``residuals.png`` -> ``residuals.png.meta.json``."""
    return figure.with_name(figure.name + SIDECAR_SUFFIX)


def sidecar_target(path: Path) -> Path | None:
    """The figure a ``<name>.<png|svg|pdf>.meta.json`` file describes, else None."""
    if not path.name.lower().endswith(SIDECAR_SUFFIX):
        return None
    target = path.with_name(path.name[: -len(SIDECAR_SUFFIX)])
    return target if target.suffix.lower() in FIGURE_EXTS else None


def valid_sidecar(obj: Any) -> bool:
    """Shape check for FigureSidecar (PROTOCOL.md 1.6); anything else is a user's JSON file."""

    def opt_str(v: Any) -> bool:
        return v is None or isinstance(v, str)

    def number(v: Any) -> bool:
        return isinstance(v, (int, float)) and not isinstance(v, bool)

    if not isinstance(obj, dict):
        return False
    axes = obj.get("axes")
    args = obj.get("args")
    return (
        "run_id" in obj
        and opt_str(obj.get("run_id"))
        and isinstance(obj.get("script"), str)
        and isinstance(args, list)
        and all(isinstance(a, str) for a in args)
        and "title" in obj
        and opt_str(obj.get("title"))
        and isinstance(axes, list)
        and all(
            isinstance(ax, dict) and all(k in ax and opt_str(ax[k]) for k in ("title", "xlabel", "ylabel"))
            for ax in axes
        )
        and isinstance(obj.get("created_at"), str)
        and all(number(obj.get(k)) for k in ("width_in", "height_in", "dpi"))
    )


def read_sidecar(figure: Path) -> dict[str, Any] | None:
    try:
        obj = json.loads(sidecar_path(figure).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return obj if valid_sidecar(obj) else None


def utf8_cut(data: bytes) -> int:
    """Length of the prefix of ``data`` that does not end inside a UTF-8 sequence.

    Only a genuinely incomplete trailing sequence (a lead byte whose continuation bytes
    have not arrived) is held back; invalid bytes are passed through and decoded with
    replacement. This is an incremental decoder that keeps chunk offsets byte-exact.
    """
    n = len(data)
    for back in range(1, min(4, n) + 1):
        byte = data[n - back]
        if byte & 0xC0 == 0x80:  # continuation byte: keep looking for the lead
            continue
        if byte >= 0xF5:
            return n  # not a valid lead byte: nothing to wait for
        if byte >= 0xF0:
            need = 4
        elif byte >= 0xE0:
            need = 3
        elif byte >= 0xC0:
            need = 2
        else:
            return n
        return n - back if back < need else n
    return n


def read_identity(run_dir: Path) -> tuple[int | None, str | None, str | None]:
    """(supervisor pid, start-time token, started_at) from meta.json."""
    try:
        meta = json.loads((run_dir / "meta.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None, None, None
    pid = meta.get("pid")
    return (pid if isinstance(pid, int) else None), meta.get("pid_start"), meta.get("started_at")


def supervisor_alive(run_dir: Path) -> bool:
    """True when the recorded supervisor is still the process running this run."""
    pid, token, started_at = read_identity(run_dir)
    if pid is None:
        # Not launched yet (or meta unreadable): give a fresh run a moment before judging.
        started = parse_iso(started_at)
        return started is not None and time.time() - started < STARTUP_GRACE_S
    return same_process(pid, token)


def reconcile(run_dir: Path) -> None:
    """Mark a run ``failed`` when it claims to be running but its supervisor is gone."""
    if read_status_line(run_dir) != "running" or supervisor_alive(run_dir):
        return
    time.sleep(0.05)  # the supervisor may have been between writing its result and exiting
    if read_status_line(run_dir) == "running":
        _finish_failed(run_dir, "fibration: the run's supervisor process is gone; the run is marked failed\n")


class RunManager:
    """Owns the runs directory. Every query reads disk; memory holds only live watchers."""

    def __init__(
        self,
        root: Path,
        runs_dir: Path,
        python: str,
        sandbox_kind: str,
        config: ProjectConfig,
        notify: Notify,
        mpl_dir: Path,
        host: str,
        out_dir_name: str | None = None,
        supervisor_python: str | None = None,
        poll_s: float = POLL_S,
        data_dirs: list[Path] | None = None,
        tail_min_s: float = TAIL_MIN_S,
    ) -> None:
        self.root = root.resolve()
        self.runs_dir = runs_dir.resolve()
        self.python = python
        self.sandbox_kind = sandbox_kind
        self.config = config
        self.notify = notify
        self.mpl_dir = mpl_dir
        self.host = host
        self.out_dir_name = validate_out_dir(self.root, out_dir_name or config.out_dir)
        if data_dirs is None:
            data_dirs, warnings = project_data_dirs(self.root, config.data_dirs)
            for w in warnings:
                log.warning("%s", w)
        self.data_dirs = list(data_dirs)
        self.supervisor_python = supervisor_python or sys.executable
        self.poll_s = poll_s
        self.tail_min_s = tail_min_s
        self._watchers: dict[str, _Watcher] = {}
        self._lock = threading.Lock()
        self.runs_dir.mkdir(parents=True, exist_ok=True)
        # One Matplotlib config/cache directory for every run, the REPL and eq.check: the font
        # cache is built once per runs dir instead of once per run.
        self.mpl_config_dir = self.runs_dir / MPLCONFIG_DIR
        self.mpl_config_dir.mkdir(parents=True, exist_ok=True)

    # ---- helpers ------------------------------------------------------------
    def run_dir(self, run_id: str) -> Path:
        if not run_id or "/" in run_id or run_id in (".", "..") or run_id.startswith("_"):
            raise RpcError(RUN_NOT_FOUND, f"invalid run id {run_id!r}")
        d = self.runs_dir / run_id
        if not (d / "meta.json").is_file():
            raise RpcError(RUN_NOT_FOUND, f"run {run_id} not found")
        return d

    def _build_env(self, run_id: str, run_dir: Path, out_abs: Path, extra: dict[str, str]) -> dict[str, str]:
        env: dict[str, str] = {}
        for key in PASSTHROUGH_ENV:
            if key in os.environ:
                env[key] = os.environ[key]
        env.update({k: str(v) for k, v in extra.items()})
        # fibration_mpl first (figure capture), then whatever the caller set, then the repo root so
        # ``python scripts/x.py`` can import the library package without installing it.
        parts = [str(self.mpl_dir)]
        if env.get("PYTHONPATH"):
            parts.append(env["PYTHONPATH"])
        parts.append(str(self.root))
        pythonpath = os.pathsep.join(parts)
        env.update(
            {
                "FIBRATION_RUN_ID": run_id,
                "FIBRATION_OUT_DIR": str(out_abs),
                "FIBRATION_ROOT": str(self.root),
                "MPLBACKEND": "module://fibration_mpl",
                "MPLCONFIGDIR": str(self.mpl_config_dir),
                "FIBRATION_FIGURE_FORMATS": ",".join(self.config.figure_formats),
                "TMPDIR": str(run_dir / "tmp"),
                "PYTHONPATH": pythonpath,
                "PYTHONUNBUFFERED": "1",
            }
        )
        return env

    def sandbox_spec(self, run_dir: Path, out_abs: Path) -> SandboxSpec:
        derived = self.root / "data" / "derived"
        return SandboxSpec(
            root=self.root,
            venv=venv_of(Path(self.python)),
            python=Path(self.python),
            run_dir=run_dir,
            out_dir=out_abs,
            data_dirs=list(self.data_dirs),
            support_dirs=[self.mpl_dir / "fibration_mpl", self.mpl_dir / "fibration_results"],
            tooling_dir=tooling_dir(),
            cache_dirs=[self.mpl_config_dir],
            derived_dir=derived if derived.is_dir() else None,
            rlimit_as=self.config.rlimit_as(),
            rlimit_nofile=self.config.open_files,
        )

    # ---- run.start ------------------------------------------------------------
    def start(
        self,
        script: str,
        args: list[str] | None = None,
        env: dict[str, str] | None = None,
        timeout_s: float | None = None,
        tree_hash: str | None = None,
    ) -> dict[str, Any]:
        args = [str(a) for a in (args or [])]
        script_abs = resolve_in_root(self.root, script)
        if not script_abs.is_file() or script_abs.suffix != ".py":
            raise RpcError(SCRIPT_NOT_FOUND, f"{script} is not an existing Python file")
        script_rel = rel_to_root(self.root, script_abs)
        timeout = float(timeout_s) if timeout_s is not None else float(self.config.timeout_s)
        if not timeout > 0:
            raise invalid_params("timeout_s must be greater than zero")

        run_id = new_run_id()
        run_dir = self.runs_dir / run_id
        while run_dir.exists():  # pragma: no cover - 6 hex digits per second
            run_id = new_run_id()
            run_dir = self.runs_dir / run_id
        run_dir.mkdir(parents=True)
        (run_dir / "tmp").mkdir()
        out_rel = f"{self.out_dir_name}/{script_abs.stem}/{run_id}"
        out_abs = self.root / out_rel
        out_abs.mkdir(parents=True, exist_ok=True)

        started_at = utc_now_iso()
        meta: dict[str, Any] = {
            "run_id": run_id,
            "script": script_rel,
            "args": args,
            "status": "running",
            "exit_code": None,
            "started_at": started_at,
            "finished_at": None,
            "out_dir": out_rel,
            "sandbox": self.sandbox_kind,
            "tree_hash": tree_hash,
            "pid": None,
            "host": self.host,
        }
        (run_dir / "stdout.log").touch()
        (run_dir / "stderr.log").touch()
        (run_dir / "artifacts.jsonl").touch()
        write_atomic(run_dir / "status", "running\n")
        write_atomic(run_dir / "launch.json", json.dumps({"timeout_s": timeout}))
        write_atomic(
            out_abs / "run.json",
            json.dumps(
                {
                    "run_id": run_id,
                    "script": script_rel,
                    "args": args,
                    "started_at": started_at,
                    "host": self.host,
                    "tree_hash": tree_hash,
                },
                indent=2,
            )
            + "\n",
        )

        cmd = [self.python, str(script_abs), *args]
        wrapped = wrap(self.sandbox_kind, self.sandbox_spec(run_dir, out_abs), cmd)
        sup = [
            self.supervisor_python,
            "-m",
            "fibration_kernel._supervise",
            "--run-dir",
            str(run_dir),
            "--timeout",
            str(timeout),
        ]
        if wrapped.rlimit_as:
            sup += ["--rlimit-as", str(wrapped.rlimit_as)]
        if wrapped.rlimit_nofile:
            sup += ["--rlimit-nofile", str(wrapped.rlimit_nofile)]
        # The supervisor closes its end of this pipe once the result is recorded; the watcher
        # waits on ours. Only the supervisor inherits the write end (never the script).
        exit_r: int | None
        exit_r, exit_w = os.pipe()
        sup += ["--exit-fd", str(exit_w), "--", *wrapped.argv]
        child_env = self._build_env(run_id, run_dir, out_abs, env or {})
        try:
            try:
                proc = subprocess.Popen(
                    sup,
                    cwd=str(self.root),
                    env=child_env,
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    start_new_session=True,
                    pass_fds=(exit_w,),
                )
            finally:
                os.close(exit_w)
        except OSError as exc:
            os.close(exit_r)
            exit_r = None
            meta["status"] = "failed"
            meta["finished_at"] = utc_now_iso()
            write_atomic(run_dir / "meta.json", json.dumps(meta, indent=2) + "\n")
            transition(run_dir, "failed")
            (run_dir / "stderr.log").write_text(f"fibration: could not launch supervisor: {exc}\n")
            proc = None
        else:
            meta["pid"] = proc.pid
            meta["pid_start"] = proc_start_token(proc.pid, allow_zombie=True)
            write_atomic(run_dir / "meta.json", json.dumps(meta, indent=2) + "\n")
        watcher = _Watcher(self, run_id, run_dir, out_abs, proc, timeout, exit_fd=exit_r)
        with self._lock:
            self._watchers[run_id] = watcher
        watcher.start()
        return {"run_id": run_id, "out_dir": out_rel}

    # ---- run.status / runs.list ---------------------------------------------
    def status(self, run_id: str) -> dict[str, Any]:
        run_dir = self.run_dir(run_id)
        self._reconcile(run_id, run_dir)
        return read_meta(run_dir)

    def _reconcile(self, run_id: str, run_dir: Path) -> None:
        """A run whose supervisor is gone is failed, not running (pid plus start time)."""
        with self._lock:
            watched = run_id in self._watchers
        if not watched:  # a live watcher owns this decision and announces run.done
            reconcile(run_dir)

    def list(self, limit: int | None = None) -> list[dict[str, Any]]:
        limit = int(limit) if limit is not None else 50
        out: list[dict[str, Any]] = []
        try:
            names = sorted((p.name for p in self.runs_dir.iterdir() if p.is_dir()), reverse=True)
        except OSError:
            names = []
        for name in names:
            if len(out) >= limit:
                break
            if name.startswith("_") or not (self.runs_dir / name / "meta.json").is_file():
                continue
            try:
                self._reconcile(name, self.runs_dir / name)
                out.append(read_meta(self.runs_dir / name))
            except RpcError:
                continue
        return out

    # ---- run.cancel ------------------------------------------------------------
    def cancel(self, run_id: str) -> bool:
        run_dir = self.run_dir(run_id)
        line = read_status_line(run_dir)
        if line != "running":
            return False
        pid, token, _ = read_identity(run_dir)
        if pid is None or not same_process(pid, token):
            # Never signal a pid that is not provably this run's supervisor: the number may
            # belong to an unrelated process group by now.
            reconcile(run_dir)
            return False
        if transition(run_dir, "cancelled") != "cancelled":
            return False  # the run finished first
        _signal_group(pid, signal.SIGTERM)
        threading.Thread(target=_kill_later, args=(pid, token, 5.0), daemon=True, name=f"cancel-{run_id}").start()
        return True

    # ---- run.attach -------------------------------------------------------------
    def attach(self, run_id: str, stdout_offset: int = 0, stderr_offset: int = 0) -> Deferred:
        run_dir = self.run_dir(run_id)
        self._reconcile(run_id, run_dir)
        meta = read_meta(run_dir)
        out_abs = resolve_in_root(self.root, meta["out_dir"])
        with self._lock:
            watcher = self._watchers.get(run_id)
            if watcher is None and meta["status"] == "running":
                timeout = _launch_timeout(run_dir, self.config.timeout_s)
                # Not started here: the adopted watcher begins tailing only after the replay
                # below has been written, so nothing is delivered twice or out of order.
                watcher = _Watcher(self, run_id, run_dir, out_abs, None, timeout, adopt=True)
                self._watchers[run_id] = watcher
        result = {"status": meta["status"], "exit_code": meta["exit_code"]}

        def replay() -> None:
            self._replay(run_id, run_dir, out_abs, meta, watcher, stdout_offset, stderr_offset)

        return Deferred(result, replay)

    def _replay(
        self,
        run_id: str,
        run_dir: Path,
        out_abs: Path,
        meta: dict[str, Any],
        watcher: _Watcher | None,
        stdout_offset: int,
        stderr_offset: int,
    ) -> None:
        if watcher is not None:
            # Replay only what the live watcher has already emitted; it continues from there.
            with watcher.lock:
                _replay_log(self.notify, run_id, "run.stdout", run_dir / "stdout.log", stdout_offset, watcher.offsets["stdout"])
                _replay_log(self.notify, run_id, "run.stderr", run_dir / "stderr.log", stderr_offset, watcher.offsets["stderr"])
                for art in watcher.known_artifacts():
                    self.notify("run.artifact", {"run_id": run_id, "artifact": art})
            watcher.start_once()
            return
        # Finished run with no watcher: make sure late artifacts are on record, then replay.
        scanner = _ArtifactScanner(self.root, run_dir, out_abs)
        scanner.load_recorded()
        scanner.record_all(scanner.scan(final=True))
        _replay_log(self.notify, run_id, "run.stdout", run_dir / "stdout.log", stdout_offset, None)
        _replay_log(self.notify, run_id, "run.stderr", run_dir / "stderr.log", stderr_offset, None)
        for art in scanner.known():
            self.notify("run.artifact", {"run_id": run_id, "artifact": art})
        if meta["status"] != "running":
            self.notify(
                "run.done",
                {
                    "run_id": run_id,
                    "status": meta["status"],
                    "exit_code": meta["exit_code"],
                    "duration_s": _duration(meta),
                },
            )

    def _watcher_done(self, run_id: str) -> None:
        with self._lock:
            self._watchers.pop(run_id, None)

    def shutdown(self) -> None:
        with self._lock:
            watchers = list(self._watchers.values())
        for w in watchers:
            w.stop.set()
        for w in watchers:
            if w.is_alive():
                w.join(timeout=2.0)


def _launch_timeout(run_dir: Path, default: float) -> float:
    try:
        return float(json.loads((run_dir / "launch.json").read_text())["timeout_s"])
    except (OSError, ValueError, KeyError, TypeError):
        return float(default)


def _duration(meta: dict[str, Any]) -> float:
    start = parse_iso(meta.get("started_at"))
    end = parse_iso(meta.get("finished_at"))
    if start is None:
        return 0.0
    if end is None:
        end = time.time()
    return round(max(0.0, end - start), 3)


def _signal_group(pid: int, sig: int) -> None:
    try:
        os.killpg(pid, sig)
    except ProcessLookupError:
        pass
    except OSError:
        try:
            os.kill(pid, sig)
        except OSError:
            pass


def _kill_later(pid: int, token: str | None, delay: float) -> None:
    """SIGKILL the run's process group if it is still there after ``delay`` seconds."""
    deadline = time.monotonic() + delay
    while time.monotonic() < deadline:
        time.sleep(0.2)
        try:
            os.killpg(pid, 0)
        except OSError:
            return
    # The group leader is the supervisor; once it has exited its pid (and so the group id)
    # may be reused, so only a group still led by the same process is killed.
    if same_process(pid, token):
        _signal_group(pid, signal.SIGKILL)


def _replay_log(
    notify: Notify, run_id: str, method: str, path: Path, start: int, end: int | None
) -> None:
    """Replay ``[start, end)`` of a log (to EOF when ``end`` is None) in chunks of at most 64 KiB."""
    offset = max(0, int(start))
    try:
        with path.open("rb") as fh:
            if end is None:
                end = os.fstat(fh.fileno()).st_size
            while offset < end:
                fh.seek(offset)
                data = fh.read(min(CHUNK_BYTES, end - offset))
                if not data:
                    return
                last = offset + len(data) >= end
                cut = len(data) if last else (utf8_cut(data) or len(data))
                chunk = data[:cut].decode("utf-8", errors="replace")
                notify(method, {"run_id": run_id, "chunk": chunk, "offset": offset})
                offset += cut
    except OSError:
        return


class _ArtifactScanner:
    """Discover artifacts under a run's output directory.

    While the run is live a file is announced once its size is non-zero and unchanged between
    two scans. The final scan, run once the moment the process has exited, needs no such
    wait: every file is looked at again, new files are announced at once, and a file whose
    size, mtime, or sidecar changed since it was announced is announced again, so the last
    record for a path is always the final state.
    """

    def __init__(self, root: Path, run_dir: Path, out_abs: Path) -> None:
        self.root = root
        self.run_dir = run_dir
        self.out_abs = out_abs
        self.sizes: dict[Path, int] = {}
        self.sidecar_waits: dict[Path, int] = {}
        self.announced: dict[str, dict[str, Any]] = {}  # path -> latest record, in announce order

    @property
    def emitted(self) -> set[str]:
        return set(self.announced)

    @property
    def recorded(self) -> list[dict[str, Any]]:
        return list(self.announced.values())

    def known(self) -> list[dict[str, Any]]:
        return list(self.announced.values())

    def load_recorded(self) -> None:
        try:
            lines = (self.run_dir / "artifacts.jsonl").read_text(encoding="utf-8").splitlines()
        except OSError:
            return
        for line in lines:
            try:
                art = json.loads(line)
                self.announced[art["path"]] = art
            except (ValueError, KeyError, TypeError):
                continue

    def record(self, art: dict[str, Any]) -> None:
        """Append one record (live discovery)."""
        self.announced[art["path"]] = art
        with (self.run_dir / "artifacts.jsonl").open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(art, separators=(",", ":")) + "\n")

    def record_all(self, arts: list[dict[str, Any]]) -> None:
        """Record the result of a final scan: the file ends up with one final record per path."""
        if not arts:
            return
        for art in arts:
            self.announced[art["path"]] = art
        text = "".join(json.dumps(a, separators=(",", ":")) + "\n" for a in self.announced.values())
        try:
            write_atomic(self.run_dir / "artifacts.jsonl", text)
        except OSError:
            log.exception("could not rewrite artifacts.jsonl in %s", self.run_dir)

    def _files(self) -> list[tuple[Path, os.DirEntry[str]]]:
        """Regular files under the output directory (no symlinks), sorted by path.

        ``scandir`` reports the file type without a ``stat`` per entry, so a live scan of a
        directory whose files are all announced already costs one ``readdir`` per directory.
        """
        out: list[tuple[Path, os.DirEntry[str]]] = []
        stack = [self.out_abs]
        while stack:
            directory = stack.pop()
            try:
                with os.scandir(directory) as it:
                    entries = list(it)
            except OSError:
                continue
            for entry in entries:
                try:
                    if entry.is_symlink():
                        continue
                    if entry.is_dir(follow_symlinks=False):
                        stack.append(Path(entry.path))
                    elif entry.is_file(follow_symlinks=False):
                        out.append((Path(entry.path), entry))
                except OSError:
                    continue
        out.sort(key=lambda pair: pair[0])
        return out

    def scan(self, final: bool = False) -> list[dict[str, Any]]:
        found: list[dict[str, Any]] = []
        for file, entry in self._files():
            if file.name.endswith(".tmp"):
                continue
            if file.parent == self.out_abs and file.name == "run.json":
                continue
            rel = rel_to_root(self.root, file)
            previous = self.announced.get(rel)
            if previous is not None and not final:
                continue
            target = sidecar_target(file)
            # ``x.png.meta.json``: a sidecar when it describes an existing figure. Anything
            # else under that name is the user's own JSON and is listed once the run ends.
            if target is not None and (not final or (target.is_file() and read_sidecar(target) is not None)):
                continue
            try:
                st = entry.stat(follow_symlinks=False)
            except OSError:
                continue
            kind = artifact_kind(file)
            if file.parent == self.out_abs and file.name == RESULTS_FILE and is_results_file(file):
                kind = "results"
            if previous is None and not final:
                prev_size = self.sizes.get(file)
                self.sizes[file] = st.st_size
                if prev_size is None or prev_size != st.st_size or st.st_size == 0:
                    continue  # still growing, or opened but not written yet
            sidecar = read_sidecar(file) if kind == "figure" else None
            if kind == "figure" and sidecar is None and not final and previous is None:
                waits = self.sidecar_waits.get(file, 0) + 1
                self.sidecar_waits[file] = waits
                if waits <= 2:
                    continue  # the sidecar is written right after the image
            art: dict[str, Any] = {
                "kind": kind,
                "path": rel,
                "size": st.st_size,
                "created_at": mtime_iso(st.st_mtime),
                "rendered": kind in RENDERED_KINDS,
            }
            if sidecar is not None:
                art["sidecar"] = sidecar
            if previous == art:
                continue
            self.announced[rel] = art
            found.append(art)
        return found


class _Watcher(threading.Thread):
    """Per-run tailer: logs, artifacts, completion, and the timeout backstop.

    A run started by this kernel has ``exit_fd``, the read end of the pipe its supervisor
    closes when the result is recorded; the watcher sleeps on it between tails and wakes the
    instant the run ends. An adopted run (attach after a kernel restart) has no pipe and is
    polled through its status file instead.
    """

    def __init__(
        self,
        manager: RunManager,
        run_id: str,
        run_dir: Path,
        out_abs: Path,
        proc: subprocess.Popen | None,
        timeout_s: float,
        adopt: bool = False,
        exit_fd: int | None = None,
    ) -> None:
        super().__init__(name=f"watch-{run_id}", daemon=True)
        self.manager = manager
        self.run_id = run_id
        self.run_dir = run_dir
        self.out_abs = out_abs
        self.proc = proc
        self.exit_fd = exit_fd
        self.timeout_s = timeout_s
        self.stop = threading.Event()
        self.lock = threading.Lock()
        self._begun = False
        self._log_fds: dict[str, int] = {}
        self.scanner = _ArtifactScanner(manager.root, run_dir, out_abs)
        self.offsets = {"stdout": 0, "stderr": 0}
        if adopt:
            # Attach after a restart: replay covers the past; continue from the current end.
            self.scanner.load_recorded()
            for name in ("stdout", "stderr"):
                try:
                    self.offsets[name] = (run_dir / f"{name}.log").stat().st_size
                except OSError:
                    self.offsets[name] = 0

    def start_once(self) -> None:
        with self.lock:
            if self._begun:
                return
            self._begun = True
        self.start()

    def start(self) -> None:
        with self.lock:
            self._begun = True
        super().start()

    def known_artifacts(self) -> list[dict[str, Any]]:
        return self.scanner.known()

    def _notify(self, method: str, params: dict[str, Any]) -> None:
        try:
            self.manager.notify(method, params)
        except Exception:
            log.exception("notification %s failed", method)

    def _log_fd(self, name: str) -> int | None:
        """The log's descriptor, opened once and kept: a quiet tail is then one ``pread``."""
        fd = self._log_fds.get(name)
        if fd is None:
            try:
                fd = os.open(self.run_dir / f"{name}.log", os.O_RDONLY)
            except OSError:
                return None
            self._log_fds[name] = fd
        return fd

    def _close_fds(self) -> None:
        fds = list(self._log_fds.values())
        self._log_fds.clear()
        if self.exit_fd is not None:
            fds.append(self.exit_fd)
            self.exit_fd = None
        for fd in fds:
            try:
                os.close(fd)
            except OSError:
                pass

    def _tail(self, name: str, final: bool = False) -> bool:
        """Emit new log output in chunks of at most 64 KiB; ``final`` flushes everything.

        Returns True when anything was emitted.
        """
        fd = self._log_fd(name)
        if fd is None:
            return False
        sent = 0
        while final or sent < MAX_CHUNKS_PER_POLL:
            offset = self.offsets[name]
            try:
                data = os.pread(fd, CHUNK_BYTES, offset)
            except OSError:
                break
            if not data:
                break
            full = len(data) == CHUNK_BYTES
            # A sequence cut by the chunk boundary or by the writer waits for its tail;
            # at the end of the run whatever is left is flushed with replacement.
            cut = len(data) if (final and not full) else utf8_cut(data)
            if cut <= 0:
                break
            chunk = data[:cut].decode("utf-8", errors="replace")
            self.offsets[name] = offset + cut
            self._notify(f"run.{name}", {"run_id": self.run_id, "chunk": chunk, "offset": offset})
            sent += 1
            if not full and not final:
                break
        return sent > 0

    def _artifacts(self, final: bool) -> None:
        arts = self.scanner.scan(final=final)
        if final:
            self.scanner.record_all(arts)
        for art in arts:
            if not final:
                self.scanner.record(art)
            self._notify("run.artifact", {"run_id": self.run_id, "artifact": art})

    def _supervisor_alive(self) -> bool:
        if self.proc is not None:
            return self.proc.poll() is None
        return supervisor_alive(self.run_dir)

    def _wait_exit(self, interval: float) -> bool:
        """Sleep up to ``interval``; True as soon as the run is over.

        With the exit pipe this returns the moment the supervisor closes it (result recorded)
        or dies (the kernel then records the failure). Without one the status file and the
        supervisor's identity are polled.
        """
        if self.exit_fd is not None:
            try:
                ready, _, _ = select.select([self.exit_fd], [], [], interval)
            except (OSError, ValueError):
                ready = [self.exit_fd]
            # Belt and braces: a supervisor that is gone has ended the run even if some other
            # process were to hold the pipe open.
            return bool(ready) or (self.proc is not None and self.proc.poll() is not None)
        if read_status_line(self.run_dir) != "running":
            return True
        if not self._supervisor_alive():
            time.sleep(0.05)  # it may have been between writing its result and exiting
            return True
        self.stop.wait(interval)
        return False

    def run(self) -> None:
        t0 = time.monotonic()
        started = parse_iso(_read_meta_field(self.run_dir, "started_at")) or time.time()
        signalled = self.exit_fd is not None
        idle = max(self.manager.poll_s, 0.001)
        busy = min(self.manager.tail_min_s, idle)
        interval = busy
        next_scan = t0 + idle
        try:
            while not self.stop.is_set():
                with self.lock:
                    moved = self._tail("stdout")
                    moved = self._tail("stderr") or moved
                    now = time.monotonic()
                    if now >= next_scan:
                        self._artifacts(final=False)
                        next_scan = now + idle
                # Output usually comes in bursts: tail quickly while it flows, back off when quiet.
                interval = busy if moved else min(interval * TAIL_BACKOFF, idle)
                if self._wait_exit(interval):
                    break
                if self.timeout_s and time.time() - started > self.timeout_s + 15.0:
                    # Backstop: the supervisor should have done this already.
                    pid, token, _ = read_identity(self.run_dir)
                    if transition(self.run_dir, "timeout") == "timeout" and pid and same_process(pid, token):
                        _signal_group(pid, signal.SIGKILL)
                    break
            if self.stop.is_set():
                return
            if read_status_line(self.run_dir) == "running":
                # Supervisor vanished without recording a result.
                _finish_failed(self.run_dir, "fibration: the run's supervisor process is gone; the run is marked failed\n")
            elif not signalled:
                # Polled run: let the supervisor finish writing meta.json before the final flush.
                # (A signalled run's supervisor closes the pipe only after meta.json is written.)
                deadline = time.monotonic() + 2.0
                while time.monotonic() < deadline:
                    if not self._supervisor_alive() or _read_meta_field(self.run_dir, "finished_at"):
                        break
                    time.sleep(0.01)
            # The process has exited: one final flush and one final scan, no stability wait.
            with self.lock:
                self._tail("stdout", final=True)
                self._tail("stderr", final=True)
                self._artifacts(final=True)
            meta = read_meta(self.run_dir)
            if meta.get("finished_at") is None:
                _set_finished(self.run_dir, meta)
            duration = _duration(meta) if meta.get("finished_at") else round(time.monotonic() - t0, 3)
            self._notify(
                "run.done",
                {
                    "run_id": self.run_id,
                    "status": meta["status"],
                    "exit_code": meta["exit_code"],
                    "duration_s": duration,
                },
            )
        except Exception:
            log.exception("watcher for %s crashed", self.run_id)
        finally:
            self._close_fds()
            self.manager._watcher_done(self.run_id)
            if self.proc is not None and not self.stop.is_set():
                try:  # reap the supervisor; it exits right after closing the pipe
                    self.proc.wait(timeout=10)
                except (subprocess.TimeoutExpired, OSError):
                    pass


def _read_meta_field(run_dir: Path, key: str) -> Any:
    try:
        return json.loads((run_dir / "meta.json").read_text(encoding="utf-8")).get(key)
    except (OSError, ValueError):
        return None


def _finish_failed(run_dir: Path, note: str | None = None) -> None:
    if transition(run_dir, "failed") != "failed":
        return  # a real result was recorded in the meantime
    if note:
        try:
            with (run_dir / "stderr.log").open("a", encoding="utf-8") as fh:
                fh.write(note)
        except OSError:
            pass
    try:
        meta = json.loads((run_dir / "meta.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return
    meta["status"] = "failed"
    meta["exit_code"] = None
    meta["finished_at"] = meta.get("finished_at") or utc_now_iso()
    write_atomic(run_dir / "meta.json", json.dumps(meta, indent=2) + "\n")


def _set_finished(run_dir: Path, meta: dict[str, Any]) -> None:
    meta["finished_at"] = utc_now_iso()
    try:
        full = json.loads((run_dir / "meta.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        full = {}
    full.update({k: meta[k] for k in ("status", "exit_code", "finished_at")})
    write_atomic(run_dir / "meta.json", json.dumps(full, indent=2) + "\n")


def validate_run_params(params: dict[str, Any]) -> None:
    rid = params.get("run_id")
    if not isinstance(rid, str) or not rid:
        raise invalid_params("run_id must be a non-empty string")
