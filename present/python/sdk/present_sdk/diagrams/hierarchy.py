"""Hierarchy diagrams: org chart and tree (top-down), driver tree (left to right)."""

from __future__ import annotations

from typing import Any

from ..content import ContentError
from .canvas import BOTTOM, LEFT, PAD, RIGHT, TOP, Canvas, Label, label, text_area
from .process import pick


def _forest(model: dict[str, Any]):
    nodes = [n for n in model.get("nodes") or [] if isinstance(n, dict)]
    if not nodes:
        raise ContentError("hierarchy has no nodes")
    by_id: dict[str, dict[str, Any]] = {}
    for i, n in enumerate(nodes):
        n.setdefault("id", f"n{i + 1}")
        if n["id"] in by_id:
            raise ContentError(f"duplicate node id {n['id']!r}")
        by_id[n["id"]] = n
    kids: dict[str, list[str]] = {n["id"]: [] for n in nodes}
    roots = []
    for n in nodes:
        parent = n.get("parent")
        if parent in (None, ""):
            roots.append(n["id"])
        elif parent not in by_id or parent == n["id"]:
            raise ContentError(f"node {n['id']!r} has unknown parent {parent!r}")
        else:
            kids[parent].append(n["id"])
    depth: dict[str, int] = {}
    todo = [(r, 0) for r in reversed(roots)]
    while todo:
        v, d = todo.pop()
        depth[v] = d
        todo.extend((k, d + 1) for k in reversed(kids[v]))
    if len(depth) != len(nodes):
        raise ContentError("hierarchy parents form a cycle")
    return by_id, kids, roots, depth


def _node_label(n: dict[str, Any], with_value: bool) -> Label:
    extra = bool(n.get("detail")) or (with_value and n.get("value") not in (None, ""))
    lab = label(n.get("label", ""), bold=extra and not with_value)
    if with_value and n.get("value") not in (None, ""):
        lab.add(str(n["value"]), bold=True)
    return lab.add(n.get("detail"))


def _tidy(kids, roots, depth, stack: bool):
    """Slot coordinates: leaves take consecutive slots, parents sit over the middle of their children."""
    pos: dict[str, tuple[float, float]] = {}
    hung: set[str] = set()
    cursor = 0.0

    def place(v: str) -> None:
        nonlocal cursor
        ch = kids[v]
        if not ch:
            pos[v] = (cursor, depth[v])
            cursor += 1
        elif stack and len(ch) >= 3 and all(not kids[k] for k in ch):
            pos[v] = (cursor, depth[v])
            for i, k in enumerate(ch):
                pos[k] = (cursor + 0.62, depth[v] + 1 + i)
                hung.add(k)
            cursor += 1.62
        else:
            for k in ch:
                place(k)
            pos[v] = ((pos[ch[0]][0] + pos[ch[-1]][0]) / 2, depth[v])

    for r in roots:
        place(r)
    return pos, hung, cursor, max(y for _, y in pos.values()) + 1


def draw_hierarchy(c: Canvas, model: dict[str, Any]) -> None:
    by_id, kids, roots, depth = _forest(model)
    if model["variant"] == "driver-tree":
        _driver(c, by_id, kids, roots, depth)
    else:
        _top_down(c, by_id, kids, roots, depth, model["variant"] == "org")


def _down_plan(c: Canvas, by_id, kids, roots, depth, stack: bool):
    pos, hung, width, rows = _tidy(kids, roots, depth, stack)
    slot, pitch = c.cx / width, min(c.cy / rows, 1500000)
    w = min(slot * 0.9, 2300000)
    h = min(pitch * 0.66, max(w * 0.62, 480000), 900000)
    top = c.y + (c.cy - rows * pitch) / 2 + (pitch - h) / 2
    left = c.x + (slot - w) / 2
    rects = {v: (left + sx * slot, top + sy * pitch, w, h) for v, (sx, sy) in pos.items()}
    _, _, tw, th = text_area("roundRect", int(w), int(h), 12000)
    entries = [(v, _node_label(by_id[v], True), tw, th) for v in by_id]
    size, failed = c.best_size(entries, pad=PAD // 2)
    return size, failed, (rects, hung, pitch, h)


def _top_down(c: Canvas, by_id, kids, roots, depth, org: bool) -> None:
    plans = [_down_plan(c, by_id, kids, roots, depth, False)]
    if org:
        plans.append(_down_plan(c, by_id, kids, roots, depth, True))
    size, failed, (rects, hung, pitch, h) = pick(plans, gain=1.0)
    c.overflow.extend(failed)
    shapes = {}
    for v, n in by_id.items():
        x, y, w, _ = rects[v]
        top_level = depth[v] == 0
        shp = c.shape("roundRect" if org else "rect", x, y, w, h, v, fill="accent1" if top_level else "lt2",
                      line=None if top_level else "accent1", adj={"adj": 12000} if org else None)  # fmt: skip
        c.text(shp, _node_label(n, True), size, color="lt1" if top_level else "dk1", pad=PAD // 2)
        shapes[v] = shp
    for v in by_id:
        for k in kids[v]:
            if k in hung:
                c.connect(shapes[k], LEFT, shapes[v], BOTTOM, f"{v}-{k}", color="dk2", arrow=False)
            else:
                mid = rects[v][1] + h + (pitch - h) / 2
                c.connect(shapes[v], BOTTOM, shapes[k], TOP, f"{v}-{k}", mid=mid, color="dk2", arrow=False)


def _driver(c: Canvas, by_id, kids, roots, depth) -> None:
    pos, _, leaves, cols = _tidy(kids, roots, depth, False)
    col, pitch = c.cx / cols, min(c.cy / leaves, 1300000)
    w = min(col * 0.74, 2500000)
    h = min(pitch * 0.8, 850000)
    top = c.y + (c.cy - leaves * pitch) / 2 + (pitch - h) / 2
    left = c.x + (c.cx - (cols - 1) * col - w) / 2
    _, _, tw, th = text_area("roundRect", int(w), int(h), 12000)
    size = c.fit([(v, _node_label(n, True), tw, th) for v, n in by_id.items()])
    shapes = {}
    for v, n in by_id.items():
        sy, sx = pos[v]
        top_level = depth[v] == 0
        shp = c.shape("roundRect", left + sx * col, top + sy * pitch, w, h, v, fill="accent1" if top_level else "lt2",
                      line=None if top_level else "accent1", adj={"adj": 12000})  # fmt: skip
        c.text(shp, _node_label(n, True), size, color="lt1" if top_level else "dk1")
        shapes[v] = shp
    for v in by_id:
        for k in kids[v]:
            mid = left + depth[v] * col + w + (col - w) / 2
            c.connect(shapes[v], RIGHT, shapes[k], LEFT, f"{v}-{k}", mid=mid, color="dk2", arrow=False)
