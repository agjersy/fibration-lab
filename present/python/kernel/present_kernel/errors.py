"""JSON-RPC error codes (PROTOCOL.md section 1.2) and the exception that carries them."""

from __future__ import annotations

from typing import Any

PARSE_ERROR = -32700
INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
INTERNAL_ERROR = -32603

NOT_FOUND = 2000
PATH_OUTSIDE_ROOT = 2001
SCHEMA_INVALID = 2002
TEMPLATE_UNREADABLE = 2003
RENDER_FAILED = 2004
BUILD_FAILED = 2005
TIMEOUT = 2006
SANDBOX_UNAVAILABLE = 2007
ENGINE_UNAVAILABLE = 2008
UNSUPPORTED_REFERENCE = 2009
TRANSFORM_FAILED = 2010

_MESSAGES = {
    PARSE_ERROR: "parse error",
    INVALID_REQUEST: "invalid request",
    METHOD_NOT_FOUND: "method not found",
    INVALID_PARAMS: "invalid params",
    INTERNAL_ERROR: "internal error",
    NOT_FOUND: "not found",
    PATH_OUTSIDE_ROOT: "path escapes the project root",
    SCHEMA_INVALID: "schema validation failed",
    TEMPLATE_UNREADABLE: "template unreadable or unsupported",
    RENDER_FAILED: "render failed",
    BUILD_FAILED: "build script failed",
    TIMEOUT: "timeout",
    SANDBOX_UNAVAILABLE: "sandbox requested but unavailable",
    ENGINE_UNAVAILABLE: "fidelity engine unavailable",
    UNSUPPORTED_REFERENCE: "unsupported reference format",
    TRANSFORM_FAILED: "transform failed",
}


class RpcError(Exception):
    """An error that maps directly onto a JSON-RPC error object."""

    def __init__(self, code: int, detail: str, message: str | None = None, **extra: Any):
        super().__init__(detail)
        self.code = code
        self.detail = detail
        self.message = message or _MESSAGES.get(code, "error")
        self.extra = extra

    def to_json(self) -> dict[str, Any]:
        data: dict[str, Any] = {"detail": self.detail}
        data.update(self.extra)
        return {"code": self.code, "message": self.message, "data": data}


def invalid_params(detail: str) -> RpcError:
    return RpcError(INVALID_PARAMS, detail)


def not_found(detail: str) -> RpcError:
    return RpcError(NOT_FOUND, detail)
