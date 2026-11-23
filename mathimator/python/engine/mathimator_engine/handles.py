"""Opaque handles (ARCHITECTURE 5.2).

A handle is an integer naming an immutable SymPy object held by the engine. Only the
reference ``{"$h": id, "kind": ...}`` crosses the wire. Each object also has a content hash
(SHA-256 of its ``srepr``), which is what scene state and the memo key use: two handles to
equal objects hash the same, so replays are deterministic whatever ids were handed out.
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass
from typing import Any

import sympy as sp

from .errors import CallError

KINDS = ("expr", "relation", "boolean", "set", "numeric_fn")


def kind_of(obj: Any) -> str:
    if isinstance(obj, sp.Set):
        return "set"
    if isinstance(obj, sp.core.relational.Relational):
        return "relation"
    if isinstance(obj, sp.logic.boolalg.Boolean) and not isinstance(obj, sp.Expr):
        return "boolean"
    if isinstance(obj, sp.Basic):
        return "expr"
    if isinstance(obj, NumericFn):
        return "numeric_fn"
    raise CallError("math", f"the engine cannot hold a value of type {type(obj).__name__}")


@dataclass(frozen=True)
class NumericFn:
    """A lambdified function: the source expression and its argument symbols."""

    expr: Any
    args: tuple[Any, ...]

    def srepr(self) -> str:
        return f"NumericFn({sp.srepr(self.expr)}, {sp.srepr(sp.Tuple(*self.args))})"


def content_hash(obj: Any) -> str:
    text = obj.srepr() if isinstance(obj, NumericFn) else sp.srepr(obj)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


@dataclass
class Entry:
    obj: Any
    kind: str
    hash: str


class HandleTable:
    def __init__(self, limit: int = 200_000):
        self._entries: dict[int, Entry] = {}
        self._by_hash: dict[str, int] = {}
        self._next = 1
        self._limit = limit

    def put(self, obj: Any) -> dict[str, Any]:
        """Store an object (or reuse the handle of an equal one) and return its reference."""
        h = content_hash(obj)
        existing = self._by_hash.get(h)
        if existing is not None:
            return self.ref(existing)
        if len(self._entries) >= self._limit:
            raise CallError("budget", "too many live objects in this session; reset the scene")
        hid = self._next
        self._next += 1
        self._entries[hid] = Entry(obj, kind_of(obj), h)
        self._by_hash[h] = hid
        return self.ref(hid)

    def ref(self, hid: int) -> dict[str, Any]:
        return {"$h": hid, "kind": self._entries[hid].kind}

    def entry(self, ref: Any, kinds: tuple[str, ...] | None = None) -> Entry:
        hid = ref.get("$h") if isinstance(ref, dict) else ref
        if not isinstance(hid, int) or isinstance(hid, bool) or hid not in self._entries:
            raise CallError("bad_handle", f"no object with handle {hid!r} in this session")
        entry = self._entries[hid]
        if kinds is not None and entry.kind not in kinds:
            raise CallError(
                "bad_handle", f"handle {hid} is a {entry.kind}; this argument needs {' or '.join(kinds)}"
            )
        return entry

    def get(self, ref: Any, kinds: tuple[str, ...] | None = None) -> Any:
        return self.entry(ref, kinds).obj

    def __len__(self) -> int:
        return len(self._entries)


def is_ref(value: Any) -> bool:
    return isinstance(value, dict) and "$h" in value and set(value) <= {"$h", "kind"}
