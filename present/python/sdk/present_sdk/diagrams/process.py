"""Process diagrams: chevrons, numbered steps, flowchart."""

from __future__ import annotations

import itertools
import math
from typing import Any

from ..content import ContentError
from .canvas import BOTTOM, LEFT, PAD, RIGHT, TOP, Canvas, Label, label, text_area


def node_label(node: dict[str, Any], with_detail: bool = True) -> Label:
    detail = node.get("detail") if with_detail else None
    lab = label(node.get("label", ""), bold=bool(detail))
    return lab.add(detail) if detail else lab


def _nodes(model: dict[str, Any]) -> list[dict[str, Any]]:
    nodes = [n for n in model.get("nodes") or [] if isinstance(n, dict)]
    if not nodes:
        raise ContentError("process diagram has no nodes")
    seen: set[str] = set()
    for i, n in enumerate(nodes):
        n.setdefault("id", f"n{i + 1}")
        if n["id"] in seen:
            raise ContentError(f"duplicate node id {n['id']!r}")
        seen.add(n["id"])
    return nodes


def draw_process(c: Canvas, model: dict[str, Any]) -> None:
    nodes = _nodes(model)
    variant = model["variant"]
    if variant == "chevrons":
        _chevrons(c, nodes)
    elif variant == "steps":
        _steps(c, nodes)
    else:
        _flowchart(c, nodes, model.get("edges") or [])


def pick(candidates: list[tuple[float, list[str], Any]], gain: float = 1.5) -> tuple[float, list[str], Any]:
    """First candidate, unless a later one overflows less or fits at a clearly larger size."""
    best = candidates[0]
    for cand in candidates[1:]:
        if len(cand[1]) < len(best[1]) or (len(cand[1]) == len(best[1]) and cand[0] >= best[0] + gain):
            best = cand
    return best


# ---- chevrons --------------------------------------------------------------------------------


def _chevron_plan(c: Canvas, nodes: list[dict[str, Any]], rows: int) -> tuple[float, list[str], Any]:
    n = len(nodes)
    per_row = math.ceil(n / rows)
    has_detail = any(nd.get("detail") for nd in nodes)
    gap = int(c.cx * 0.008)
    vgap = int(c.cy * 0.05)
    w = (c.cx - gap * (per_row - 1)) // per_row
    row_h = (c.cy - vgap * (rows - 1)) // rows
    h = int(min(row_h * (0.42 if has_detail else 1.0), w * 0.62, 1100000))
    depth = int(min(h * 0.32, w * 0.16))
    adj = int(depth / max(min(w, h), 1) * 100000)
    detail_h = int(min(row_h - h - PAD, 1500000)) if has_detail else 0
    block_h = h + (detail_h + PAD if has_detail else 0)
    top = c.y + (c.cy - (rows * block_h + (rows - 1) * vgap)) // 2
    items, cells = [], []
    for i, nd in enumerate(nodes):
        r, k = divmod(i, per_row)
        prst = "homePlate" if k == 0 else "chevron"
        x, y = c.x + k * (w + gap), top + r * (block_h + vgap)
        _, _, tw, th = text_area(prst, w, h, adj)
        items.append((nd["id"], label(nd.get("label", ""), bold=True), tw, th))
        if nd.get("detail"):
            items.append((nd["id"], label(nd["detail"]), w, detail_h))
        cells.append((nd, prst, x, y))
    size, failed = c.best_size(items)
    return size, failed, (cells, w, h, adj, detail_h)


def _chevrons(c: Canvas, nodes: list[dict[str, Any]]) -> None:
    max_rows = min(4, len(nodes))
    size, failed, (cells, w, h, adj, detail_h) = pick([_chevron_plan(c, nodes, r) for r in range(1, max_rows + 1)])
    c.overflow.extend(i for i in dict.fromkeys(failed))
    for nd, prst, x, y in cells:
        shp = c.shape(prst, x, y, w, h, nd["id"], fill="accent1", adj={"adj": adj})
        c.text(shp, label(nd.get("label", ""), bold=True), size, color="lt1")
        if nd.get("detail"):
            c.textbox(x, y + h + PAD, w, detail_h, f"{nd['id']}-detail", label(nd["detail"]), size, anchor="top")


