"""Matrix diagrams: 2x2 plot, score grid with Harvey balls, KPI tiles."""

from __future__ import annotations

import math
from typing import Any

from ..content import ContentError, as_text
from .canvas import PAD, Canvas, Label, label, text_area
from .process import pick


def _cells(model: dict[str, Any]) -> list[dict[str, Any]]:
    cells = [dict(cell) for cell in model.get("cells") or [] if isinstance(cell, dict)]
    if not cells:
        raise ContentError("matrix has no cells")
    seen: set[str] = set()
    for i, cell in enumerate(cells):
        cell["id"] = str(cell.get("id") or f"cell-{i + 1}")
        if cell["id"] in seen:
            raise ContentError(f"duplicate cell id {cell['id']!r}")
        seen.add(cell["id"])
    return cells


def draw_matrix(c: Canvas, model: dict[str, Any]) -> None:
    cells = _cells(model)
    variant = model["variant"]
    if variant == "2x2":
        _two_by_two(c, cells, model.get("axes") or {})
    elif variant == "grid":
        _grid(c, cells, [str(r) for r in model.get("rows") or []], [str(v) for v in model.get("columns") or []])
    else:
        _tiles(c, cells)


# ---- 2x2 -------------------------------------------------------------------------------------


def _overlap(a: tuple[float, float, float, float], b: tuple[float, float, float, float]) -> float:
    w = min(a[0] + a[2], b[0] + b[2]) - max(a[0], b[0])
    h = min(a[1] + a[3], b[1] + b[3]) - max(a[1], b[1])
    return w * h if w > 0 and h > 0 else 0.0


def _spread(points: list[list[float]], d: float, box: tuple[float, float, float, float]) -> None:
    """Push coincident dots apart (deterministic relaxation) and keep them inside the plot."""
    x0, y0, w, h = box
    need = d * 1.15
    for _ in range(40):
        moved = False
        for i in range(len(points)):
            for j in range(i + 1, len(points)):
                dx, dy = points[j][0] - points[i][0], points[j][1] - points[i][1]
                dist = math.hypot(dx, dy)
                if dist >= need:
                    continue
                if dist < 1:
                    dx, dy, dist = 1.0, 0.0, 1.0
                push = (need - dist) / 2 + 1
                points[i][0] -= dx / dist * push
                points[i][1] -= dy / dist * push
                points[j][0] += dx / dist * push
                points[j][1] += dy / dist * push
                moved = True
        for p in points:
            p[0] = min(max(p[0], x0 + d / 2), x0 + w - d / 2)
            p[1] = min(max(p[1], y0 + d / 2), y0 + h - d / 2)
        if not moved:
            break


def _place_labels(c: Canvas, cells, points, d: float, plot, size: float, force: bool):
    """Greedy label boxes around the dots; ``None`` when one cannot be placed without overlap."""
    px, py, pw, ph = plot
    pad = PAD // 4
    gap = d * 0.25
    dots = [(p[0] - d / 2, p[1] - d / 2, d, d) for p in points]
    placed: list[tuple[float, float, float, float]] = []
    failed: list[str] = []
    for i, cell in enumerate(cells):
        lab = label(cell.get("label", ""))
        if lab.empty:
            placed.append((points[i][0], points[i][1], 0.0, 0.0))
            continue
        w = min(max(c.word_width(lab, size, pad), min(c.width_of(lab, size, pad), pw * 0.26)), pw * 0.45)
        h = c.height_of(lab, int(w), size, pad)
        x, y = points[i]
        r = d / 2 + gap
        spots = [(x + r, y - h / 2), (x - r - w, y - h / 2), (x - w / 2, y - r - h), (x - w / 2, y + r),
                 (x + r, y - r - h), (x + r, y + r), (x - r - w, y - r - h), (x - r - w, y + r)]  # fmt: skip
        best = None
        for k, (bx, by) in enumerate(spots):
            inside = bx >= px and by >= py and bx + w <= px + pw and by + h <= py + ph
            bx, by = min(max(bx, px), px + pw - w), min(max(by, py), py + ph - h)
            box = (bx, by, w, h)
            cost = sum(_overlap(box, o) for o in placed) + sum(_overlap(box, o) for j, o in enumerate(dots) if j != i)
            cost += 0 if inside else _overlap(box, dots[i]) + 1
            if best is None or cost < best[0]:
                best = (cost, k, box)
            if cost == 0:
                break
        if best[0] > 0 or not c.fits(lab, int(w), int(h), size, pad):
            if not force:
                return None
            failed.append(cell["id"])
        placed.append(best[2])
    return placed, failed


