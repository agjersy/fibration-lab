"""Entry point of the native diagram primitives (PROTOCOL.md sections 2.5 and 5.3)."""

from __future__ import annotations

from typing import Any

from ..content import ContentError
from .canvas import DEFAULT_MIN_PT, Canvas
from .hierarchy import draw_hierarchy
from .matrix import draw_matrix
from .process import draw_process
from .timeline import draw_timeline

_FAMILIES = {
    "process": (draw_process, ("chevrons", "steps", "flowchart")),
    "timeline": (draw_timeline, ("milestones", "swimlanes", "gantt")),
    "matrix": (draw_matrix, ("2x2", "grid", "kpi-tiles")),
    "hierarchy": (draw_hierarchy, ("org", "tree", "driver-tree")),
}


def add_diagram(slide: Any, model: dict[str, Any], box: tuple[int, int, int, int],
                min_font_pt: float = DEFAULT_MIN_PT) -> tuple[Any, list[str]]:
    """Draw ``model`` (a Diagram of the deck schema) inside ``box`` (x, y, cx, cy in EMU).

    Returns the single group shape holding every native shape and connector, and the ids of the
    nodes or items whose text does not fit at ``min_font_pt``.
    """
    family, variant = model.get("family"), model.get("variant")
    if family not in _FAMILIES:
        raise ContentError(f"unknown diagram family {family!r}")
    draw, variants = _FAMILIES[family]
    if variant not in variants:
        raise ContentError(f"unknown {family} variant {variant!r} (one of {', '.join(variants)})")
    if box[2] <= 0 or box[3] <= 0:
        raise ContentError("diagram box has no area")
    canvas = Canvas(slide, box, min_font_pt)
    try:
        draw(canvas, model)
    except Exception:
        el = canvas.group._element
        el.getparent().remove(el)
        raise
    return canvas.finish(), list(canvas.overflow)
