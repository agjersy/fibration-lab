"""Method groups registered on the kernel as their work packages land."""

from __future__ import annotations

from typing import Any


def hello_info(kernel) -> dict[str, Any]:
    info: dict[str, Any] = {
        "sandbox": {"kind": "limits", "available": True, "detail": "resource limits only"},
        "engines": [],
        "embedding": {"kind": "none", "model": "", "dims": 0},
    }
    for provider in _HELLO:
        info.update(provider(kernel))
    return info


_HELLO: list = []
_GROUPS: list = []


def hello_provider(fn):
    _HELLO.append(fn)
    return fn


def group(fn):
    _GROUPS.append(fn)
    return fn


def register_all(kernel) -> None:
    from importlib import import_module

    for name in ("fidelity", "checker", "refs", "transforms", "runner", "exports"):
        try:
            import_module(f"present_kernel.{name}.rpc")
        except ModuleNotFoundError as exc:
            if exc.name and exc.name.startswith("present_kernel"):
                continue
            raise
    for fn in _GROUPS:
        fn(kernel)
