"""Named numeric results of a run (docs/PROTOCOL.md section 1.7).

A script records the numbers it wants to stand behind::

    from fibration_results import record
    record("tau", 1.82, unit="s", uncertainty=0.04)

Each call writes ``$FIBRATION_OUT_DIR/results.json`` (``./out/results.json`` when the variable is
unset) atomically; recording a name again replaces its value. The kernel lists the file as an
artifact of kind ``results``, the artifact index exposes the values, and slide text can bind a
number to one of them. Standard library only; runs on Python 3.9 and later.
"""

from __future__ import annotations

import json
import math
import os
import re
from pathlib import Path

__all__ = ["RESULTS_FILE", "load", "record", "results_path"]

RESULTS_FILE = "results.json"
_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_.\-]{0,63}$")


def results_path() -> Path:
    base = os.environ.get("FIBRATION_OUT_DIR")
    root = Path(base) if base else Path.cwd() / "out"
    return root / RESULTS_FILE


def load(path: Path | None = None) -> list[dict]:
    """The results recorded so far (an empty list when there are none)."""
    p = path or results_path()
    try:
        data = json.loads(p.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return []
    items = data.get("results") if isinstance(data, dict) else None
    return [r for r in items if isinstance(r, dict)] if isinstance(items, list) else []


def _number(what: str, value: object) -> float:
    if isinstance(value, (bool, str, bytes)):
        raise TypeError(f"{what} must be a real number, not {type(value).__name__}")
    try:
        number = float(value)  # ints, floats, and NumPy scalars
    except (TypeError, ValueError):
        raise TypeError(f"{what} must be a real number, not {type(value).__name__}") from None
    if not math.isfinite(number):
        raise ValueError(f"{what} must be finite, not {number}")
    return number


def record(name: str, value: float, unit: str | None = None, uncertainty: float | None = None) -> dict:
    """Record a named numeric result of this run; returns the entry written."""
    if not isinstance(name, str) or not _NAME.match(name):
        raise ValueError("name must start with a letter or _ and use letters, digits, _ . - (at most 64)")
    entry: dict = {"name": name, "value": _number("value", value)}
    if unit is not None:
        if not isinstance(unit, str) or len(unit) > 32:
            raise ValueError("unit must be a string of at most 32 characters")
        entry["unit"] = unit
    if uncertainty is not None:
        u = _number("uncertainty", uncertainty)
        if u < 0:
            raise ValueError("uncertainty must not be negative")
        entry["uncertainty"] = u
    path = results_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    results = [r for r in load(path) if r.get("name") != name]
    results.append(entry)
    tmp = path.with_name(f".{RESULTS_FILE}.{os.getpid()}.tmp")
    tmp.write_text(json.dumps({"schema": 1, "results": results}, indent=1) + "\n", encoding="utf-8")
    os.replace(tmp, path)
    return entry
