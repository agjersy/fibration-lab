"""Memo persistence, call budgets, and the stdio contract."""

from __future__ import annotations

import json
import subprocess
import sys
import time

from mathimator_engine.engine import Engine
from mathimator_engine.registry import REGISTRY


def test_disk_memo_survives_the_process(tmp_path):
    first = Engine(memo_dir=tmp_path)
    x = first.call("sym.symbols", {"names": "x"})["value"][0]
    f = first.call("sym.define", {"text": "exp(x)*sin(x)"})["value"]
    assert first.call("integrate", {"expr": f, "var": x})["status"] == "ok"
    second = Engine(memo_dir=tmp_path)
    x2 = second.call("sym.symbols", {"names": "x"})["value"][0]
    f2 = second.call("sym.define", {"text": "exp(x)*sin(x)"})["value"]
    out = second.call("integrate", {"expr": f2, "var": x2})
    assert out.get("memo") is True and out["status"] == "ok"


def test_corrupt_memo_entries_are_recomputed(tmp_path):
    e = Engine(memo_dir=tmp_path)
    e.call("sym.define", {"text": "x"})
    for path in tmp_path.glob("*.json"):
        path.write_text('{"$srepr": "__import__(\\"os\\")"}')
    fresh = Engine(memo_dir=tmp_path)
    out = fresh.call("sym.define", {"text": "x"})
    assert out["status"] == "ok" and "memo" not in out


def test_budget_interrupts_and_reports_timeout():
    e = Engine()
    op = REGISTRY["integrate"]
    saved = op.budget_s
    op.budget_s = 0.3
    try:
        f = e.call("sym.define", {"text": "exp(sin(x)**3)*x**7/(1+x**5)**(1/3)"})["value"]
        started = time.perf_counter()
        out = e.call("integrate", {"expr": f, "var": "x"})
        elapsed = time.perf_counter() - started
    finally:
        op.budget_s = saved
    assert out["status"] == "error" and out["error"]["kind"] == "timeout"
    assert elapsed < 0.3 + 0.5


def _talk(lines):
    proc = subprocess.run(
        [sys.executable, "-m", "mathimator_engine"],
        input="".join(json.dumps(m) + "\n" for m in lines),
        capture_output=True,
        text=True,
        timeout=60,
    )
    return [json.loads(line) for line in proc.stdout.splitlines()], proc.returncode


def test_stdio_contract():
    out, code = _talk(
        [
            {"jsonrpc": "2.0", "id": 1, "method": "call", "params": {"fn": "sym.define", "args": {"text": "x"}}},
            {"jsonrpc": "2.0", "id": 2, "method": "engine.hello", "params": {"protocol_version": 1}},
            {"jsonrpc": "2.0", "id": 3, "method": "call", "params": {"fn": "sym.define", "args": {"text": "x"}}},
            {"jsonrpc": "2.0", "id": 4, "method": "nope"},
            {"jsonrpc": "2.0", "id": 5, "method": "state.get"},
            {"jsonrpc": "2.0", "id": 6, "method": "engine.shutdown"},
            {"jsonrpc": "2.0", "id": 7, "method": "state.get"},
        ]
    )
    assert code == 0
    assert out[0]["error"]["code"] == -32600  # hello must come first
    assert out[1]["result"]["protocol_version"] == 1
    assert out[2]["result"]["status"] == "ok"
    assert out[3]["error"]["code"] == -32601
    assert out[4]["result"]["revision"] == 0
    assert out[5]["result"] == {"ok": True}
    assert len(out) == 6  # nothing is served after shutdown


def test_protocol_version_checked():
    out, _ = _talk([{"jsonrpc": "2.0", "id": 1, "method": "engine.hello", "params": {"protocol_version": 2}}])
    assert out[0]["error"]["code"] == -32602
