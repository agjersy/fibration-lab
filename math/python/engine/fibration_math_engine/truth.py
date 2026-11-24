"""Three-valued truth (ARCHITECTURE 5.2).

Introspection answers ``true``, ``false`` or ``unknown``. ``unknown`` is a first-class answer:
the SDK raises ``Undecided`` when code branches on it, so an undecided fact is never quietly
read as false. ``method`` says how the answer was reached, which is what claim badges use:
``symbolic`` answers can support a **proven** claim, ``numeric`` ones only **checked**.
"""

from __future__ import annotations

from typing import Any


def truth(value: bool | None, method: str = "symbolic", reason: str | None = None) -> dict[str, Any]:
    out: dict[str, Any] = {
        "truth": "unknown" if value is None else ("true" if value else "false"),
        "method": method if value is not None else "none",
    }
    if reason:
        out["reason"] = reason
    return out


def from_fuzzy(value: Any, method: str = "symbolic", reason: str | None = None) -> dict[str, Any]:
    """SymPy's fuzzy booleans (True, False, None, or sympy.true/false) to a Truth."""
    if value is None:
        return truth(None, reason=reason or "the engine could not decide this")
    return truth(bool(value), method, reason)
