"""Declarative chart and table transforms (PROTOCOL.md section 2.6): a closed, deterministic set."""

from __future__ import annotations

import math
from typing import Any

import pandas as pd

from ..errors import TRANSFORM_FAILED, RpcError

_CMP = {
    "==": lambda s, v: s == v, "!=": lambda s, v: s != v, "<": lambda s, v: s < v, "<=": lambda s, v: s <= v,
    ">": lambda s, v: s > v, ">=": lambda s, v: s >= v,
    "in": lambda s, v: s.isin(v if isinstance(v, list) else [v]),
    "not_in": lambda s, v: ~s.isin(v if isinstance(v, list) else [v]),
    "contains": lambda s, v: s.astype(str).str.contains(str(v), regex=False),
}  # fmt: skip


def to_frame(table: dict[str, Any]) -> pd.DataFrame:
    df = pd.DataFrame(table.get("rows", []), columns=list(table.get("columns", [])))
    for col in df.columns:
        if df[col].dtype == object:
            converted = pd.to_numeric(df[col], errors="coerce")
            if converted.notna().sum() == df[col].notna().sum() and df[col].notna().any():
                df[col] = converted
    return df


def from_frame(df: pd.DataFrame, loc: str = "") -> dict[str, Any]:
    rows = []
    for record in df.itertuples(index=False, name=None):
        row = []
        for v in record:
            if v is None or (isinstance(v, float) and math.isnan(v)) or v is pd.NA:
                row.append(None)
            elif hasattr(v, "item"):
                v = v.item()
                row.append(v)
            else:
                row.append(v)
        rows.append(row)
    return {"columns": [str(c) for c in df.columns], "rows": rows, "loc": loc}


def _need(df: pd.DataFrame, *cols: str) -> None:
    missing = [c for c in cols if c not in df.columns]
    if missing:
        raise KeyError(f"no column {missing[0]!r} (columns: {', '.join(map(str, df.columns))})")


def _step(df: pd.DataFrame, step: dict[str, Any]) -> pd.DataFrame:
    op = step.get("op")
    if op == "select":
        _need(df, *step["columns"])
        return df[step["columns"]]
    if op == "rename":
        _need(df, *step["map"].keys())
        return df.rename(columns=step["map"])
    if op == "filter":
        _need(df, step["column"])
        return df[_CMP[step["cmp"]](df[step["column"]], step.get("value"))]
    if op == "sort":
        _need(df, *step["by"])
        return df.sort_values(step["by"], ascending=not step.get("descending", False), kind="stable")
    if op == "top_n":
        _need(df, step["by"])
        ordered = df.sort_values(step["by"], ascending=False, kind="stable")
        top, rest = ordered.head(int(step["n"])), ordered.iloc[int(step["n"]):]
        if step.get("other_label") and len(rest):
            other = {c: (rest[c].sum() if pd.api.types.is_numeric_dtype(rest[c]) else None) for c in df.columns}
            label_col = next((c for c in df.columns if not pd.api.types.is_numeric_dtype(df[c])), df.columns[0])
            other[label_col] = step["other_label"]
            top = pd.concat([top, pd.DataFrame([other], columns=df.columns)], ignore_index=True)
        return top
    if op in ("group_sum", "group_mean", "group_count"):
        _need(df, *step["by"])
        cols = step.get("columns") or [c for c in df.columns if c not in step["by"] and pd.api.types.is_numeric_dtype(df[c])]
        _need(df, *cols)
        grouped = df.groupby(step["by"], sort=False, dropna=False)
        if op == "group_count":
            return grouped.size().reset_index(name="count")
        agg = grouped[cols].sum() if op == "group_sum" else grouped[cols].mean()
        return agg.reset_index()
    if op == "pivot":
        _need(df, step["index"], step["columns"], step["values"])
        out = df.pivot_table(index=step["index"], columns=step["columns"], values=step["values"],
                             aggfunc=step.get("agg", "sum"), sort=False)  # fmt: skip
        out.columns = [str(c) for c in out.columns]
        return out.reset_index()
    if op == "unpivot":
        _need(df, *step["id_columns"])
        return df.melt(id_vars=step["id_columns"], var_name=step["var_name"], value_name=step["value_name"])
    if op in ("pct_change", "share_of_total", "cumulative"):
        _need(df, step["column"])
        col = step["column"]
        out = df.copy()
        if op == "pct_change":
            out[step.get("as") or f"{col}_pct_change"] = out[col].pct_change(periods=int(step.get("periods", 1)))
        elif op == "share_of_total":
            total = out[col].sum()
            out[step.get("as") or f"{col}_share"] = out[col] / total if total else float("nan")
        else:
            out[step.get("as") or f"{col}_cumulative"] = out[col].cumsum()
        return out
    if op == "scale":
        _need(df, *step["columns"])
        out = df.copy()
        for c in step["columns"]:
            out[c] = out[c] * float(step["factor"])
        return out
    if op == "round":
        _need(df, *step["columns"])
        out = df.copy()
        for c in step["columns"]:
            out[c] = out[c].round(int(step["digits"]))
        return out
    raise ValueError(f"unknown transform op {op!r}")


def apply_steps(table: dict[str, Any], steps: list[dict[str, Any]], keep: bool = False) -> Any:
    """Apply the steps in order. With ``keep`` also return the table after each step."""
    df = to_frame(table)
    intermediate = []
    for i, step in enumerate(steps or []):
        try:
            df = _step(df, step).reset_index(drop=True)
        except RpcError:
            raise
        except Exception as exc:
            raise RpcError(TRANSFORM_FAILED, f"step {i + 1} ({step.get('op')}): {exc}", step_index=i) from exc
        if keep:
            intermediate.append(from_frame(df))
    final = from_frame(df, table.get("loc", ""))
    return (final, intermediate) if keep else final
