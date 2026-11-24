"""The runner contract: one JSON document in, one JSON line out (runner.py docstring)."""

from __future__ import annotations

from mathimator import _client, scene
from mathimator.runner import run_cells


def test_cells_share_a_namespace_and_record_their_origin(fresh_engine):
    seen = []
    real_call = fresh_engine.call

    def spy(fn, args):
        seen.append((fn, dict(fresh_engine.origin)))
        return real_call(fn, args)

    fresh_engine.call = spy
    results = run_cells(
        [
            {"id": "c1", "code": "from mathimator import sym, plot\nx = sym.symbols('x')\nprint('hello')"},
            {"id": "c2", "code": "plot.explicit(x**2, x=(-1, 1), id='p')"},
        ]
    )
    assert [r["status"] for r in results] == ["ok", "ok"]
    assert results[0]["stdout"] == "hello\n"
    assert ("plot.explicit", {"kind": "cell", "cell": "c2"}) in seen
    assert [o["id"] for o in scene.state()["objects"]] == ["p"]


def test_first_error_stops_the_run_with_details():
    results = run_cells(
        [
            {"id": "c1", "code": "from mathimator import sym\nx = sym.symbols('x')"},
            {"id": "c2", "code": "y = 1\nsym.define('1/x').limit(x, 0)"},
            {"id": "c3", "code": "print('never')"},
        ]
    )
    assert [r["id"] for r in results] == ["c1", "c2"]
    err = results[1]["error"]
    assert err["type"] == "NoLimit" and err["line"] == 2
    assert err["call_error"]["kind"] == "math"


def test_syntax_errors_point_at_the_line():
    results = run_cells([{"id": "c1", "code": "x = 1\nx = = 2"}])
    assert results[0]["error"]["type"] == "SyntaxError" and results[0]["error"]["line"] == 2


def test_runner_process_round_trip(tmp_path):
    import json
    import subprocess
    import sys

    doc = {"cells": [{"id": "c1", "code": "from mathimator import sym\nprint(sym.define('x+1').text)"}]}
    proc = subprocess.run(
        [sys.executable, "-m", "mathimator.runner"], input=json.dumps(doc), capture_output=True, text=True, timeout=60
    )
    lines = proc.stdout.splitlines()
    assert proc.returncode == 0 and len(lines) == 1
    assert json.loads(lines[0])["results"][0]["stdout"] == "x + 1\n"
    assert _client  # the module is importable without an engine until a call is made
