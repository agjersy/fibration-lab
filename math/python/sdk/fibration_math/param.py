"""Parameters: named values with slider ranges, bound into the scene."""

from __future__ import annotations

from typing import Any

from ._objects import call


def declare(**specs: tuple[float, ...]) -> Any:
    """``a, b = param.declare(a=(1.0, -3, 3), b=(1.0, 0.1, 5, 0.1))``: (value, min, max[, step]).

    Returns the parameters' symbols, in the order given.
    """
    if not specs:
        raise ValueError("declare at least one parameter")
    out = []
    for name, spec in specs.items():
        if not isinstance(spec, tuple | list) or len(spec) not in (3, 4):
            raise ValueError(f"{name}: give (value, min, max) or (value, min, max, step)")
        value, lo, hi, *rest = spec
        symbol = call("param.declare", name=name, value=value, min=lo, max=hi, step=rest[0] if rest else None)
        symbol.name = name
        out.append(symbol)
    return out[0] if len(out) == 1 else tuple(out)


def set(**values: float) -> None:
    """``param.set(a=2.4)``."""
    call("param.set", values=values)
