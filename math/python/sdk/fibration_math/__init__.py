"""The Fibration Math scene SDK.

A scene is a Python file whose cells call this API. Everything mathematical happens in the
engine; this package holds handles, never symbolic objects, and needs only the standard
library::

    from fibration_math import sym, plot, param, note, cam

    x = sym.symbols("x")
    a, b = param.declare(a=(1.0, -3, 3), b=(1.0, 0.1, 5))
    f = sym.define("a*sin(b*x)", name="f")
    plot.explicit(f, x=(-10, 10), id="curve")
    if f.singularities(x).is_empty():
        note.label("entire", at="curve")

The namespaces are ``sym`` (construct expressions), ``param`` (parameters and sliders),
``plot`` (objects on the canvas), ``note`` (annotations), ``scene`` (edit and inspect the
scene), and ``cam`` (view hints). Their operations map one to one onto the engine's API
(``scene.api()`` returns the manifest).
"""

from __future__ import annotations

from . import cam, note, param, plot, scene, sym
from ._objects import Expr, Relation, Roots, Set, Truth
from .errors import EngineUnavailable, FibrationMathError, MathError, NoLimit, ProtocolError, Undecided

__version__ = "0.0.1"
API_VERSION = 1

__all__ = [
    "API_VERSION",
    "EngineUnavailable",
    "Expr",
    "FibrationMathError",
    "MathError",
    "NoLimit",
    "ProtocolError",
    "Relation",
    "Roots",
    "Set",
    "Truth",
    "Undecided",
    "cam",
    "note",
    "param",
    "plot",
    "scene",
    "sym",
]
