"""Error model (ARCHITECTURE 5.5).

Two layers, kept apart on purpose:

* ``CallError`` is a classified failure of one engine call. It is returned *inside* a
  successful JSON-RPC response as ``{"status": "error", "error": {...}}`` so the same object
  reaches a cell (as an SDK exception), the agent (as a tool result), and the UI.
* ``RpcError`` is a protocol failure (bad framing, unknown method), returned as a JSON-RPC
  error object, with the codes of Analyst PROTOCOL 1.2.
"""

from __future__ import annotations

from typing import Any

PARSE_ERROR = -32700
INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
INTERNAL_ERROR = -32603

ERROR_KINDS = (
    "parse",  # math text could not be read
    "unknown_fn",  # no such operation
    "bad_args",  # arguments do not match the operation's schema or meaning
    "bad_handle",  # a handle does not exist or has the wrong kind
    "math",  # SymPy raised, or the mathematics has no answer of the requested form
    "undecided",  # a truth value was required and the engine could not decide it
    "timeout",  # the call ran past its budget; never a partial answer
    "budget",  # a size limit (expression size, sample count) was exceeded
    "masked",  # the operation is not valid in the current scene state
)


class CallError(Exception):
    def __init__(self, kind: str, detail: str, hint: str | None = None):
        assert kind in ERROR_KINDS, kind
        super().__init__(detail)
        self.kind = kind
        self.detail = detail
        self.hint = hint

    def to_json(self) -> dict[str, Any]:
        out: dict[str, Any] = {"kind": self.kind, "detail": self.detail}
        if self.hint:
            out["hint"] = self.hint
        return out


class RpcError(Exception):
    def __init__(self, code: int, detail: str):
        super().__init__(detail)
        self.code = code
        self.detail = detail

    def to_json(self) -> dict[str, Any]:
        messages = {
            PARSE_ERROR: "parse error",
            INVALID_REQUEST: "invalid request",
            METHOD_NOT_FOUND: "method not found",
            INVALID_PARAMS: "invalid params",
            INTERNAL_ERROR: "internal error",
        }
        return {"code": self.code, "message": messages.get(self.code, "error"), "data": {"detail": self.detail}}
