"""The engine's JSON-RPC server (ARCHITECTURE 5.1).

Framing is the Analyst kernel's: newline-delimited JSON-RPC 2.0, one object per line, UTF-8,
requests on stdin and responses on stdout, logs on stderr only. Unlike the kernel, the engine
serves one request at a time on its main thread. Scene state allows one call sequence at a
time anyway, and the main thread is where call budgets can interrupt SymPy.

Methods: ``engine.hello``, ``engine.shutdown``, ``call``, ``describe_api``, ``state.get``,
``state.snapshot``, ``state.restore``, ``state.graph``, ``reset``, ``sample``, ``export.render``, and ``code.glsl``.
"""

from __future__ import annotations

import importlib
import json
import logging
import sys
from collections.abc import Callable
from typing import Any, BinaryIO

import sympy as sp

from . import __version__
from .engine import Engine
from .errors import INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR, RpcError
from .registry import API_VERSION

PROTOCOL_VERSION = 1
log = logging.getLogger("fibration_math_engine")


def encode(obj: dict[str, Any]) -> bytes:
    return (json.dumps(obj, ensure_ascii=True, separators=(",", ":"), allow_nan=False) + "\n").encode("utf-8")


class Methods:
    def __init__(self, engine: Engine):
        self.engine = engine
        self.greeted = False
        self.table: dict[str, Callable[[dict[str, Any]], Any]] = {
            "engine.hello": self.hello,
            "engine.shutdown": lambda p: {"ok": True},
            "call": self.call,
            "describe_api": lambda p: engine.describe_api(bool(p.get("state", False))),
            "state.get": lambda p: engine.state(),
            "state.snapshot": lambda p: {"revision": engine.scene.revision, "hash": engine.scene.hash()},
            "state.restore": self.restore,
            "state.graph": lambda p: engine.scene.graph(),
            "reset": lambda p: engine.reset(),
        }

    def register(self, name: str, fn: Callable[[dict[str, Any]], Any]) -> None:
        self.table[name] = fn

    def hello(self, params: dict[str, Any]) -> dict[str, Any]:
        if params.get("protocol_version") != PROTOCOL_VERSION:
            raise RpcError(INVALID_PARAMS, f"unsupported protocol_version {params.get('protocol_version')!r}")
        self.greeted = True
        return {
            "engine_version": __version__,
            "protocol_version": PROTOCOL_VERSION,
            "api_version": API_VERSION,
            "sympy": sp.__version__,
        }

    def call(self, params: dict[str, Any]) -> dict[str, Any]:
        fn = params.get("fn")
        if not isinstance(fn, str):
            raise RpcError(INVALID_PARAMS, "call needs fn: string")
        return self.engine.call(
            fn, params.get("args") or {}, params.get("origin"), bool(params.get("memo", True))
        )

    def restore(self, params: dict[str, Any]) -> dict[str, Any]:
        from .errors import CallError

        revision = params.get("revision")
        if not isinstance(revision, int) or isinstance(revision, bool):
            raise RpcError(INVALID_PARAMS, "state.restore needs revision: integer")
        try:
            return self.engine.restore(revision)
        except CallError as exc:
            raise RpcError(INVALID_PARAMS, exc.detail) from None

    def dispatch(self, method: str, params: Any) -> Any:
        if method != "engine.hello" and not self.greeted:
            raise RpcError(INVALID_REQUEST, "the first request must be engine.hello")
        handler = self.table.get(method)
        if handler is None:
            raise RpcError(METHOD_NOT_FOUND, f"unknown method {method!r}")
        if params is None:
            params = {}
        if not isinstance(params, dict):
            raise RpcError(INVALID_PARAMS, "params must be an object")
        return handler(params)


def handle_line(methods: Methods, line: bytes) -> tuple[dict[str, Any] | None, bool]:
    """One request line to (response or None, stop?)."""
    try:
        msg = json.loads(line)
    except ValueError:
        return {
            "jsonrpc": "2.0",
            "id": None,
            "error": RpcError(PARSE_ERROR, "not valid JSON").to_json(),
        }, False
    if not isinstance(msg, dict) or not isinstance(msg.get("method"), str):
        return {
            "jsonrpc": "2.0",
            "id": None,
            "error": RpcError(INVALID_REQUEST, "not a request").to_json(),
        }, False
    rid = msg.get("id")
    method = msg["method"]
    try:
        result = methods.dispatch(method, msg.get("params"))
        response = {"jsonrpc": "2.0", "id": rid, "result": result}
    except RpcError as exc:
        response = {"jsonrpc": "2.0", "id": rid, "error": exc.to_json()}
    except Exception as exc:  # a bug in the engine; report it rather than die
        log.exception("internal error in %s", method)
        response = {"jsonrpc": "2.0", "id": rid, "error": RpcError(INTERNAL_ERROR, repr(exc)).to_json()}
    if rid is None:
        response = None
    return response, method == "engine.shutdown"


def serve(engine: Engine, stdin: BinaryIO | None = None, stdout: BinaryIO | None = None) -> int:
    inp = stdin if stdin is not None else sys.stdin.buffer
    out = stdout if stdout is not None else sys.stdout.buffer
    methods = Methods(engine)
    # Methods that live in their own modules: certified sampling, static export, shader code.
    for module in ("sample", "export", "glsl"):
        try:
            register = importlib.import_module(f".{module}", __package__).register
        except ImportError as exc:
            log.warning("engine method module %s is unavailable: %s", module, exc)
        else:
            register(methods)
    for raw in inp:
        if not raw.strip():
            continue
        response, stop = handle_line(methods, raw)
        if response is not None:
            try:
                data = encode(response)
            except ValueError:
                data = encode(
                    {
                        "jsonrpc": "2.0",
                        "id": response.get("id"),
                        "error": RpcError(INTERNAL_ERROR, "result is not JSON").to_json(),
                    }
                )
            try:
                out.write(data)
                out.flush()
            except (BrokenPipeError, OSError):
                return 0
        if stop:
            return 0
    return 0
