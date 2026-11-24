from __future__ import annotations

import pytest

from mathimator_engine.engine import Engine


class Session:
    """An engine with helpers that raise on errors, for readable tests."""

    def __init__(self, engine: Engine):
        self.engine = engine

    def ok(self, fn, **args):
        out = self.engine.call(fn, args)
        assert out["status"] == "ok", out
        return out

    def val(self, fn, **args):
        return self.ok(fn, **args)["value"]

    def err(self, fn, **args):
        out = self.engine.call(fn, args)
        assert out["status"] == "error", out
        return out["error"]

    def obj(self, ref):
        return self.engine.handles.get(ref)

    def define(self, text):
        return self.val("sym.define", text=text)

    def x(self):
        return self.val("sym.symbols", names="x")[0]


@pytest.fixture
def s() -> Session:
    return Session(Engine())