# ---- steps -----------------------------------------------------------------------------------


def _steps_plan(c: Canvas, nodes: list[dict[str, Any]], rows: int) -> tuple[float, list[str], Any]:
    n = len(nodes)
    per_row = math.ceil(n / rows)
    gap = int(min(c.cx * 0.035, c.cx / per_row * 0.16))
    vgap = int(c.cy * 0.06)
    w = (c.cx - gap * (per_row - 1)) // per_row
    row_h = (c.cy - vgap * (rows - 1)) // rows
    h = int(min(row_h, w * 0.95))
    badge = int(max(270000, min(w, h) * 0.2))
    badge = min(badge, 460000, h // 2)
    top = c.y + (c.cy - (rows * h + (rows - 1) * vgap)) // 2
    text_top = badge + PAD
    items, cells = [], []
    for i, nd in enumerate(nodes):
        r, k = divmod(i, per_row)
        cells.append((nd, c.x + k * (w + gap), top + r * (h + vgap), k))
        _, _, tw, th = text_area("roundRect", int(w), int(h), 8000)
        items.append((nd["id"], node_label(nd), tw, th - text_top))
    size, failed = c.best_size(items)
    return size, failed, (cells, w, h, badge, text_top)


def _steps(c: Canvas, nodes: list[dict[str, Any]]) -> None:
    n = len(nodes)
    base = max(1, math.ceil(n / 5))
    plans = [_steps_plan(c, nodes, r) for r in range(base, min(n, base + 1) + 1)]
    size, failed, (cells, w, h, badge, text_top) = pick(plans, gain=2.0)
    c.overflow.extend(i for i in dict.fromkeys(failed))
    inner = text_area("ellipse", badge, badge)
    num_size, _ = c.best_size([("", label(str(n), bold=True), inner[2], inner[3])], max_pt=size, pad=0)
    prev = None
    for i, (nd, x, y, k) in enumerate(cells):
        card = c.shape("roundRect", x, y, w, h, nd["id"], fill="lt2", line="accent1", adj={"adj": 8000})
        c.text(card, node_label(nd), size, anchor="top", pad=(PAD, text_top + PAD, PAD, PAD))
        dot = c.shape("ellipse", x + PAD, y + PAD, badge, badge, f"{nd['id']}-badge", fill="accent1")
        c.text(dot, label(str(i + 1), bold=True), num_size, color="lt1", pad=0)
        if prev is not None and k > 0:
            c.connect(prev, RIGHT, card, LEFT, f"{cells[i - 1][0]['id']}-{nd['id']}", color="accent1", width_pt=1.5)
        prev = card


# ---- flowchart -------------------------------------------------------------------------------

_KIND_PRST = {"step": "rect", "decision": "diamond", "start": "flowChartTerminator", "end": "flowChartTerminator"}


def _graph(nodes: list[dict[str, Any]], edges: list[dict[str, Any]]):
    """Edges cleaned (known ends, no self-loops, no duplicates) and the set that closes cycles."""
    ids = [n["id"] for n in nodes]
    known = set(ids)
    clean: list[tuple[str, str, Any]] = []
    seen: set[tuple[str, str]] = set()
    for e in edges:
        u, v = e.get("from"), e.get("to")
        if u not in known or v not in known:
            raise ContentError(f"flowchart edge {u!r} -> {v!r} names an unknown node")
        if u == v or (u, v) in seen:
            continue
        seen.add((u, v))
        clean.append((u, v, e.get("label")))
    out: dict[str, list[str]] = {i: [] for i in ids}
    indeg = dict.fromkeys(ids, 0)
    for u, v, _ in clean:
        out[u].append(v)
        indeg[v] += 1
    back: set[tuple[str, str]] = set()
    state = dict.fromkeys(ids, 0)  # 0 new, 1 on the stack, 2 done
    for root in [i for i in ids if indeg[i] == 0] + ids:
        if state[root]:
            continue
        stack = [(root, iter(out[root]))]
        state[root] = 1
        while stack:
            u, it = stack[-1]
            for v in it:
                if state[v] == 1:
                    back.add((u, v))
                elif state[v] == 0:
                    state[v] = 1
                    stack.append((v, iter(out[v])))
                    break
            else:
                state[u] = 2
                stack.pop()
    return ids, clean, back


def _layers(ids: list[str], dag: list[tuple[str, str]]) -> list[list[str]]:
    """Longest-path layering, then barycenter ordering inside the layers."""
    preds: dict[str, list[str]] = {i: [] for i in ids}
    succs: dict[str, list[str]] = {i: [] for i in ids}
    for u, v in dag:
        succs[u].append(v)
        preds[v].append(u)
    indeg = {i: len(preds[i]) for i in ids}
    order = [i for i in ids if indeg[i] == 0]
    rank = dict.fromkeys(ids, 0)
    k = 0
    while k < len(order):
        u = order[k]
        k += 1
        for v in succs[u]:
            rank[v] = max(rank[v], rank[u] + 1)
            indeg[v] -= 1
            if indeg[v] == 0:
                order.append(v)
    layers: list[list[str]] = [[] for _ in range(max(rank.values()) + 1)]
    for i in ids:
        layers[rank[i]].append(i)

    def coords() -> dict[str, float]:
        return {v: p - (len(layer) - 1) / 2 for layer in layers for p, v in enumerate(layer)}

    for _ in range(4):
        for sweep, nbrs in ((range(1, len(layers)), preds), (range(len(layers) - 2, -1, -1), succs)):
            for li in sweep:
                pos = coords()
                keyed = []
                for p, v in enumerate(layers[li]):
                    near = [pos[u] for u in nbrs[v]]
                    keyed.append((sum(near) / len(near) if near else pos[v], p, v))
                layers[li] = [v for _, _, v in sorted(keyed)]
    return layers


class _Flow:
    """A flowchart layout in (main, cross) coordinates: main runs along the layers."""

    def __init__(self, c: Canvas, nodes: list[dict[str, Any]], layers: list[list[str]], n_channel: int,
                 horizontal: bool) -> None:
        self.c, self.horizontal, self.layers = c, horizontal, layers
        main_len, cross_len = (c.cx, c.cy) if horizontal else (c.cy, c.cx)
        self.main0, self.cross0 = (c.x, c.y) if horizontal else (c.y, c.x)
        self.n_lanes = min(n_channel, 3)
        self.pitch = int(max(cross_len * 0.035, 100000))
        margin = int(self.pitch * (self.n_lanes + 0.25)) if n_channel else 0
        width = max(len(layer) for layer in layers)
        cell_m, cell_c = main_len / len(layers), (cross_len - 2 * margin) / width
        self.cell_m, self.cell_c = cell_m, cell_c
        if horizontal:
            w = min(cell_m * 0.78, 2300000)
            h = min(cell_c * 0.7, w * 0.7, 1100000)
            w = min(w, h * 3.2)
        else:
            h = min(cell_m * 0.7, 1000000)
            w = min(cell_c * 0.8, h * 5.0, 2500000)
        self.lo, self.hi = self.cross0 + margin, self.cross0 + cross_len - margin
        self.rect: dict[str, tuple[float, float, float, float]] = {}  # id -> (main, cross, main len, cross len)
        self.layer_of: dict[str, int] = {}
        kinds = {n["id"]: n.get("kind") or "step" for n in nodes}
        self.kinds = kinds
        for li, layer in enumerate(layers):
            for p, v in enumerate(layer):
                grow = 1.3 if kinds[v] == "decision" else 1.0
                nw, nh = (min(w * grow, cell_m * 0.9), min(h * grow, cell_c * 0.9)) if horizontal else (
                    min(w * grow, cell_c * 0.92), min(h * grow, cell_m * 0.94))
                ml, cl = (nw, nh) if horizontal else (nh, nw)
                mc = self.main0 + (li + 0.5) * cell_m
                cc = (self.lo + self.hi) / 2 + (p - (len(layer) - 1) / 2) * cell_c
                self.rect[v] = (mc - ml / 2, cc - cl / 2, ml, cl)
                self.layer_of[v] = li
        self.gap_mid = []
        for li in range(len(layers) - 1):
            end = max(self.rect[v][0] + self.rect[v][2] for v in layers[li])
            start = min(self.rect[v][0] for v in layers[li + 1])
            self.gap_mid.append(((end + start) / 2, start - end))
        self.used = {"lo": 0, "hi": 0}

    def xywh(self, v: str) -> tuple[float, float, float, float]:
        m, cr, ml, cl = self.rect[v]
        return (m, cr, ml, cl) if self.horizontal else (cr, m, cl, ml)

    def point(self, m: float, cr: float) -> tuple[float, float]:
        return (m, cr) if self.horizontal else (cr, m)

    def hits(self, path: list[tuple[float, float]], skip: tuple[str, str]) -> int:
        count = 0
        for (m1, c1), (m2, c2) in itertools.pairwise(path):
            for v, (m, cr, ml, cl) in self.rect.items():
                if v in skip:
                    continue
                if min(m1, m2) < m + ml and max(m1, m2) > m and min(c1, c2) < cr + cl and max(c1, c2) > cr:
                    count += 1
        return count

    def route(self, u: str, v: str, back: bool):
        """Sites, the middle-segment coordinate, and the label anchor for the edge ``u -> v``."""
        um, uc, uml, ucl = self.rect[u]
        vm, vc, vml, vcl = self.rect[v]
        lu, lv = self.layer_of[u], self.layer_of[v]
        out_site, in_site = (RIGHT, LEFT) if self.horizontal else (BOTTOM, TOP)
        if not back and lv > lu:
            b, e = (um + uml, uc + ucl / 2), (vm, vc + vcl / 2)
            options = [self.gap_mid[lu][0]] if lv == lu + 1 else [self.gap_mid[lv - 1][0], self.gap_mid[lu][0]]
            for mid in options:
                path = [b, (mid, b[1]), (mid, e[1]), e]
                if self.hits(path, (u, v)) == 0:
                    if abs(b[1] - e[1]) < 1:
                        anchor = ((b[0] + e[0]) / 2, b[1])
                    else:
                        step = min(abs(e[1] - b[1]), self.cell_c) / 2
                        anchor = (mid, e[1] - math.copysign(step, e[1] - b[1]))
                    gap = self.gap_mid[lv - 1 if mid == self.gap_mid[lv - 1][0] else lu][1]
                    return out_site, in_site, (None if abs(b[1] - e[1]) < 1 else mid), anchor, gap
        paths = {}
        for side in ("lo", "hi"):
            lane = min(self.used[side], max(self.n_lanes - 1, 0))
            ch = self.lo - self.pitch * (lane + 0.75) if side == "lo" else self.hi + self.pitch * (lane + 0.75)
            bc, ec = (uc, vc) if side == "lo" else (uc + ucl, vc + vcl)
            path = [(um + uml / 2, bc), (um + uml / 2, ch), (vm + vml / 2, ch), (vm + vml / 2, ec)]
            paths[side] = (self.hits(path, (u, v)), ch, path)
        prefer = ("lo", "hi") if back else ("hi", "lo")
        side = prefer[0] if paths[prefer[0]][0] <= paths[prefer[1]][0] else prefer[1]
        self.used[side] += 1
        _, ch, path = paths[side]
        if self.horizontal:
            site = TOP if side == "lo" else BOTTOM
        else:
            site = LEFT if side == "lo" else RIGHT
        return site, site, ch, (path[0][0], (path[0][1] + ch) / 2), None


def _flow_plan(c: Canvas, nodes, layers, n_channel: int, horizontal: bool):
    flow = _Flow(c, nodes, layers, n_channel, horizontal)
    items = []
    for nd in nodes:
        _, _, w, h = flow.xywh(nd["id"])
        _, _, tw, th = text_area(_KIND_PRST.get(flow.kinds[nd["id"]], "rect"), int(w), int(h))
        items.append((nd["id"], node_label(nd), tw + 2 * (PAD - _flow_pad(flow.kinds[nd["id"]])),
                      th + 2 * (PAD - _flow_pad(flow.kinds[nd["id"]]))))
    size, failed = c.best_size(items)
    return size, failed, flow


def _flow_pad(kind: str) -> int:
    """Text inset of a flowchart node: diamonds and terminators already have an inset text rectangle."""
    return 0 if kind in ("decision", "start", "end") else PAD // 2


def _flowchart(c: Canvas, nodes: list[dict[str, Any]], edges: list[dict[str, Any]]) -> None:
    ids, clean, back = _graph(nodes, edges)
    dag = [((v, u) if (u, v) in back else (u, v)) for u, v, _ in clean]
    layers = _layers(ids, dag)
    rank = {v: li for li, layer in enumerate(layers) for v in layer}
    n_channel = sum(1 for u, v, _ in clean if (u, v) in back or rank[v] - rank[u] > 1)
    plans = [_flow_plan(c, nodes, layers, n_channel, horizontal) for horizontal in (True, False)]
    if len(layers) == 1:
        plans = plans[:1]
    size, failed, flow = pick(plans, gain=1.0)
    c.overflow.extend(failed)
    shapes = {}
    for nd in nodes:
        kind = flow.kinds[nd["id"]]
        x, y, w, h = flow.xywh(nd["id"])
        emphasis = kind in ("start", "end")
        shp = c.shape(_KIND_PRST.get(kind, "rect"), x, y, w, h, nd["id"], fill="accent1" if emphasis else "lt2",
                      line=None if emphasis else "accent1")  # fmt: skip
        c.text(shp, node_label(nd), size, color="lt1" if emphasis else "dk1", pad=_flow_pad(kind))
        shapes[nd["id"]] = shp
    label_size = max(c.min_pt, round(size * 0.85 * 2) / 2)
    notes = []
    for u, v, text in clean:
        site_a, site_b, mid, anchor, room = flow.route(u, v, (u, v) in back)
        c.connect(shapes[u], site_a, shapes[v], site_b, f"{u}-{v}", mid=mid, color="dk2", width_pt=1.25)
        if text:
            notes.append((u, v, text, anchor, room))
    pad = PAD // 2
    for u, v, text, anchor, room in notes:
        lab = label(text)
        ax, ay = flow.point(*anchor)
        if flow.horizontal:
            limit = max(room * 0.92, 300000) if room else flow.cell_m * 0.6
        else:
            limit = flow.cell_c * 0.6
        w = int(min(c.width_of(lab, label_size, pad), limit))
        h = c.height_of(lab, w, label_size, pad)
        if not c.fits(lab, w, h, label_size, pad) and f"{u}->{v}" not in c.overflow:
            c.overflow.append(f"{u}->{v}")
        x = min(max(ax - w / 2, c.x), c.x + c.cx - w)
        y = min(max(ay - h / 2, c.y), c.y + c.cy - h)
        c.textbox(x, y, w, h, f"{u}-{v}-label", lab, label_size, pad=pad, fill="lt1")