def _two_by_two(c: Canvas, cells: list[dict[str, Any]], axes: dict[str, Any]) -> None:
    for cell in cells:
        for key in ("x", "y"):
            v = cell.get(key)
            if not isinstance(v, (int, float)) or isinstance(v, bool) or not 0 <= v <= 1:
                raise ContentError(f"2x2 item {cell['id']!r} needs {key} in 0..1")
    has_x = any(axes.get(k) for k in ("x", "x_low", "x_high"))
    has_y = any(axes.get(k) for k in ("y", "y_low", "y_high"))
    axis_pt = max(c.min_pt, min(c.max_pt, 14.0))
    strip = int(c.measurer.line_height_pt(c.font, axis_pt, True) * 12700) + 2 * PAD
    px, py = c.x + (strip if has_y else 0), c.y
    pw, ph = c.cx - (strip if has_y else 0), c.cy - (strip if has_x else 0)
    seam = int(min(pw, ph) * 0.012)
    quads = [(px, py), (px + pw / 2 + seam / 2, py), (px, py + ph / 2 + seam / 2), (px + pw / 2 + seam / 2, py + ph / 2 + seam / 2)]
    for k, (qx, qy) in enumerate(quads):
        c.shape("rect", qx, qy, pw / 2 - seam / 2, ph / 2 - seam / 2, f"quadrant-{k + 1}", fill="lt2")
    parts = []  # name, text, bold, align, horizontal?, x, y, w, h
    if has_x:
        y = py + ph
        parts += [("x-low", axes.get("x_low"), False, "left", True, px, y, pw / 3, strip),
                  ("x-title", axes.get("x"), True, "center", True, px + pw / 3, y, pw / 3, strip),
                  ("x-high", axes.get("x_high"), False, "right", True, px + 2 * pw / 3, y, pw / 3, strip)]  # fmt: skip
    if has_y:
        parts += [("y-high", axes.get("y_high"), False, "right", False, c.x, py, strip, ph / 3),
                  ("y-title", axes.get("y"), True, "center", False, c.x, py + ph / 3, strip, ph / 3),
                  ("y-low", axes.get("y_low"), False, "left", False, c.x, py + 2 * ph / 3, strip, ph / 3)]  # fmt: skip
    parts = [p for p in parts if p[1]]
    entries = [(f"axis:{n}", label(t, bold=b), int(w if hz else h), int(h if hz else w)) for n, t, b, _, hz, _, _, w, h in parts]
    a_size = c.fit(entries, max_pt=axis_pt) if entries else axis_pt
    for n, t, b, align, hz, x, y, w, h in parts:
        c.textbox(x, y, w, h, n, label(t, bold=b), a_size, color="dk2", align=align, vert=None if hz else "vert270")
    d = min(max(ph * 0.04, 120000), 180000)
    points = [[px + cell["x"] * pw, py + (1 - cell["y"]) * ph] for cell in cells]
    plot = (px, py, pw, ph)
    _spread(points, d, plot)
    size = min(c.max_pt, 16.0)
    result = None
    while size >= c.min_pt:
        result = _place_labels(c, cells, points, d, plot, size, False)
        if result:
            break
        size -= 1.0
    if not result:
        size = c.min_pt
        result = _place_labels(c, cells, points, d, plot, size, True)
    boxes, failed = result
    c.overflow.extend(failed)
    for cell, (x, y) in zip(cells, points):
        c.shape("ellipse", x - d / 2, y - d / 2, d, d, cell["id"], fill="accent1", line="lt1")
    for cell, (bx, by, w, h) in zip(cells, boxes):
        if w > 0:
            c.textbox(bx, by, w, h, f"{cell['id']}-label", label(cell.get("label", "")), size, align="left", pad=PAD // 4)


# ---- grid ------------------------------------------------------------------------------------

_PIE_END = {1: 0, 2: 5400000, 3: 10800000}


def _cell_label(cell: dict[str, Any], value_scale: float = 1.0, value_color: str | None = None) -> Label:
    lab = Label()
    if cell.get("value") not in (None, ""):
        run: dict[str, Any] = {"t": str(cell["value"])}
        if value_color:
            run["color"] = value_color
        lab.add({"paragraphs": [{"runs": [run]}]}, bold=True, scale=value_scale)
    return lab.add(as_text(cell.get("label") or ""))


def _grid(c: Canvas, cells: list[dict[str, Any]], rows: list[str], cols: list[str]) -> None:
    taken: dict[tuple[int, int], dict[str, Any]] = {}
    loose = []
    for cell in cells:
        r, k = cell.get("row"), cell.get("col")
        if isinstance(r, int) and isinstance(k, int) and r >= 0 and k >= 0 and not isinstance(r, bool):
            if (r, k) in taken:
                raise ContentError(f"grid cell ({r}, {k}) is given twice")
            taken[(r, k)] = cell
        elif r is None and k is None:
            loose.append(cell)
        else:
            raise ContentError(f"grid cell {cell['id']!r} needs both row and col (0-based) or neither")
    n_cols = max([len(cols)] + [k + 1 for _, k in taken])
    if n_cols == 0:
        n_cols = math.ceil(math.sqrt(len(cells) * 1.6))
    slot = 0
    for cell in loose:
        while divmod(slot, n_cols) in taken:
            slot += 1
        taken[divmod(slot, n_cols)] = cell
    n_rows = max([len(rows)] + [r + 1 for r, _ in taken])
    gap = int(min(c.cx, c.cy) * 0.015)
    head_w = int(c.cx * 0.2) if rows else 0
    head_h = int(min(c.cy * 0.14, 420000)) if cols else 0
    gx, gy = c.x + (head_w + gap if rows else 0), c.y + (head_h + gap if cols else 0)
    cw = (c.x + c.cx - gx - gap * (n_cols - 1)) / n_cols
    ch = min((c.y + c.cy - gy - gap * (n_rows - 1)) / n_rows, 1300000)
    ball = min(ch * 0.5, cw * 0.3, 330000)
    entries = []
    geom = {}
    for (r, k), cell in sorted(taken.items()):
        lab = _cell_label(cell)
        scored = isinstance(cell.get("score"), int) and not isinstance(cell.get("score"), bool)
        if scored and not 0 <= cell["score"] <= 4:
            raise ContentError(f"grid cell {cell['id']!r}: score must be 0..4")
        left = int(ball + 2 * PAD) if scored and not lab.empty else PAD
        geom[(r, k)] = (lab, scored, left)
        _, _, tw, th = text_area("roundRect", int(cw), int(ch), 8000)
        entries.append((cell["id"], lab, int(tw - left + PAD), th))
    entries += [(f"row:{i}", label(t, bold=True), head_w, int(ch)) for i, t in enumerate(rows)]
    entries += [(f"col:{i}", label(t, bold=True), int(cw), head_h) for i, t in enumerate(cols)]
    size = c.fit(entries)
    for i, t in enumerate(cols):
        head = c.shape("rect", gx + i * (cw + gap), c.y, cw, head_h, f"col-{i + 1}", fill="accent1")
        c.text(head, label(t, bold=True), size, color="lt1")
    for i, t in enumerate(rows):
        head = c.shape("rect", c.x, gy + i * (ch + gap), head_w, ch, f"row-{i + 1}", fill="dk2")
        c.text(head, label(t, bold=True), size, color="lt1")
    for r in range(n_rows):
        for k in range(n_cols):
            x, y = gx + k * (cw + gap), gy + r * (ch + gap)
            if (r, k) not in taken:
                c.shape("roundRect", x, y, cw, ch, f"empty-{r + 1}-{k + 1}", fill="lt2", adj={"adj": 8000})
                continue
            cell = taken[(r, k)]
            lab, scored, left = geom[(r, k)]
            box = c.shape("roundRect", x, y, cw, ch, cell["id"], fill="lt2", adj={"adj": 8000})
            if not lab.empty:
                c.text(box, lab, size, align="left" if scored else "center", pad=(left, PAD, PAD, PAD))
            if scored:
                bx = x + PAD if not lab.empty else x + (cw - ball) / 2
                by = y + (ch - ball) / 2
                score = cell["score"]
                c.shape("ellipse", bx, by, ball, ball, f"{cell['id']}-ball", fill="accent1" if score == 4 else "lt1",
                        line="accent1", line_pt=1.25)  # fmt: skip
                if score in _PIE_END:
                    c.shape("pie", bx, by, ball, ball, f"{cell['id']}-score", fill="accent1",
                            adj={"adj1": 16200000, "adj2": _PIE_END[score]})  # fmt: skip


# ---- KPI tiles -------------------------------------------------------------------------------

_VALUE_SCALE = 2.0


def _tile_plan(c: Canvas, cells: list[dict[str, Any]], rows: int):
    per_row = math.ceil(len(cells) / rows)
    gap = int(min(c.cx, c.cy) * 0.03)
    w = (c.cx - gap * (per_row - 1)) / per_row
    h = min((c.cy - gap * (rows - 1)) / rows, w * 0.8, 2000000)
    top = c.y + (c.cy - rows * h - (rows - 1) * gap) / 2
    spots = []
    for i, cell in enumerate(cells):
        r, k = divmod(i, per_row)
        spots.append((cell, c.x + k * (w + gap), top + r * (h + gap)))
    _, _, tw, th = text_area("roundRect", int(w), int(h), 8000)
    size, failed = c.best_size([(cell["id"], _cell_label(cell, _VALUE_SCALE), tw, th) for cell in cells])
    return size, failed, (spots, w, h)


def _tiles(c: Canvas, cells: list[dict[str, Any]]) -> None:
    n = len(cells)
    base = max(1, math.ceil(n / 5))
    plans = [_tile_plan(c, cells, r) for r in range(base, min(n, base + 2) + 1)]
    size, failed, (spots, w, h) = pick(plans, gain=2.0)
    c.overflow.extend(failed)
    for cell, x, y in spots:
        tile = c.shape("roundRect", x, y, w, h, cell["id"], fill="lt2", line="accent1", adj={"adj": 8000})
        c.text(tile, _cell_label(cell, _VALUE_SCALE, "accent1"), size)
