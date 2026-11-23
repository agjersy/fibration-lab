"""View hints. These set the initial camera only and are never scene state."""

from __future__ import annotations

from ._objects import call, interval


def fit() -> None:
    call("cam.fit")


def view(x, y=None) -> None:
    """``cam.view(x=(-5, 5), y=(-2, 2))``."""
    call("cam.view", x=interval(x), y=interval(y))
