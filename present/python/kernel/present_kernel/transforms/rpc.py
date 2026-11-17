"""``chart.compute``: resolve a chart source and run its transform pipeline."""

from __future__ import annotations

from typing import Any

from ..errors import NOT_FOUND, RpcError, invalid_params
from ..methods import group
from . import apply_steps


@group
def _register(kernel) -> None:
    def chart_compute(params: dict[str, Any]) -> dict[str, Any]:
        source = params.get("source")
        if not isinstance(source, dict):
            raise invalid_params("source is required")
        if source.get("kind") == "inline":
            table = source.get("table") or {}
        elif source.get("kind") == "ref":
            try:
                from ..refs.reader import ProjectRefs
            except ImportError as exc:
                raise RpcError(NOT_FOUND, "reference reading is not available") from exc
            try:
                table = ProjectRefs(kernel.project).table(source["cite"])
            except RpcError:
                raise
            except Exception as exc:
                raise RpcError(NOT_FOUND, f"{source.get('cite')}: {exc}") from exc
        else:
            raise invalid_params("source.kind must be ref or inline")
        final, steps = apply_steps(table, params.get("steps") or [], keep=True)
        return {"table": final, "step_tables": steps}

    kernel.dispatcher.register("chart.compute", chart_compute)
