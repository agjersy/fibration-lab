"""The kernel: wires every RPC method onto the modules. Shared by the stdio server and tests."""

from __future__ import annotations

import logging
import socket
import sys
import threading
from collections.abc import Callable
from pathlib import Path
from typing import Any

from . import PROTOCOL_VERSION, __version__
from .config import ConfigError, ProjectConfig, load_config, project_data_dirs, validate_out_dir
from .errors import RpcError, invalid_params
from .paths import resolve_in_root
from .rpc import Dispatcher
from .runs import RunManager
from .sandbox import SandboxSpec, detect, host_os, tooling_dir, venv_of

log = logging.getLogger("analyst_kernel")

Notify = Callable[[str, dict[str, Any]], None]


def _opt(params: dict[str, Any], key: str, types: tuple[type, ...], default: Any = None) -> Any:
    value = params.get(key, default)
    if value is None:
        return default
    if isinstance(value, bool) and bool not in types:
        raise invalid_params(f"{key} has the wrong type")
    if not isinstance(value, types):
        raise invalid_params(f"{key} has the wrong type")
    return value


def _str_list(params: dict[str, Any], key: str) -> list[str] | None:
    value = params.get(key)
    if value is None:
        return None
    if not isinstance(value, list) or not all(isinstance(v, str) for v in value):
        raise invalid_params(f"{key} must be a list of strings")
    return value


