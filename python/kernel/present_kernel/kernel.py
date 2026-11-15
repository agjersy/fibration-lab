"""The kernel: wires every RPC method onto the modules. Shared by the stdio server and tests."""

from __future__ import annotations

import logging
import sys
import tomllib
from collections.abc import Callable
from pathlib import Path
from typing import Any

from present_sdk.template import describe_template

from . import PROTOCOL_VERSION, SCHEMA_VERSION, __version__
from .errors import TEMPLATE_UNREADABLE, RpcError, invalid_params
from .paths import resolve_in_root
from .rpc import Dispatcher

log = logging.getLogger("present_kernel")

Notify = Callable[[str, dict[str, Any]], None]

FIXED_PACKAGES = (
    "python-pptx", "lxml", "Pillow", "openpyxl", "python-docx", "pypdfium2", "numpy", "pandas", "matplotlib",
    "jsonschema", "uharfbuzz", "olefile", "beautifulsoup4", "onnxruntime", "tokenizers", "zstandard",
)  # fmt: skip


def host_os() -> str:
    return "windows" if sys.platform == "win32" else "macos" if sys.platform == "darwin" else "linux"


def _req(params: dict[str, Any], key: str, types: tuple[type, ...] = (str,)) -> Any:
    value = params.get(key)
    if value is None or not isinstance(value, types) or (isinstance(value, bool) and bool not in types):
        raise invalid_params(f"{key} is required")
    return value


def _opt(params: dict[str, Any], key: str, types: tuple[type, ...], default: Any = None) -> Any:
    value = params.get(key)
    if value is None:
        return default
    if not isinstance(value, types) or (isinstance(value, bool) and bool not in types):
        raise invalid_params(f"{key} has the wrong type")
    return value


class Kernel:
    def __init__(self, project: Path, home: Path, engine: str = "auto", sandbox: str = "auto",
                 notify: Notify | None = None) -> None:
        self.project = Path(project).resolve()
        self.home = Path(home).resolve()
        self.engine_pref = engine
        self.sandbox_pref = sandbox
        self._notify: Notify = notify or (lambda m, p: None)
        self.dispatcher = Dispatcher()
        self._register()

    def set_notify(self, notify: Notify) -> None:
        self._notify = notify

    def notify(self, method: str, params: dict[str, Any]) -> None:
        self._notify(method, params)

    def progress(self, request_id: Any, message: str, fraction: float | None = None) -> None:
        payload: dict[str, Any] = {"request_id": request_id, "message": message}
        if fraction is not None:
            payload["fraction"] = fraction
        self.notify("task.progress", payload)

    def close(self) -> None:
        for closer in getattr(self, "_closers", []):
            try:
                closer()
            except Exception:  # pragma: no cover - best effort on shutdown
                log.exception("closer failed")

    def path(self, rel: str) -> Path:
        return resolve_in_root(self.project, rel)

    # ---- registration ---------------------------------------------------------------------
    def _register(self) -> None:
        d = self.dispatcher
        d.register("kernel.hello", self.hello)
        d.register("kernel.shutdown", lambda p: None)
        d.register("template.inspect", self.template_inspect)
        d.register("template.sidecar", self.template_sidecar)
        d.register("deck.validate", self.deck_validate)
        d.register("deck.render", self.deck_render)
        from . import methods

        methods.register_all(self)

    # ---- lifecycle ------------------------------------------------------------------------
    def hello(self, params: dict[str, Any]) -> dict[str, Any]:
        from importlib import metadata

        if params.get("protocol_version") != PROTOCOL_VERSION:
            raise invalid_params(f"protocol_version must be {PROTOCOL_VERSION}")
        packages = {}
        for name in FIXED_PACKAGES:
            try:
                packages[name] = metadata.version(name)
            except metadata.PackageNotFoundError:
                packages[name] = "missing"
        from . import methods

        return {
            "kernel_version": __version__,
            "protocol_version": PROTOCOL_VERSION,
            "schema_version": SCHEMA_VERSION,
            "os": host_os(),
            "python": sys.version.split()[0],
            "packages": packages,
            **methods.hello_info(self),
        }

    # ---- templates ------------------------------------------------------------------------
    def _sidecar(self, template_rel: str) -> dict[str, Any] | None:
        side = self.path(template_rel).parent / "template.toml"
        if not side.is_file():
            return None
        return tomllib.loads(side.read_text(encoding="utf-8"))

    def template_inspect(self, params: dict[str, Any]) -> dict[str, Any]:
        rel = _req(params, "path")
        full = self.path(rel)
        if not full.is_file():
            raise RpcError(TEMPLATE_UNREADABLE, f"{rel} not found")
        try:
            return describe_template(full, rel, self._sidecar(rel))
        except RpcError:
            raise
        except Exception as exc:
            raise RpcError(TEMPLATE_UNREADABLE, f"{rel}: {type(exc).__name__}: {exc}") from exc

    def template_sidecar(self, params: dict[str, Any]) -> dict[str, Any] | None:
        return self._sidecar(_req(params, "path"))

    # ---- deck -----------------------------------------------------------------------------
    def deck_validate(self, params: dict[str, Any]) -> list[dict[str, Any]]:
        from .validate import validate_deck

        return validate_deck(self.project, _opt(params, "deck", (str,), "deck/deck.json"))

    def deck_render(self, params: dict[str, Any]) -> dict[str, Any]:
        from .render import render_deck

        slides = params.get("slides")
        if slides is not None and (not isinstance(slides, list) or not all(isinstance(s, str) for s in slides)):
            raise invalid_params("slides must be a list of slide ids")
        return render_deck(self.project, _req(params, "out"), _opt(params, "snapshot_id", (str,), ""), slides,
                           generated=getattr(self, "run_generated_slide", None))  # fmt: skip
