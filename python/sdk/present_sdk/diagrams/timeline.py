"""Timeline diagrams: milestones, swimlanes, Gantt."""

from __future__ import annotations

import itertools
from datetime import date
from typing import Any

from ..content import ContentError
from . import dates as D
from .canvas import ACCENTS, PAD, Canvas, Label, label


class _Axis:
    def __init__(self, model: dict[str, Any], items: list[dict[str, Any]]) -> None:
        spans = []
        for it in items:
            start = D.parse_start(it["start"])
            end = D.parse_end(it["end"]) if it.get("end") else None
            if end is not None and end <= start:
                raise ContentError(f"item {it['id']!r} ends before it starts")
            spans.append((start, end))
        self.spans = spans
        axis = model.get("axis") or {}
        if axis.get("from") and axis.get("to"):
            self.lo, self.hi = D.parse_start(axis["from"]), D.parse_end(axis["to"])
            if self.hi <= self.lo:
                raise ContentError("timeline axis ends before it starts")
            self.unit = axis.get("unit") or D.auto_unit(self.lo, self.hi)
        else:
            lo = min(s for s, _ in spans)
            hi = max(e or s for s, e in spans)
            self.unit = axis.get("unit") or D.auto_unit(lo, hi)
            self.lo, self.hi = D.floor_unit(lo, self.unit), D.ceil_unit(hi, self.unit)
            if self.hi <= self.lo or (self.hi == hi and any(e is None and s == hi for s, e in spans)):
                self.hi = D.next_unit(D.floor_unit(hi, self.unit), self.unit)
        if self.unit not in D.UNITS:
            raise ContentError(f"unknown axis unit {self.unit!r}")

    def frac(self, d: date) -> float:
        return min(1.0, max(0.0, (d - self.lo).days / (self.hi - self.lo).days))


def _items(model: dict[str, Any]) -> list[dict[str, Any]]:
    items = [i for i in model.get("items") or [] if isinstance(i, dict)]
    if not items:
        raise ContentError("timeline has no items")
    seen: set[str] = set()
    for k, it in enumerate(items):
        it.setdefault("id", f"i{k + 1}")
        if it["id"] in seen:
            raise ContentError(f"duplicate item id {it['id']!r}")
        seen.add(it["id"])
        if not it.get("start"):
            raise ContentError(f"item {it['id']!r} has no start date")
    return items


def _lanes(model: dict[str, Any], items: list[dict[str, Any]]) -> list[str]:
    lanes = [str(v) for v in model.get("lanes") or []]
    for it in items:
        name = str(it.get("lane") or "")
        if name not in lanes and (name or lanes or any(i.get("lane") for i in items)):
            lanes.append(name)
    return lanes


def draw_timeline(c: Canvas, model: dict[str, Any]) -> None:
    items = _items(model)
    axis = _Axis(model, items)
    variant = model["variant"]
    if variant == "milestones":
        _milestones(c, items, axis)
    elif variant == "swimlanes":
        _swimlanes(c, items, axis, _lanes(model, items))
    else:
        _gantt(c, items, axis, _lanes(model, items))


