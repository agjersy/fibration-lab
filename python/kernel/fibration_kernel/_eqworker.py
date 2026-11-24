"""Equation worker: a subprocess of the kernel that does all SymPy work.

One JSON request per line on stdin, one JSON response per line on stdout. The kernel kills
this process when a request exceeds its time budget, so a pathological expression costs
one worker, not the kernel. Nothing here executes repository code.
"""

from __future__ import annotations

import json
import os
import sys

MEMORY_CAP_BYTES = 4 * 1024**3


def main() -> int:
    out = os.fdopen(os.dup(1), "w", encoding="utf-8")
    os.dup2(2, 1)  # stray prints from libraries go to stderr, never into the protocol
    try:
        import resource

        _, hard = resource.getrlimit(resource.RLIMIT_AS)
        cap = MEMORY_CAP_BYTES if hard == resource.RLIM_INFINITY else min(MEMORY_CAP_BYTES, hard)
        resource.setrlimit(resource.RLIMIT_AS, (cap, hard))
    except (ImportError, ValueError, OSError):
        pass
    sys.setrecursionlimit(2000)

    from . import eqcore

    def send(obj: dict) -> None:
        out.write(json.dumps(obj) + "\n")
        out.flush()

    send({"ready": True})
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue
        res: dict = {"id": req.get("id")}
        try:
            res["result"] = eqcore.analyze(
                req["source"], int(req.get("line_offset", 0)), bool(req.get("is_method", False))
            )
        except MemoryError:
            res["error"] = "out of memory while analyzing the expression"
        except RecursionError:
            res["error"] = "expression too deeply nested"
        except Exception as exc:
            res["error"] = f"{type(exc).__name__}: {exc}"
        send(res)
    return 0


if __name__ == "__main__":
    sys.exit(main())