class Kernel:
    def __init__(
        self,
        python: str,
        root: Path,
        runs_dir: Path,
        out_dir: str | None = None,
        sandbox: str = "auto",
        notify: Notify | None = None,
        data_dirs: list[str] | None = None,
        prewarm: bool = True,
    ) -> None:
        self.python = python
        self.prewarm = prewarm
        self._prewarmed = False
        self._packages_cache: tuple[Any, list[dict[str, Any]]] | None = None
        self._packages_lock = threading.Lock()
        self.root = Path(root).resolve()
        self.runs_dir = Path(runs_dir).resolve()
        self.config: ProjectConfig = load_config(self.root)
        # Raises ConfigError (exit code 2) unless out_dir is a relative path inside the root.
        self.out_dir_name = validate_out_dir(self.root, out_dir or self.config.out_dir)
        self.config.out_dir = self.out_dir_name
        # Data directories: the repo's own config may only name directories inside the repo;
        # anything outside comes from --data-dir, which the user approved in the global config.
        self.warnings: list[str] = []
        inside, self.warnings = project_data_dirs(self.root, self.config.data_dirs)
        for name, target in self.config.targets.items():
            entries = target.get("data_dirs")
            if isinstance(entries, list):
                kept, warned = project_data_dirs(self.root, [str(e) for e in entries])
                target["data_dirs"] = [str(k) for k in kept]
                self.warnings += [f"[targets.{name}] {w}" for w in warned]
        external: list[Path] = []
        for d in data_dirs or []:
            p = Path(d)
            if not p.is_absolute():
                raise ConfigError(f"--data-dir {d!r} must be an absolute path")
            external.append(p)
        self.data_dirs: list[Path] = inside + external
        for w in self.warnings:
            log.warning("%s", w)
        self.sandbox = detect(sandbox)
        self.host = socket.gethostname()
        self.os_name = host_os()
        self._notify: Notify = notify or (lambda m, p: None)
        self.mpl_dir = Path(__file__).resolve().parent.parent  # directory containing analyst_mpl/
        self.runs = RunManager(
            root=self.root,
            runs_dir=self.runs_dir,
            python=self.python,
            sandbox_kind=self.sandbox,
            config=self.config,
            notify=self.notify,
            mpl_dir=self.mpl_dir,
            host=self.host,
            out_dir_name=self.out_dir_name,
            data_dirs=self.data_dirs,
        )
        self._eq = None
        self._eq_lock = threading.Lock()
        self._closed = False
        self._snippets = None
        self._snippets_lock = threading.Lock()
        self.dispatcher = Dispatcher()
        self._register()

    # ---- plumbing ---------------------------------------------------------------
    def notify(self, method: str, params: dict[str, Any]) -> None:
        self._notify(method, params)

    def set_notify(self, fn: Notify) -> None:
        self._notify = fn

    def call(self, method: str, params: dict[str, Any] | None = None) -> Any:
        return self.dispatcher.call(method, params)

    def _register(self) -> None:
        reg = self.dispatcher.register
        reg("kernel.hello", self.m_hello)
        reg("kernel.shutdown", self.m_shutdown)
        reg("env.info", self.m_env_info)
        reg("env.packages", self.m_env_packages)
        reg("scripts.index", self.m_scripts_index)
        reg("lib.index", self.m_lib_index)
        reg("run.start", self.m_run_start)
        reg("run.attach", self.m_run_attach)
        reg("run.status", self.m_run_status)
        reg("run.cancel", self.m_run_cancel)
        reg("runs.list", self.m_runs_list)
        reg("exec.snippet", self.m_exec_snippet, lane="snippet")
        reg("out.list", self.m_out_list)
        reg("out.read", self.m_out_read)
        reg("eq.extract", self.m_eq_extract)
        reg("eq.check", self.m_eq_check)
        reg("contract.check", self.m_contract_check)
        reg("contract.similar", self.m_contract_similar)
        reg("lint.run", self.m_lint_run)

    def _tool_spec(self, name: str) -> SandboxSpec:
        """Sandbox spec for kernel-internal subprocesses (REPL, eq.check)."""
        run_dir = self.runs_dir / f"_{name}"
        run_dir.mkdir(parents=True, exist_ok=True)
        out_dir = self.root / self.out_dir_name / f"_{name}"
        out_dir.mkdir(parents=True, exist_ok=True)
        derived = self.root / "data" / "derived"
        return SandboxSpec(
            root=self.root,
            venv=venv_of(Path(self.python)),
            python=Path(self.python),
            run_dir=run_dir,
            out_dir=out_dir,
            data_dirs=list(self.data_dirs),
            support_dirs=[self.mpl_dir / "analyst_mpl"],
            tooling_dir=tooling_dir(),
            cache_dirs=[self.runs.mpl_config_dir],
            derived_dir=derived if derived.is_dir() else None,
            rlimit_as=self.config.rlimit_as(),
            rlimit_nofile=self.config.open_files,
        )

    def _tool_env(self, name: str) -> dict[str, str]:
        run_dir = self.runs_dir / f"_{name}"
        out_dir = self.root / self.out_dir_name / f"_{name}"
        env = self.runs._build_env(f"_{name}", run_dir, out_dir, {})
        (run_dir / "tmp").mkdir(parents=True, exist_ok=True)
        return env

    # ---- lifecycle ----------------------------------------------------------------
    def m_hello(self, params: dict[str, Any]) -> dict[str, Any]:
        pv = params.get("protocol_version")
        if pv is not None and pv != PROTOCOL_VERSION:
            raise invalid_params(f"protocol_version {pv!r} is not supported (kernel speaks {PROTOCOL_VERSION})")
        for w in self.warnings:
            log.warning("%s", w)
        self._prewarm_equations()
        return {
            "kernel_version": __version__,
            "protocol_version": PROTOCOL_VERSION,
            "host": self.host,
            "os": self.os_name,
            "sandbox": self.sandbox,
            "python": self.python,
            "root": str(self.root),
            "runs_dir": str(self.runs_dir),
        }

    def m_shutdown(self, params: dict[str, Any]) -> dict[str, Any]:
        self.close()
        return {"ok": True}

    def close(self) -> None:
        """Stop helpers. A running snippet or equation analysis is killed, not waited for."""
        self._closed = True
        with self._snippets_lock:
            snippets, self._snippets = self._snippets, None
        if snippets is not None:
            snippets.shutdown()
        with self._eq_lock:
            eq, self._eq = self._eq, None
        if eq is not None:
            eq.close()
        self.runs.shutdown()

    def _eq_worker(self):
        from .equations import EqWorker

        with self._eq_lock:
            if self._eq is None:
                self._eq = EqWorker()
            return self._eq

    def _prewarm_equations(self) -> None:
        """Start the equation worker (the SymPy import) in the background, once.

        Called from ``kernel.hello``; returns at once. The first ``eq.extract`` then finds a
        worker that is ready, or one that is already part-way through starting, instead of
        paying the whole import itself. Nothing else waits on it.
        """
        if not self.prewarm or self._prewarmed or self._closed:
            return
        self._prewarmed = True

        def warm() -> None:
            try:
                from .equations import EqWorker

                with self._eq_lock:  # close() sets _closed before it takes this lock
                    if self._closed:
                        return
                    if self._eq is None:
                        self._eq = EqWorker()
                    worker = self._eq
                worker.prewarm()
            except Exception:
                log.debug("equation worker prewarm failed", exc_info=True)

        threading.Thread(target=warm, name="eq-prewarm", daemon=True).start()

    # ---- environment -------------------------------------------------------------------
    def m_env_info(self, params: dict[str, Any]) -> dict[str, Any]:
        from .envinfo import env_info

        return env_info(self.python, self.sandbox, self.host, self.os_name)

    def m_env_packages(self, params: dict[str, Any]) -> dict[str, Any]:
        from .envinfo import env_packages, packages_stamp

        # The listing spawns ``uv``; its answer only changes when the lockfile, the project
        # file, or the environment's site-packages change, so it is cached on their mtimes.
        with self._packages_lock:
            stamp = packages_stamp(self.python, self.root)
            cached = self._packages_cache
            if cached is None or cached[0] != stamp:
                packages = env_packages(self.python, self.root)
                # Stamp taken before the listing: a change during it invalidates the entry.
                cached = self._packages_cache = (stamp, packages)
            return {"packages": [dict(p) for p in cached[1]]}

    # ---- indexes --------------------------------------------------------------------
    def m_scripts_index(self, params: dict[str, Any]) -> dict[str, Any]:
        from .index_scripts import index_scripts, public_script

        globs = _str_list(params, "globs") or self.config.scripts
        # error 1002 for a glob outside the root; .venv, .git and the output directory are skipped
        return {"scripts": [public_script(s) for s in index_scripts(self.root, globs, self.out_dir_name)]}

    def m_lib_index(self, params: dict[str, Any]) -> dict[str, Any]:
        from .index_lib import index_lib

        package = _opt(params, "package", (str,), self.config.library)
        resolve_in_root(self.root, package.replace(".", "/"))
        return {"functions": index_lib(self.root, package, self.config.scripts, self.out_dir_name)}

    # ---- runs -------------------------------------------------------------------------
    def m_run_start(self, params: dict[str, Any]) -> dict[str, Any]:
        script = _opt(params, "script", (str,))
        if not script:
            raise invalid_params("script is required")
        args = _str_list(params, "args") or []
        env = params.get("env") or {}
        if not isinstance(env, dict) or not all(isinstance(k, str) and isinstance(v, str) for k, v in env.items()):
            raise invalid_params("env must be an object of strings")
        timeout_s = _opt(params, "timeout_s", (int, float))
        if params.get("network"):
            raise invalid_params("runs never have network access; there is no `network` parameter")
        tree_hash = _opt(params, "tree_hash", (str,))
        return self.runs.start(script, args, env, timeout_s, tree_hash)

    def _run_id(self, params: dict[str, Any]) -> str:
        rid = params.get("run_id")
        if not isinstance(rid, str) or not rid:
            raise invalid_params("run_id must be a non-empty string")
        return rid

    def m_run_attach(self, params: dict[str, Any]):
        return self.runs.attach(
            self._run_id(params),
            int(_opt(params, "stdout_offset", (int,), 0)),
            int(_opt(params, "stderr_offset", (int,), 0)),
        )

    def m_run_status(self, params: dict[str, Any]) -> dict[str, Any]:
        return self.runs.status(self._run_id(params))

    def m_run_cancel(self, params: dict[str, Any]) -> dict[str, Any]:
        return {"ok": self.runs.cancel(self._run_id(params))}

    def m_runs_list(self, params: dict[str, Any]) -> dict[str, Any]:
        return {"runs": self.runs.list(_opt(params, "limit", (int,)))}

    # ---- snippets ---------------------------------------------------------------------
    def m_exec_snippet(self, params: dict[str, Any]) -> dict[str, Any]:
        from .snippet import SnippetRunner

        code = _opt(params, "code", (str,))
        if code is None:
            raise invalid_params("code is required")
        timeout_s = _opt(params, "timeout_s", (int, float))
        with self._snippets_lock:
            if self._closed:
                return {"ok": False, "stdout": "", "stderr": "REPL unavailable: the kernel is shutting down", "value_repr": None}
            if self._snippets is None:
                self._snippets = SnippetRunner(
                    self.python,
                    self.root,
                    self.sandbox,
                    lambda: self._tool_spec("repl"),
                    self._tool_env("repl"),
                )
            runner = self._snippets
        return runner.run(code, timeout_s)

    # ---- outputs ------------------------------------------------------------------------
    def m_out_list(self, params: dict[str, Any]) -> dict[str, Any]:
        from .outputs import out_list

        path = _opt(params, "path", (str,))
        run_id = _opt(params, "run_id", (str,))
        run_out = self.runs.status(run_id)["out_dir"] if run_id else None
        return {"entries": out_list(self.root, self.out_dir_name, path, run_out)}

    def m_out_read(self, params: dict[str, Any]) -> dict[str, Any]:
        from .outputs import out_read

        return out_read(self.root, self.python, _opt(params, "path", (str,), ""), _opt(params, "max_bytes", (int,)))

    # ---- equations ---------------------------------------------------------------------
    def m_eq_extract(self, params: dict[str, Any]) -> dict[str, Any]:
        from .equations import eq_extract

        return {
            "equations": eq_extract(
                self.root, _opt(params, "path", (str,), ""), _opt(params, "function", (str,)), self._eq_worker()
            )
        }

    def m_eq_check(self, params: dict[str, Any]) -> dict[str, Any]:
        from .equations import eq_check

        ranges = params.get("ranges")
        if ranges is not None and not isinstance(ranges, dict):
            raise invalid_params("ranges must be an object")
        return eq_check(
            self.root,
            self.python,
            self.sandbox,
            lambda: self._tool_spec("eqcheck"),
            self._tool_env("eqcheck"),
            _opt(params, "path", (str,), ""),
            _opt(params, "function", (str,), ""),
            _opt(params, "trials", (int,)),
            _opt(params, "tol", (int, float)),
            ranges,
            self._eq_worker(),
        )

    # ---- contract and lint -----------------------------------------------------------------
    def m_contract_check(self, params: dict[str, Any]) -> dict[str, Any]:
        from .contract import contract_check

        return {"diagnostics": contract_check(self.root, self.config, _str_list(params, "paths"))}

    def m_contract_similar(self, params: dict[str, Any]) -> dict[str, Any]:
        from .similarity import similar_pairs

        return {"pairs": similar_pairs(self.root, self.config, _opt(params, "threshold", (int, float)))}

    def m_lint_run(self, params: dict[str, Any]) -> dict[str, Any]:
        from .astutil import library_files, script_files
        from .lint import lint_run

        out = self.out_dir_name
        defaults = script_files(self.root, self.config.scripts, out) + library_files(self.root, self.config.library, out)
        return {"diagnostics": lint_run(self.root, _str_list(params, "paths"), defaults)}


__all__ = ["ConfigError", "Kernel", "RpcError", "sys"]
