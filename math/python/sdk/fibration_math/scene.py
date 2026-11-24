"""Edit and inspect the scene."""

from __future__ import annotations

from typing import Any

from . import _client
from ._objects import call, interval
from .plot import Plotted


def highlight(target: str, over=None, *, id=None, style=None):
    return Plotted(call("scene.highlight", target=target, over=interval(over), id=id, style=style))


def style(target: str, **style: Any) -> None:
    call("scene.style", target=target, style=style)


def remove(target: str) -> list[str]:
    return call("scene.remove", target=target)["removed"]


def state() -> dict[str, Any]:
    """The current scene state (parameters, objects, notes, revision, hash)."""
    return _client.client().state()


def api(available_only: bool = False) -> dict[str, Any]:
    """The engine's operation manifest; with ``available_only``, only what the scene allows now."""
    manifest = _client.client().describe_api(with_state=available_only)
    if available_only:
        manifest["operations"] = [op for op in manifest["operations"] if op.get("available", True)]
    return manifest
