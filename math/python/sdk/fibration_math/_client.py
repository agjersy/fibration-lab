"""Transport to the engine: the router's Unix socket, or a private headless engine.

Inside the app (cell runner, attached terminal) ``FIBRATION_MATH_SOCKET`` names the router's
socket, and every call goes through the core, which validates, logs, and forwards it.
Without it (``python scene.py`` on its own), the SDK starts a private engine process on
stdio, so a scene file is a self-contained tool. Both speak the engine's newline-delimited
JSON-RPC; only the ``call``, ``state.get`` and ``describe_api`` methods are used through the
router.
"""

from __future__ import annotations

import atexit
import importlib.util
import itertools
import json
import os
import socket
import subprocess
import sys
import threading
from typing import Any

from .errors import EngineUnavailable, ProtocolError

PROTOCOL_VERSION = 1


class _Transport:
    def request(self, method: str, params: dict[str, Any]) -> Any:
        raise NotImplementedError

    def close(self) -> None:
        pass


class _LineTransport(_Transport):
    def __init__(self, reader, writer):
        self._reader = reader
        self._writer = writer
        self._ids = itertools.count(1)
        self._lock = threading.Lock()

    def request(self, method: str, params: dict[str, Any]) -> Any:
        rid = next(self._ids)
        line = json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params}, separators=(",", ":"))
        with self._lock:
            try:
                self._writer.write((line + "\n").encode("utf-8"))
                self._writer.flush()
                raw = self._reader.readline()
            except OSError as exc:
                raise EngineUnavailable(f"lost the connection to the engine: {exc}") from None
        if not raw:
            raise EngineUnavailable("the engine closed the connection")
        msg = json.loads(raw)
        if msg.get("id") != rid:
            raise ProtocolError(f"response id {msg.get('id')!r} does not match request {rid}")
        if "error" in msg:
            err = msg["error"]
            detail = (err.get("data") or {}).get("detail") or err.get("message")
            raise ProtocolError(f"{method}: {detail}")
        return msg.get("result")


class SocketTransport(_LineTransport):
    def __init__(self, path: str):
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            sock.connect(path)
        except OSError as exc:
            raise EngineUnavailable(f"cannot connect to the Fibration Math socket {path}: {exc}") from None
        self._sock = sock
        super().__init__(sock.makefile("rb"), sock.makefile("wb"))

    def close(self) -> None:
        try:
            self._sock.close()
        except OSError:
            pass


class HeadlessTransport(_LineTransport):
    """A private engine process for this Python process."""

    def __init__(self):
        python = os.environ.get("FIBRATION_MATH_ENGINE_PYTHON")
        if not python:
            if importlib.util.find_spec("fibration_math_engine") is None:
                raise EngineUnavailable(
                    "no Fibration Math engine: run this scene from the app or with `fibration-math run`, "
                    "or set FIBRATION_MATH_ENGINE_PYTHON to an interpreter that has fibration_math_engine"
                )
            python = sys.executable
        args = [python, "-m", "fibration_math_engine"]
        memo = os.environ.get("FIBRATION_MATH_MEMO_DIR")
        if memo:
            args += ["--memo-dir", memo]
        self._proc = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        super().__init__(self._proc.stdout, self._proc.stdin)
        self.request("engine.hello", {"protocol_version": PROTOCOL_VERSION})

    def close(self) -> None:
        if self._proc.poll() is None:
            try:
                self.request("engine.shutdown", {})
            except Exception:
                pass
            try:
                self._proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._proc.kill()


class Client:
    def __init__(self, transport: _Transport):
        self.transport = transport
        self.origin: dict[str, Any] = {"kind": "attach"}

    def call(self, fn: str, args: dict[str, Any]) -> dict[str, Any]:
        return self.transport.request("call", {"fn": fn, "args": args, "origin": self.origin})

    def state(self) -> dict[str, Any]:
        return self.transport.request("state.get", {})

    def describe_api(self, with_state: bool = False) -> dict[str, Any]:
        return self.transport.request("describe_api", {"state": with_state})

    def reset(self) -> dict[str, Any]:
        return self.transport.request("reset", {})

    def close(self) -> None:
        self.transport.close()


_client: Client | None = None
_lock = threading.Lock()


def client() -> Client:
    global _client
    with _lock:
        if _client is None:
            path = os.environ.get("FIBRATION_MATH_SOCKET")
            transport: _Transport = SocketTransport(path) if path else HeadlessTransport()
            _client = Client(transport)
            if not path:
                _client.origin = {"kind": "script"}
            atexit.register(_client.close)
        return _client


def use(new: Client | None) -> None:
    """Replace the process's client (tests, the runner)."""
    global _client
    with _lock:
        _client = new


def set_origin(origin: dict[str, Any]) -> None:
    client().origin = origin
