"""The cell runner (ARCHITECTURE 6.3): ``python -m fibration_math.runner``.

The core starts one runner per replay, in a fresh interpreter, under the sandbox, with
``FIBRATION_MATH_SOCKET`` naming the router. The contract is one JSON document in and one JSON
line out:

stdin::

    {"cells": [{"id": "c1", "code": "..."}, ...]}

stdout (one line)::

    {"results": [{"id": "c1", "status": "ok", "stdout": "..."},
                 {"id": "c2", "status": "error", "stdout": "",
                  "error": {"type": "MathError", "message": "...", "traceback": "...",
                            "line": 3, "call_error": {"kind": "...", "detail": "...", "hint": "..."}}}]}

Cells run in order in one namespace, so later cells see earlier variables. Each engine call
a cell makes carries the origin ``{"kind": "cell", "cell": <id>}``. The first failing cell
stops the run; the cells after it are not reported. Anything a cell prints is captured per
cell; the runner's own stdout carries only the result line.
"""

from __future__ import annotations

import contextlib
import io
import json
import sys
import traceback
from typing import Any

MAX_CAPTURE = 64_000


def _clip(text: str) -> str:
    return text if len(text) <= MAX_CAPTURE else text[:MAX_CAPTURE] + "\n[output truncated]"


def _error(exc: BaseException, cell_id: str) -> dict[str, Any]:
    from .errors import MathError

    filename = f"<cell {cell_id}>"
    frames = [f for f in traceback.extract_tb(exc.__traceback__) if f.filename == filename]
    out: dict[str, Any] = {
        "type": type(exc).__name__,
        "message": str(exc),
        "traceback": "".join(traceback.format_exception(type(exc), exc, exc.__traceback__)[-12:]),
    }
    if frames:
        out["line"] = frames[-1].lineno
    if isinstance(exc, SyntaxError) and exc.filename == filename:
        out["line"] = exc.lineno
    if isinstance(exc, MathError):
        out["call_error"] = {"kind": exc.kind, "detail": exc.detail, "hint": exc.hint, "fn": exc.fn}
    return out


def run_cells(cells: list[dict[str, Any]]) -> list[dict[str, Any]]:
    from . import _client

    namespace: dict[str, Any] = {"__name__": "__fibration_math_scene__", "__builtins__": __builtins__}
    results = []
    for cell in cells:
        cell_id = str(cell.get("id", "?"))
        code = cell.get("code", "")
        captured = io.StringIO()
        entry: dict[str, Any] = {"id": cell_id, "status": "ok"}
        try:
            _client.set_origin({"kind": "cell", "cell": cell_id})
            compiled = compile(code, f"<cell {cell_id}>", "exec")
            with contextlib.redirect_stdout(captured), contextlib.redirect_stderr(captured):
                exec(compiled, namespace)
        except BaseException as exc:  # a cell's SystemExit or KeyboardInterrupt is its failure too
            entry = {"id": cell_id, "status": "error", "error": _error(exc, cell_id)}
        entry["stdout"] = _clip(captured.getvalue())
        results.append(entry)
        if entry["status"] == "error":
            break
    return results


def main() -> int:
    out = sys.stdout
    try:
        doc = json.loads(sys.stdin.read())
        cells = doc["cells"]
        if not isinstance(cells, list):
            raise ValueError("cells must be a list")
    except (ValueError, KeyError, TypeError) as exc:
        out.write(json.dumps({"error": f"bad runner input: {exc}"}) + "\n")
        out.flush()
        return 2
    results = run_cells(cells)
    out.write(json.dumps({"results": results}, default=str) + "\n")
    out.flush()
    return 0


if __name__ == "__main__":
    sys.exit(main())
