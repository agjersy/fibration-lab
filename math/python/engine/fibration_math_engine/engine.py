"""The engine: one session's handles, scene state, memo, and the ``call`` dispatcher."""

from __future__ import annotations

import difflib
import time
from collections import OrderedDict
from pathlib import Path
from typing import Any

import sympy as sp

from . import mathparse, schema
from .budget import budget
from .errors import CallError
from .handles import HandleTable, is_ref
from .memo import Memo, Out, key
from .registry import REGISTRY, manifest
from .scene import SceneState, diff

SNAPSHOT_LIMIT = 256


def require_defined(obj: Any, what: str = "the result") -> None:
    """Refuse an expression that is undefined: SymPy's complex infinity (1/0) or nan (0/0).

    These are not real numbers, so no object the engine hands out or plots may contain them.
    Signed infinities are fine: they are the extended-real values limits return.
    """
    if isinstance(obj, sp.Basic) and not isinstance(obj, sp.Set) and obj.has(sp.zoo, sp.nan):
        raise CallError(
            "math", f"{what} is undefined (a division by zero or an indeterminate form such as 0/0)"
        )


class Engine:
    def __init__(self, memo_dir: Path | None = None):
        # Importing the operation modules fills the registry.
        from . import ops  # noqa: F401

        self.handles = HandleTable()
        self.memo = Memo(memo_dir)
        self.scene = SceneState()
        self.snapshots: OrderedDict[int, SceneState] = OrderedDict()
        # Sampled geometry by what it depends on (see sample._cache_key).
        self.samples: OrderedDict[str, dict[str, Any]] = OrderedDict()
        self._snapshot()

    # ---- argument helpers used by operations -------------------------------------
    def value(self, arg: Any) -> Any:
        """A scalar argument (number, math text, or handle) as a SymPy object."""
        if is_ref(arg):
            return self.handles.get(arg)
        if isinstance(arg, bool):
            raise CallError("bad_args", "a boolean is not a number")
        if isinstance(arg, int):
            return sp.Integer(arg)
        if isinstance(arg, float):
            # Read through its shortest decimal form, as typed: 0.1 is 1/10.
            return sp.Rational(repr(arg))
        if isinstance(arg, str):
            return mathparse.read_math(arg)
        raise CallError("bad_args", f"cannot read {arg!r} as a value")

    def entry(self, arg: Any, kinds: tuple[str, ...] | None = None):
        """The handle-table entry for a handle reference or for math text."""
        if isinstance(arg, str):
            arg = self.handles.put(mathparse.read_math(arg))
        return self.handles.entry(arg, kinds)

    def expr(self, arg: Any, what: str = "expression") -> sp.Expr:
        obj = self.value(arg)
        if not isinstance(obj, sp.Expr):
            raise CallError("bad_args", f"the {what} must be an expression, not a {type(obj).__name__}")
        return obj

    def symbol(self, arg: Any, what: str = "variable") -> sp.Symbol:
        obj = self.value(arg)
        if not isinstance(obj, sp.Symbol):
            raise CallError("bad_args", f"the {what} must be a single symbol such as x")
        return obj

    def interval(self, pair: list[Any], what: str = "interval") -> tuple[sp.Expr, sp.Expr]:
        lo, hi = (self.expr(v, what) for v in pair)
        if lo.is_comparable and hi.is_comparable and not bool(lo < hi):
            raise CallError("bad_args", f"the {what} [{lo}, {hi}] is empty; the lower end must come first")
        return lo, hi

    # ---- state lifecycle ------------------------------------------------------
    def _snapshot(self) -> None:
        self.snapshots[self.scene.revision] = self.scene.copy()
        while len(self.snapshots) > SNAPSHOT_LIMIT:
            self.snapshots.popitem(last=False)

    def reset(self) -> dict[str, Any]:
        """Empty the scene for a replay. Handles and the memo are kept: that is what makes
        replaying an unchanged prefix cheap."""
        self.scene = SceneState()
        self.snapshots.clear()
        self._snapshot()
        return {"revision": 0, "hash": self.scene.hash()}

    def restore(self, revision: int) -> dict[str, Any]:
        snap = self.snapshots.get(revision)
        if snap is None:
            raise CallError("bad_args", f"revision {revision} is not available to restore")
        before = self.scene
        self.scene = snap.copy()
        return {"revision": self.scene.revision, "hash": self.scene.hash(), "diff": diff(before, self.scene, self.handles)}

    def state(self) -> dict[str, Any]:
        return self.scene.public(self.handles)

    def describe_api(self, with_state: bool = False) -> dict[str, Any]:
        return manifest(self.scene if with_state else None)

    # ---- the one primitive ------------------------------------------------------
    def call(self, fn: str, args: Any = None, origin: Any = None, use_memo: bool = True) -> dict[str, Any]:
        started = time.perf_counter()
        try:
            result = self._call(fn, {} if args is None else args, origin, use_memo)
        except CallError as exc:
            return {"status": "error", "error": exc.to_json(), "revision": self.scene.revision}
        result["ms"] = round((time.perf_counter() - started) * 1000, 3)
        return result

    def _call(self, fn: str, args: Any, origin: Any, use_memo: bool) -> dict[str, Any]:
        op = REGISTRY.get(fn)
        if op is None:
            close = difflib.get_close_matches(fn, list(REGISTRY), n=3)
            raise CallError(
                "unknown_fn", f"there is no operation {fn!r}", hint=("did you mean " + ", ".join(close)) if close else None
            )
        schema.validate(op.schema(), args)
        if op.mask is not None and not op.mask.test(self.scene):
            raise CallError("masked", f"{fn} is not available now: {op.mask.reason}")

        if op.pure:
            memo_key = key(fn, args, self.handles) if use_memo else None
            if memo_key is not None:
                hit, cached = self.memo.get(memo_key)
                if hit:
                    return {"status": "ok", "value": self._externalize(cached), "revision": self.scene.revision, "memo": True}
            value = self._run(op, args)
            if memo_key is not None:
                self.memo.put(memo_key, value)
            return {"status": "ok", "value": self._externalize(value), "revision": self.scene.revision}

        before = self.scene
        self.scene = before.copy()
        self._origin = origin
        try:
            value = self._run(op, args)
        except BaseException:
            self.scene = before
            raise
        changes = diff(before, self.scene, self.handles)
        if op.changes_view or not changes or set(changes) == {"view"}:
            # View hints and no-op calls do not create a revision.
            self.scene.revision = before.revision
        else:
            self.scene.revision = before.revision + 1
            self._snapshot()
        return {
            "status": "ok",
            "value": self._externalize(value),
            "diff": changes,
            "revision": self.scene.revision,
            "hash": self.scene.hash(),
        }

    @property
    def origin(self) -> str | None:
        origin = getattr(self, "_origin", None)
        if isinstance(origin, dict):
            return origin.get("cell") or origin.get("kind")
        return origin if isinstance(origin, str) else None

    def _run(self, op, args: dict[str, Any]) -> Any:
        with budget(op.budget_s):
            try:
                value = op.impl(self, **args)
                # Turn results into handles here, inside the budget and the error mapping:
                # hashing a result prints it, which can be slow or fail for a huge object.
                self._externalize(value)
                return value
            except CallError:
                raise
            except RecursionError:
                raise CallError("budget", "the expression is too deeply nested") from None
            except (
                NotImplementedError,
                ValueError,
                TypeError,
                ZeroDivisionError,
                AttributeError,
                sp.PolynomialError,
                sp.SympifyError,
            ) as exc:
                raise CallError("math", f"{op.name} failed: {exc}" if str(exc) else f"{op.name} failed") from None

    def _externalize(self, value: Any) -> Any:
        if isinstance(value, Out):
            require_defined(value.obj)
            return self.handles.put(value.obj)
        if isinstance(value, dict):
            return {k: self._externalize(v) for k, v in value.items()}
        if isinstance(value, list | tuple):
            return [self._externalize(v) for v in value]
        return value
