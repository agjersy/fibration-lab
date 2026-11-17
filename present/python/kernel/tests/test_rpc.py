from __future__ import annotations

import json
import subprocess
import sys


def _talk(tmp_path, lines):
    proc = subprocess.run(
        [sys.executable, "-m", "present_kernel", "--project", str(tmp_path), "--home", str(tmp_path / "home")],
        input="".join(json.dumps(m) + "\n" for m in lines).encode(), capture_output=True, timeout=120,
    )  # fmt: skip
    assert proc.returncode == 0, proc.stderr.decode()
    return {m["id"]: m for m in (json.loads(x) for x in proc.stdout.decode().splitlines() if x.strip()) if "id" in m}


def test_hello_and_shutdown_over_stdio(tmp_path):
    out = _talk(tmp_path, [
        {"jsonrpc": "2.0", "id": 1, "method": "kernel.hello", "params": {"core_version": "0", "protocol_version": 1}},
        {"jsonrpc": "2.0", "id": 2, "method": "no.such"},
        {"jsonrpc": "2.0", "id": 3, "method": "kernel.shutdown"},
    ])  # fmt: skip
    hello = out[1]["result"]
    assert hello["protocol_version"] == 1 and hello["schema_version"] == 1
    assert hello["packages"]["python-pptx"] != "missing"
    assert {"sandbox", "engines", "embedding"} <= set(hello)
    assert out[2]["error"]["code"] == -32601 and "detail" in out[2]["error"]["data"]
    assert out[3]["result"] is None


def test_wrong_protocol_version_is_rejected(tmp_path):
    out = _talk(tmp_path, [
        {"jsonrpc": "2.0", "id": 1, "method": "kernel.hello", "params": {"core_version": "0", "protocol_version": 99}},
        {"jsonrpc": "2.0", "id": 2, "method": "kernel.shutdown"},
    ])  # fmt: skip
    assert out[1]["error"]["code"] == -32602
