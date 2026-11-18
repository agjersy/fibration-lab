"""Named results (``analyst_results.record``, PROTOCOL.md 1.7): written by the script, listed by the kernel."""

from __future__ import annotations

import json
import math
from pathlib import Path

import pytest
from conftest import Notes
from test_runs import write_script

from analyst_kernel.kernel import Kernel
from analyst_results import load, record


def test_record_writes_and_replaces(tmp_path: Path, monkeypatch):
    monkeypatch.setenv("ANALYST_OUT_DIR", str(tmp_path / "out"))
    assert record("tau", 1.82, unit="s", uncertainty=0.04) == {"name": "tau", "value": 1.82, "unit": "s", "uncertainty": 0.04}
    record("n", 50)
    record("tau", 1.9, unit="s")
    data = json.loads((tmp_path / "out" / "results.json").read_text())
    assert data["schema"] == 1
    assert load() == [{"name": "n", "value": 50.0}, {"name": "tau", "value": 1.9, "unit": "s"}]
    assert not list((tmp_path / "out").glob("*.tmp"))


@pytest.mark.parametrize("args,exc", [
    (("", 1.0), ValueError), (("a b", 1.0), ValueError), (("x", "1.0"), TypeError), (("x", True), TypeError),
    (("x", math.nan), ValueError), (("x", math.inf), ValueError), (("x", 1.0, None, -1.0), ValueError),
])
def test_record_refuses_what_cannot_be_checked(tmp_path: Path, monkeypatch, args, exc):
    monkeypatch.setenv("ANALYST_OUT_DIR", str(tmp_path / "out"))
    with pytest.raises(exc):
        record(*args)


def test_the_kernel_lists_results_as_an_artifact(kernel: Kernel, repo: Path, notes: Notes):
    script = write_script(repo, "named.py", """
from analyst_results import record
record("tau", 1.8234, unit="s", uncertainty=0.05)
record("margin", 0.183)
print("ok")
""")
    res = kernel.call("run.start", {"script": script, "args": []})
    run_id = res["run_id"]
    done = notes.wait_for("run.done", run_id)[0]
    assert done["status"] == "done", done
    arts = [a["artifact"] for a in notes.of("run.artifact", run_id)]
    results = [a for a in arts if a["path"].endswith("/results.json")]
    assert results and results[-1]["kind"] == "results" and results[-1]["rendered"] is True
    assert results[-1]["path"] == f"{res['out_dir']}/results.json"
    names = [r["name"] for r in load(repo / res["out_dir"] / "results.json")]
    assert names == ["tau", "margin"]
