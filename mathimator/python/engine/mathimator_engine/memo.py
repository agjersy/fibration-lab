"""Memoization of pure calls (ARCHITECTURE 5.2).

The key is the operation name plus its arguments with every handle replaced by the content
hash of its object, so a replay that re-creates the same objects (under new handle ids) hits
the same entries. Values are stored with SymPy objects inside ``Out`` markers.

An optional directory makes the memo survive the process: each entry is one JSON file whose
SymPy objects are written as ``srepr`` and read back through the restricted ``read_srepr``
walker, never ``eval``. A file that cannot be read is ignored and the call is recomputed.
"""

from __future__ import annotations

import hashlib
import json
import os
from collections import OrderedDict
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import sympy as sp

from .errors import CallError
from .handles import HandleTable, NumericFn, is_ref
from .mathparse import read_srepr


@dataclass(frozen=True)
class Out:
    """A SymPy object an operation returns; the engine turns it into a handle reference."""

    obj: Any


def canonical_args(args: Any, handles: HandleTable) -> Any:
    if is_ref(args):
        return {"$hash": handles.entry(args).hash}
    if isinstance(args, dict):
        return {k: canonical_args(v, handles) for k, v in sorted(args.items())}
    if isinstance(args, list):
        return [canonical_args(v, handles) for v in args]
    return args


def key(fn: str, args: Any, handles: HandleTable) -> str:
    text = json.dumps([fn, canonical_args(args, handles)], sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _encode(value: Any) -> Any:
    if isinstance(value, Out):
        if isinstance(value.obj, NumericFn):
            raise TypeError("numeric functions are not stored")
        return {"$srepr": sp.srepr(value.obj)}
    if isinstance(value, dict):
        return {k: _encode(v) for k, v in value.items()}
    if isinstance(value, list | tuple):
        return [_encode(v) for v in value]
    return value


def _decode(value: Any) -> Any:
    if isinstance(value, dict):
        if set(value) == {"$srepr"}:
            return Out(read_srepr(value["$srepr"]))
        return {k: _decode(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_decode(v) for v in value]
    return value


class Memo:
    def __init__(self, directory: Path | None = None, limit: int = 50_000):
        self._mem: OrderedDict[str, Any] = OrderedDict()
        self._dir = directory
        self._limit = limit
        self.hits = 0
        self.misses = 0
        if directory is not None:
            directory.mkdir(parents=True, exist_ok=True)

    def get(self, k: str) -> tuple[bool, Any]:
        if k in self._mem:
            self._mem.move_to_end(k)
            self.hits += 1
            return True, self._mem[k]
        if self._dir is not None:
            path = self._dir / f"{k}.json"
            try:
                value = _decode(json.loads(path.read_text("utf-8")))
            except (OSError, ValueError, CallError):
                value = None
            else:
                self._remember(k, value)
                self.hits += 1
                return True, value
        self.misses += 1
        return False, None

    def put(self, k: str, value: Any) -> None:
        self._remember(k, value)
        if self._dir is not None:
            try:
                data = json.dumps(_encode(value), separators=(",", ":"))
            except (TypeError, ValueError):
                return
            tmp = self._dir / f".{k}.{os.getpid()}.tmp"
            try:
                tmp.write_text(data, "utf-8")
                os.replace(tmp, self._dir / f"{k}.json")
            except OSError:
                pass

    def _remember(self, k: str, value: Any) -> None:
        self._mem[k] = value
        self._mem.move_to_end(k)
        while len(self._mem) > self._limit:
            self._mem.popitem(last=False)

