"""Annotations: labels, formulas, arrows."""

from __future__ import annotations

from typing import Any

from ._objects import Handle, _scalar, call
from .plot import Plotted


def _at(at: Any) -> Any:
    if isinstance(at, str):
        return at
    x, y = at
    return [_scalar(x), _scalar(y)]


def label(text: str, at: Any, *, id=None, style=None):
    """``note.label("max", at="curve")`` or ``at=(1, 2)``."""
    return Plotted(call("note.label", text=text, at=_at(at), id=id, style=style))


def latex(expr: Handle, at: Any, *, id=None, style=None):
    """Show an object's formula at a point or on an object."""
    return Plotted(call("note.latex", expr=expr, at=_at(at), id=id, style=style))


def arrow(start, end, *, id=None, style=None):
    return Plotted(call("note.arrow", start=_at(start), end=_at(end), id=id, style=style))
