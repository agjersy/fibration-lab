"""Scene state: masking, revisions, determinism, rollback, view hints."""

from __future__ import annotations

import json
import subprocess
import sys

import pytest

from fibration_math_engine.registry import REGISTRY

SCRIPT = [
    ("sym.symbols", {"names": "x"}),
    ("param.declare", {"name": "a", "value": 1, "min": -3, "max": 3}),
    ("plot.explicit", {"expr": "a*sin(x)", "var": "x", "id": "curve"}),
    ("plot.tangent", {"of": "curve", "at": 0}),
    ("param.set", {"values": {"a": 2.5}}),
    ("note.label", {"text": "hi", "at": "curve"}),
    ("plot.area", {"of": "curve", "over": [0, "pi"]}),
]


def run_script(engine):
    for fn, args in SCRIPT:
        out = engine.call(fn, args)
        assert out["status"] == "ok", (fn, out)
    return engine.scene.hash()


MASKED_FOR_EMPTY = {"param.set", "plot.tangent", "plot.area", "plot.riemann", "scene.highlight", "scene.style", "scene.remove"}


def test_masking_on_empty_scene(s):
    manifest = s.engine.describe_api(with_state=True)
    unavailable = {o["name"] for o in manifest["operations"] if not o["available"]}
    assert unavailable == MASKED_FOR_EMPTY
    err = s.err("param.set", values={"a": 1})
    assert err["kind"] == "masked" and "param.declare" in err["detail"]


@pytest.mark.parametrize(
    ("setup", "available", "unavailable"),
    [
        ([("param.declare", {"name": "a", "value": 0, "min": 0, "max": 1})], {"param.set"}, {"plot.tangent", "scene.remove"}),
        ([("plot.point", {"at": [0, 0]})], {"scene.remove", "scene.style", "scene.highlight"}, {"plot.tangent", "param.set"}),
        ([("plot.explicit", {"expr": "x", "var": "x"})], {"plot.tangent", "plot.area", "plot.riemann"}, {"param.set"}),
        ([("plot.parametric", {"x": "cos(t)", "y": "sin(t)", "var": "t", "range": [0, 1]})], {"scene.remove"}, {"plot.tangent"}),
        ([("note.label", {"text": "t", "at": [0, 0]})], {"scene.remove"}, {"plot.area"}),
    ],
)
def test_masking_table(s, setup, available, unavailable):
    for fn, args in setup:
        s.ok(fn, **args)
    ops = {o["name"]: o["available"] for o in s.engine.describe_api(with_state=True)["operations"]}
    assert all(ops[n] for n in available)
    assert not any(ops[n] for n in unavailable)


def test_revisions_and_diffs(s):
    s.ok("sym.symbols", names="x")
    assert s.engine.scene.revision == 0  # pure calls do not change the scene
    out = s.ok("plot.explicit", expr="x**2", var="x", id="p")
    assert out["revision"] == 1 and out["diff"]["objects"]["added"][0]["id"] == "p"
    out = s.ok("scene.style", target="p", style={"color": "red"})
    assert out["revision"] == 2 and out["diff"]["objects"]["updated"][0]["style"] == {"color": "red"}
    out = s.ok("scene.remove", target="p")
    assert out["diff"]["objects"]["removed"] == ["p"]


def test_failed_call_rolls_back(s):
    s.ok("plot.explicit", expr="x", var="x", id="p")
    before = s.engine.scene.hash()
    s.err("plot.explicit", expr="x + b", var="x", id="q")
    assert s.engine.scene.hash() == before and "q" not in s.engine.scene.objects


def test_unbound_symbols_name_the_fix(s):
    err = s.err("plot.explicit", expr="a*x + b", var="x")
    assert "a, b" in err["detail"] and "param.declare" in err["hint"]


def test_view_hints_do_not_change_scene(s):
    s.ok("plot.explicit", expr="x", var="x")
    h, rev = s.engine.scene.hash(), s.engine.scene.revision
    out = s.ok("cam.view", x=[-1, 1])
    assert out["revision"] == rev and s.engine.scene.hash() == h and out["diff"] == {"view": s.engine.scene.view}


def test_param_ranges_enforced(s):
    s.ok("param.declare", name="a", value=1, min=0, max=2)
    assert s.err("param.set", values={"a": 3})["kind"] == "bad_args"
    assert s.err("param.declare", name="b", value=5, min=0, max=2)["kind"] == "bad_args"


def test_restore_and_reset(s):
    s.ok("plot.explicit", expr="x", var="x")
    s.ok("plot.explicit", expr="x**2", var="x")
    first = s.engine.snapshots[1].hash()
    out = s.engine.restore(1)
    assert out["hash"] == first and len(s.engine.scene.objects) == 1
    s.engine.reset()
    assert s.engine.scene.revision == 0 and not s.engine.scene.objects


def test_replay_is_deterministic_across_processes(s):
    local = run_script(s.engine)
    code = (
        "import json,sys; sys.path[:0]=[{!r}]; from fibration_math_engine.engine import Engine; from tests.test_scene import run_script;"
        "print(run_script(Engine()))"
    ).format(".")
    other = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, check=True).stdout.strip()
    assert local == other
    # Replaying in the same engine after a reset gives the same hash with memo hits.
    s.engine.reset()
    hits = s.engine.memo.hits
    assert run_script(s.engine) == local
    assert s.engine.memo.hits > hits


def test_public_state_is_json(s):
    run_script(s.engine)
    text = json.dumps(s.engine.state(), allow_nan=False)
    assert '"latex"' in text


def test_every_operation_is_documented():
    for op in REGISTRY.values():
        assert op.summary and op.result
        for name in op.required:
            assert name in op.args, (op.name, name)