def _draw_axis(c: Canvas, axis: _Axis, px: float, pw: float, y: float, h: float, grid_to: float | None,
               cap: float, marks: float | None = None) -> None:
    """Interval labels in the band ``y..y+h``; grid lines down to ``grid_to``; tick marks at ``marks``."""
    chosen = None
    for max_ticks in (12, 8, 6, 4, 3, 2, 1):
        ticks, _ = D.ticks(axis.lo, axis.hi, axis.unit, max_ticks)
        cells = []
        for a, b in itertools.pairwise(ticks):
            x1, x2 = px + axis.frac(a) * pw, px + axis.frac(b) * pw
            cells.append((D.tick_label(a, axis.unit), x1, x2))
        if not cells:
            continue
        size, failed = c.best_size([("", label(t), int(x2 - x1), int(h)) for t, x1, x2 in cells], max_pt=cap, pad=PAD // 4)
        chosen = (ticks, cells, size)
        if not failed:
            break
    if chosen is None:
        return
    ticks, cells, size = chosen
    if grid_to is not None:
        for k, t in enumerate(ticks):
            x = px + axis.frac(t) * pw
            c.line(x, y, x, grid_to, f"grid-{k + 1}", color="dk2", width_pt=0.5, dash=True)
    for k, t in enumerate(ticks):
        x = px + axis.frac(t) * pw
        if marks is not None:
            c.line(x, y, x, y + h * 0.3, f"scale-mark-{k + 1}", color="dk2", width_pt=0.75)
            c.line(x, marks - h * 0.2, x, marks + h * 0.2, f"axis-mark-{k + 1}", color="dk2", width_pt=0.75)
    for k, (text, x1, x2) in enumerate(cells):
        c.textbox(x1, y, x2 - x1, h, f"tick-{k + 1}", label(text), size, color="dk2", pad=PAD // 4)


# ---- milestones ------------------------------------------------------------------------------

_MAX_TIERS = 6


def _tiers(xs: list[float], width: float, left: float, right: float):
    """Give each marker a side (0 above, 1 below), a tier, and a label span, without collisions.

    A label may sit centered on its marker or hang to either side of it, so that the leaders of
    neighboring markers can pass. Returns the placements and how many needed the relaxed rule.
    """
    placed: list[tuple[int, int, float, float, float]] = []  # side, tier, label left, label right, marker x
    out = []
    relaxed = 0
    for i, x in enumerate(xs):
        prefer = i % 2
        spans = []
        for shift, align in ((0.5, "center"), (0.88, "right"), (0.12, "left")):
            lo = min(max(x - width * shift, left), right - width)
            spans.append((lo, lo + width, align))

        def clash(side: int, tier: int, lo: float, hi: float, strict: bool, x: float = x) -> bool:
            for s, t, a, b, mx in placed:
                if s != side:
                    continue
                if t == tier and a < hi and b > lo:
                    return True
                if strict and ((t < tier and a <= x <= b) or (t > tier and lo <= mx <= hi)):
                    return True
            return False

        best = None
        for tier in range(_MAX_TIERS):
            for side in (prefer, 1 - prefer):
                for k, (lo, hi, align) in enumerate(spans):
                    if clash(side, tier, lo, hi, True):
                        continue
                    covered = sum(1 for j, other in enumerate(xs) if j != i and lo <= other <= hi)
                    key = (tier + covered, tier, side != prefer, k)
                    if best is None or key < best[0]:
                        best = (key, side, tier, lo, hi, align)
        if best is None:
            relaxed += 1
            tier = 0
            lo, hi, align = spans[0]
            while clash(prefer, tier, lo, hi, False):
                tier += 1
            best = (None, prefer, tier, lo, hi, align)
        _, side, tier, lo, hi, align = best
        placed.append((side, tier, lo, hi, x))
        out.append((side, tier, lo, align))
    return out, relaxed


def _milestones(c: Canvas, items: list[dict[str, Any]], axis: _Axis) -> None:
    d = int(min(max(c.cy * 0.045, 130000), 220000))
    band = int(min(max(c.cy * 0.06, 230000), 300000))
    order = sorted(range(len(items)), key=lambda i: (axis.spans[i][0], i))
    left, right = c.x + d / 2, c.x + c.cx - d / 2
    xs = [left + axis.frac(axis.spans[i][0]) * (right - left) for i in order]
    step = min(d * 1.2, (right - left) / max(len(xs) - 1, 1))
    for k in range(1, len(xs)):  # keep markers apart, then pull back inside the box
        xs[k] = max(xs[k], xs[k - 1] + step)
    if xs[-1] > right:
        xs[-1] = right
        for k in range(len(xs) - 2, -1, -1):
            xs[k] = min(xs[k], xs[k + 1] - step)
    labels = [Label().add(D.format_date(items[i]["start"]), bold=True).add(items[i].get("label", "")) for i in order]
    axis_y = c.y + (c.cy - band) / 2
    zones = ((c.y, axis_y - d / 2 - PAD), (axis_y + d / 2 + PAD, c.y + c.cy - band))
    best = None
    for k in (3, 4, 5, 6, 8, 10, 12):
        width = c.cx / k
        tiers, relaxed = _tiers(xs, width, c.x, c.x + c.cx)
        counts = [max((t + 1 for s, t, _, _ in tiers if s == side), default=1) for side in (0, 1)]
        heights = [(zones[side][1] - zones[side][0]) / counts[side] for side in (0, 1)]
        entries = [(items[i]["id"], lab, int(width), int(heights[s])) for i, lab, (s, _, _, _) in zip(order, labels, tiers)]
        size, failed = c.best_size(entries)
        key = (len(failed), relaxed, -size)
        if best is None or key < best[0]:
            best = (key, size, failed, width, tiers, heights)
    _, size, failed, width, tiers, heights = best
    c.overflow.extend(failed)
    c.line(c.x, axis_y, c.x + c.cx, axis_y, "axis", color="dk2", width_pt=2.0)
    boxes = []
    for i, x, (side, tier, lx, align) in zip(order, xs, tiers):
        h = heights[side]
        if side == 0:
            y = zones[0][1] - (tier + 1) * h
            y1, y2 = axis_y - d / 2, y + h
        else:
            y = zones[1][0] + tier * h
            y1, y2 = axis_y + d / 2, y
        if abs(y2 - y1) > 1:
            c.line(x, y1, x, y2, f"{items[i]['id']}-leader", color="dk2", width_pt=0.75)
        boxes.append((lx, y, h, side, align))
    scale_y = c.y + c.cy - band
    c.line(left, scale_y, right, scale_y, "scale", color="dk2", width_pt=0.75)
    _draw_axis(c, axis, left, right - left, scale_y, band, None, max(c.min_pt, size - 1), marks=axis_y)
    for i, x in zip(order, xs):
        c.shape("ellipse", x - d / 2, axis_y - d / 2, d, d, items[i]["id"], fill="accent1", line="lt1")
    for i, lab, (lx, y, h, side, align) in zip(order, labels, boxes):
        c.textbox(lx, y, width, h, f"{items[i]['id']}-label", lab, size, align=align, anchor="bottom" if side == 0 else "top")


# ---- swimlanes -------------------------------------------------------------------------------


def _header_height(c: Canvas) -> int:
    return int(min(max(c.cy * 0.07, 240000), 330000))


def _swim_plan(c: Canvas, items, axis: _Axis, lanes: list[str], px: float, pw: float, top: float, k: int):
    width = pw / k
    gap = pw * 0.01
    rows_of: list[list[list[tuple[int, float, float, float]]]] = []  # lane -> sub-rows -> (item, bar x, bar w, label x)
    for name in lanes or [""]:
        rows: list[list[tuple[int, float, float, float]]] = []
        ends: list[float] = []
        mine = [i for i, it in enumerate(items) if str(it.get("lane") or "") == name]
        for i in sorted(mine, key=lambda i: (axis.spans[i][0], i)):
            start, end = axis.spans[i]
            x1 = px + axis.frac(start) * pw
            x2 = px + axis.frac(end) * pw if end else x1
            lx = min(x1, px + pw - width)
            lo, hi = min(x1, lx), max(x2, lx + width) + gap
            r = next((k for k, e in enumerate(ends) if e <= lo), len(ends))
            if r == len(ends):
                ends.append(0.0)
                rows.append([])
            ends[r] = hi
            rows[r].append((i, x1, x2 - x1, lx))
        rows_of.append(rows or [[]])
    total = sum(len(rows) for rows in rows_of)
    lane_gap = c.cy * 0.015
    sub_h = min((c.y + c.cy - top - lane_gap * len(rows_of)) / total, 760000)
    text_h = sub_h * 0.66
    entries = [(items[i]["id"], label(items[i].get("label", "")), int(width), int(text_h))
               for rows in rows_of for row in rows for i, _, _, _ in row]  # fmt: skip
    size, failed = c.best_size(entries, pad=PAD // 2)
    return size, failed, (width, rows_of, sub_h, text_h, lane_gap)


def _swimlanes(c: Canvas, items: list[dict[str, Any]], axis: _Axis, lanes: list[str]) -> None:
    head_w = c.cx * 0.17 if lanes else 0
    head_h = _header_height(c)
    px, pw = c.x + head_w + (c.cx * 0.01 if lanes else 0), c.cx - head_w - (c.cx * 0.01 if lanes else 0)
    top = c.y + head_h
    best = None
    for k in (3, 4, 5, 6):
        plan = _swim_plan(c, items, axis, lanes, px, pw, top, k)
        if best is None or (len(plan[1]), -plan[0]) < (len(best[1]), -best[0]):
            best = plan
    size, failed, (width, rows_of, sub_h, text_h, lane_gap) = best
    c.overflow.extend(failed)
    names = lanes or [""]
    lane_boxes = []
    y = top
    for rows in rows_of:
        y += lane_gap
        lane_boxes.append((y, len(rows) * sub_h))
        y += len(rows) * sub_h
    lane_items = [(f"lane:{n}", label(n, bold=True), int(head_w), int(h)) for n, (_, h) in zip(names, lane_boxes)]
    lane_size = c.fit(lane_items, max_pt=size) if lanes else size
    for k, (name, (ly, lh)) in enumerate(zip(names, lane_boxes)):
        c.shape("rect", px, ly, pw, lh, f"lane-{k + 1}-band", fill="lt2")
        if lanes:
            head = c.shape("rect", c.x, ly, head_w, lh, f"lane-{k + 1}", fill=ACCENTS[k % len(ACCENTS)])
            c.text(head, label(name, bold=True), lane_size, color="lt1")
    _draw_axis(c, axis, px, pw, c.y, head_h, y, size)
    bar_h = sub_h * 0.2
    for k, (rows, (ly, _)) in enumerate(zip(rows_of, lane_boxes)):
        color = ACCENTS[k % len(ACCENTS)]
        for r, row in enumerate(rows):
            ry = ly + r * sub_h
            for i, x1, w, lx in row:
                it = items[i]
                if axis.spans[i][1] is None:
                    m = bar_h * 1.5
                    mx = min(max(x1 - m / 2, px), px + pw - m)
                    c.shape("diamond", mx, ry + sub_h * 0.07, m, m, it["id"], fill=color)
                else:
                    c.shape("roundRect", x1, ry + sub_h * 0.1, max(w, bar_h), bar_h, it["id"], fill=color, adj={"adj": 50000})
                c.textbox(lx, ry + sub_h * 0.34, width, text_h, f"{it['id']}-label", label(it.get("label", "")), size,
                          align="left", anchor="top", pad=PAD // 2)  # fmt: skip


# ---- gantt -----------------------------------------------------------------------------------


def _gantt(c: Canvas, items: list[dict[str, Any]], axis: _Axis, lanes: list[str]) -> None:
    order = list(range(len(items)))
    if lanes:
        order.sort(key=lambda i: (lanes.index(str(items[i].get("lane") or "")), i))
    gap = c.cx * 0.01
    lane_w = c.cx * 0.14 if lanes else 0
    label_w = c.cx * 0.25
    lx = c.x + (lane_w + gap if lanes else 0)
    px = lx + label_w + gap
    pw = c.x + c.cx - px
    head_h = _header_height(c)
    top = c.y + head_h
    row_h = min((c.cy - head_h) / len(items), 520000)
    bottom = top + row_h * len(items)
    size = c.fit([(items[i]["id"], label(items[i].get("label", "")), int(label_w), int(row_h)) for i in order], pad=PAD // 2)
    groups: list[tuple[str, int, int]] = []
    for r, i in enumerate(order):
        name = str(items[i].get("lane") or "")
        if lanes and groups and groups[-1][0] == name:
            groups[-1] = (name, groups[-1][1], r + 1)
        else:
            groups.append((name, r, r + 1))
    inset = row_h * 0.06
    if lanes:
        entries = [(f"lane:{n}", label(n, bold=True), int(lane_w), int((b - a) * row_h - 2 * inset)) for n, a, b in groups]
        lane_size = c.fit(entries, max_pt=size)
        for n, a, b in groups:
            k = lanes.index(n)
            head = c.shape("rect", c.x, top + a * row_h + inset, lane_w, (b - a) * row_h - 2 * inset, f"lane-{k + 1}",
                           fill=ACCENTS[k % len(ACCENTS)])  # fmt: skip
            c.text(head, label(n, bold=True), lane_size, color="lt1")
    for r in range(len(order) + 1):
        c.line(lx, top + r * row_h, c.x + c.cx, top + r * row_h, f"row-{r + 1}", color="lt2", width_pt=0.75)
    _draw_axis(c, axis, px, pw, c.y, head_h, bottom, size)
    bar_h = row_h * 0.52
    for r, i in enumerate(order):
        it = items[i]
        start, end = axis.spans[i]
        color = ACCENTS[lanes.index(str(it.get("lane") or "")) % len(ACCENTS)] if lanes else "accent1"
        y = top + r * row_h + (row_h - bar_h) / 2
        x1 = px + axis.frac(start) * pw
        if end is None:
            mx = min(max(x1 - bar_h / 2, px), px + pw - bar_h)
            c.shape("diamond", mx, y, bar_h, bar_h, it["id"], fill=color)
        else:
            w = max(px + axis.frac(end) * pw - x1, bar_h * 0.5)
            c.shape("roundRect", min(x1, px + pw - w), y, w, bar_h, it["id"], fill=color, adj={"adj": 25000})
        c.textbox(lx, top + r * row_h, label_w, row_h, f"{it['id']}-label", label(it.get("label", "")), size,
                  align="left", pad=PAD // 2)  # fmt: skip
