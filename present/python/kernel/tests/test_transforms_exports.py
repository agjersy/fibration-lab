from __future__ import annotations

import pytest
from corpus import SALES

from present_kernel.errors import RpcError
from present_kernel.transforms import apply_steps

T = {"columns": ["Region", "Product", "Revenue", "Units"], "loc": "S!A1:D7", "rows": [
    ["EMEA", "A", 100.0, 10], ["EMEA", "B", 50.0, 5], ["AMER", "A", 200.0, 20],
    ["AMER", "B", 25.0, 2], ["APAC", "A", 75.0, 7], ["APAC", "B", 10.0, 1]]}  # fmt: skip


def run(*steps):
    return apply_steps(T, list(steps))


def test_group_sort_top_n_with_other():
    out = run({"op": "group_sum", "by": ["Region"], "columns": ["Revenue"]}, {"op": "top_n", "n": 2, "by": "Revenue", "other_label": "Other"})
    assert out["columns"] == ["Region", "Revenue"]
    assert out["rows"] == [["AMER", 225.0], ["EMEA", 150.0], ["Other", 85.0]]
    assert out["loc"] == "S!A1:D7"


def test_filter_select_rename_sort():
    out = run({"op": "filter", "column": "Product", "cmp": "==", "value": "A"}, {"op": "select", "columns": ["Region", "Revenue"]},
              {"op": "rename", "map": {"Revenue": "Rev"}}, {"op": "sort", "by": ["Rev"], "descending": True})  # fmt: skip
    assert out == {"columns": ["Region", "Rev"], "rows": [["AMER", 200.0], ["EMEA", 100.0], ["APAC", 75.0]], "loc": "S!A1:D7"}
    assert run({"op": "filter", "column": "Region", "cmp": "in", "value": ["APAC"]})["rows"][0][0] == "APAC"
    assert len(run({"op": "filter", "column": "Revenue", "cmp": ">=", "value": 75})["rows"]) == 3


def test_pivot_and_unpivot_round_trip():
    wide = run({"op": "pivot", "index": "Region", "columns": "Product", "values": "Revenue"})
    assert wide["columns"] == ["Region", "A", "B"] and wide["rows"][0] == ["EMEA", 100.0, 50.0]
    long = apply_steps(wide, [{"op": "unpivot", "id_columns": ["Region"], "var_name": "Product", "value_name": "Revenue"}])
    assert sorted(map(tuple, long["rows"])) == sorted((r[0], r[1], r[2]) for r in T["rows"])


def test_derived_columns():
    out = run({"op": "group_sum", "by": ["Region"], "columns": ["Revenue"]}, {"op": "share_of_total", "column": "Revenue", "as": "Share"},
              {"op": "cumulative", "column": "Revenue", "as": "Cum"}, {"op": "pct_change", "column": "Revenue", "as": "Chg"},
              {"op": "scale", "columns": ["Share"], "factor": 100}, {"op": "round", "columns": ["Share", "Chg"], "digits": 1})  # fmt: skip
    assert out["columns"] == ["Region", "Revenue", "Share", "Cum", "Chg"]
    assert out["rows"][0] == ["EMEA", 150.0, 32.6, 150.0, None]
    assert out["rows"][1] == ["AMER", 225.0, 48.9, 375.0, 0.5]


def test_group_mean_and_count():
    assert run({"op": "group_mean", "by": ["Product"], "columns": ["Units"]})["rows"][0] == ["A", pytest.approx(37 / 3)]
    assert run({"op": "group_count", "by": ["Region"]})["rows"] == [["EMEA", 2], ["AMER", 2], ["APAC", 2]]


def test_failures_name_the_step():
    with pytest.raises(RpcError) as err:
        run({"op": "sort", "by": ["Revenue"]}, {"op": "select", "columns": ["Nope"]})
    assert err.value.code == 2010 and err.value.extra["step_index"] == 1 and "Nope" in err.value.detail
    with pytest.raises(RpcError):
        run({"op": "explode"})


def test_determinism_and_keep():
    steps = [{"op": "group_sum", "by": ["Region"]}, {"op": "sort", "by": ["Revenue"]}]
    final, each = apply_steps(T, steps, keep=True)
    assert len(each) == 2 and each[-1]["rows"] == final["rows"]
    assert apply_steps(T, steps) == apply_steps(T, steps)


def test_chart_compute_rpc_inline(kernel_for, tmp_path):
    out = kernel_for(tmp_path).dispatcher.call("chart.compute", {"source": {"kind": "inline", "table": SALES, "cites": []},
                                                              "steps": [{"op": "top_n", "n": 1, "by": "Revenue"}]})  # fmt: skip
    assert out["table"]["rows"] == [["AMER", 200, 150, 0.25]] and len(out["step_tables"]) == 1


def test_outline_and_notes_exports(kernel_for, corp_project):
    k = kernel_for(corp_project)
    k.dispatcher.call("export.outline_md", {"deck": "deck/deck.json", "out": "exports/outline.md", "include_sources": True})
    md = (corp_project / "exports/outline.md").read_text(encoding="utf-8")
    assert md.startswith("# Corpus deck") and "## Part one" in md
    assert "### 2. Margin recovered to 18% on pricing actions" in md
    assert "  - Two thirds from list-price changes in EMEA" in md
    assert "> Pause on the EMEA point." in md and "Hidden backup slide" not in md
    assert "## Sources" in md and "1. `ref:r-q3rep@3f9a1c2b7e01#p11:c120-188`" in md
    k.dispatcher.call("export.notes_docx", {"deck": "deck/deck.json", "out": "exports/notes.docx", "include_sources": True})
    from docx import Document

    paras = [p.text for p in Document(str(corp_project / "exports/notes.docx")).paragraphs]
    assert "Pause on the EMEA point." in paras and any(p.startswith("2. Margin recovered") for p in paras)
