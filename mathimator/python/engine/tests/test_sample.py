"""The certified sampler (WP-0.5): golden cases, an independent check of the certified bound,
the other object kinds, determinism, parameter overrides, and the RPC method."""

from __future__ import annotations

import itertools
import json
import math

import numpy as np
import pytest
import sympy as sp
from golden_sample import CASES, Case

from mathimator_engine.engine import Engine
from mathimator_engine.sample import TOL_PX, sample
from mathimator_engine.server import Methods, handle_line

PIXELS = (800, 600)


@pytest.fixture
def engine():
    e = Engine()
    e.x = e.call("sym.symbols", {"names": "x"})["value"][0]
    return e


def plot(engine, expr, obj_id="c"):
    r = engine.call("plot.explicit", {"expr": expr, "var": engine.x, "id": obj_id})
    assert r["status"] == "ok", r
    return obj_id


def strip(result):
    return {k: v for k, v in result.items() if k != "stats"}


def points_of(result):
    return [p for seg in result["segments"] for p in seg]


def rounded(pairs):
    return [tuple(round(v, 9) for v in p) for p in pairs]


def crosses(result, x0, excl):
    """True when some drawn segment spans x0 (from left of it to right of it)."""
    for seg in result["segments"]:
        for (xa, _), (xb, _) in itertools.pairwise(seg):
            if xa < x0 - excl / 2 and xb > x0 + excl / 2:
                return True
    return False


def graph_within_tolerance(result, f_text, viewport, exclusions, pixels=PIXELS, n=4000):
    """Independent check of the certified claim: densely sampled points of the true graph lie
    within TOL_PX of the polyline (on screen, away from marked breakpoints)."""
    x = sp.Symbol("x", real=True)
    from mathimator_engine.mathparse import read_math

    fn = sp.lambdify(x, read_math(f_text), "numpy")
    (xlo, xhi), (ylo, yhi) = viewport["x"], viewport["y"]
    sx, sy = pixels[0] / (xhi - xlo), pixels[1] / (yhi - ylo)
    xs = np.linspace(xlo, xhi, n)
    with np.errstate(all="ignore"):
        ys = np.asarray(fn(xs), dtype=float) * np.ones_like(xs)
    segs = []
    for seg in result["segments"]:
        arr = np.array(seg)
        segs.append(
            np.stack(
                [
                    (arr[:-1, 0] - xlo) * sx,
                    (yhi - arr[:-1, 1]) * sy,
                    (arr[1:, 0] - xlo) * sx,
                    (yhi - arr[1:, 1]) * sy,
                ],
                axis=1,
            )
        )
    allsegs = np.concatenate(segs) if segs else np.zeros((0, 4))
    worst = 0.0
    for xv, yv in zip(xs, ys):
        if not math.isfinite(yv) or not (ylo <= yv <= yhi):
            continue
        if any(abs(xv - e) * sx <= TOL_PX + 1e-9 for e in exclusions):
            continue
        px, py = (xv - xlo) * sx, (yhi - yv) * sy
        ax, ay, bx, by = allsegs.T
        dx, dy = bx - ax, by - ay
        length = dx * dx + dy * dy
        t = np.where(length > 0, ((px - ax) * dx + (py - ay) * dy) / np.where(length > 0, length, 1), 0)
        t = np.clip(t, 0, 1)
        d = np.hypot(px - (ax + t * dx), py - (ay + t * dy)).min()
        worst = max(worst, d)
    return worst


@pytest.mark.parametrize("name", sorted(CASES))
def test_golden(engine, name):
    case: Case = CASES[name]
    plot(engine, case.expr)
    result = sample(engine, "c", case.viewport, PIXELS)
    assert result["status"] == case.status, (result["status"], result.get("reason"))
    if case.status == "sampled":
        assert result.get("reason"), "a sampled result must say why"
        if case.reason:
            assert case.reason in result["reason"], result["reason"]
    else:
        assert "reason" not in result
    excl = TOL_PX / (PIXELS[0] / (case.viewport["x"][1] - case.viewport["x"][0]))
    markers = result["markers"]

    asym = sorted(m["x"] for m in markers if m["kind"] == "asymptote")
    assert asym == pytest.approx(sorted(case.asymptotes), abs=1e-9)
    for pole in case.asymptotes:
        assert not crosses(result, pole, excl), f"a segment crosses the pole at {pole}"

    holes = sorted((m["x"], m["y"]) for m in markers if m["kind"] == "hole")
    assert rounded(holes) == rounded(sorted(case.holes))
    for hx, _ in case.holes:
        assert all(m["closed"] is False for m in markers if m["kind"] == "hole")
        assert not crosses(result, hx, excl)

    jumps = sorted((m["x"], m["y"], m["closed"]) for m in markers if m["kind"] == "jump_end")
    assert [(a, b, c) for a, b, c in jumps] == sorted((float(a), float(b), c) for a, b, c in case.jump_ends)
    for jx, _, _ in case.jump_ends:
        assert not crosses(result, jx, excl), f"a segment crosses the jump at {jx}"
    pts = sorted((m["x"], m["y"]) for m in markers if m["kind"] == "point")
    assert rounded(pts) == rounded(sorted(case.points))

    xs = [p[0] for p in points_of(result)]
    ys = [p[1] for p in points_of(result)]
    assert xs, "nothing was drawn"
    assert all(math.isfinite(v) for v in xs + ys)
    if case.peak is not None:
        assert max(ys) >= case.peak - 1e-3, max(ys)
    if case.x_min is not None:
        assert min(xs) > case.x_min
    if case.ends is not None:
        assert (min(xs), max(xs)) == case.ends

    if case.status == "certified":
        exclusions = [*case.asymptotes, *[h[0] for h in case.holes], *[j[0] for j in case.jump_ends]]
        worst = graph_within_tolerance(result, case.expr, case.viewport, exclusions)
        assert worst <= TOL_PX + 1e-6, f"the true graph is {worst:.3f} px from the polyline"
    json.dumps(result, allow_nan=False)  # the result is valid JSON


def test_deterministic(engine):
    plot(engine, "tan(x) + sin(5*x)/x")
    a = sample(engine, "c", {"x": [-10, 10], "y": [-5, 5]})
    b = sample(engine, "c", {"x": [-10, 10], "y": [-5, 5]})
    assert strip(a) == strip(b)


def test_piecewise_jump(engine):
    pw = engine.call(
        "sym.piecewise",
        {"pieces": [{"expr": "x**2", "when": "x < 1"}, {"expr": "x + 1", "when": "otherwise"}]},
    )["value"]
    assert engine.call("plot.explicit", {"expr": pw, "var": engine.x, "id": "c"})["status"] == "ok"
    result = sample(engine, "c", {"x": [-3, 3], "y": [-1, 5]})
    assert result["status"] == "certified", result.get("reason")
    jumps = sorted((m["x"], m["y"], m["closed"]) for m in result["markers"] if m["kind"] == "jump_end")
    assert jumps == [(1.0, 1.0, False), (1.0, 2.0, True)]


def test_parameters_and_overrides(engine):
    engine.call("param.declare", {"name": "a", "value": 1, "min": -3, "max": 3})
    plot(engine, "a*sin(x)")
    view = {"x": [-5, 5], "y": [-4, 4]}
    revision = engine.scene.revision
    base = sample(engine, "c", view)
    dragged = sample(engine, "c", view, params={"a": 2.5})
    assert max(p[1] for p in points_of(dragged)) == pytest.approx(2.5, abs=1e-3)
    assert max(p[1] for p in points_of(base)) == pytest.approx(1.0, abs=1e-3)
    assert engine.scene.revision == revision, "sampling must not change scene state"
    with pytest.raises(Exception, match="no parameter"):
        sample(engine, "c", view, params={"zz": 1})


def test_parametric_circle(engine):
    t = engine.call("sym.symbols", {"names": "t"})["value"][0]
    engine.call("plot.parametric", {"x": "cos(t)", "y": "sin(t)", "var": t, "range": [0, "2*pi"], "id": "c"})
    result = sample(engine, "c", {"x": [-2, 2], "y": [-1.5, 1.5]})
    assert result["status"] == "certified", result.get("reason")
    pts = np.array(points_of(result))
    assert np.allclose(np.hypot(pts[:, 0], pts[:, 1]), 1, atol=4 / 800)
    assert np.allclose(pts[0], pts[-1])


def test_polar_rose(engine):
    t = engine.call("sym.symbols", {"names": "t"})["value"][0]
    engine.call("plot.polar", {"r": "cos(3*t)", "var": t, "id": "c"})
    result = sample(engine, "c", {"x": [-1.5, 1.5], "y": [-1.2, 1.2]})
    assert result["status"] == "certified", result.get("reason")
    pts = np.array(points_of(result))
    assert np.hypot(pts[:, 0], pts[:, 1]).max() == pytest.approx(1, abs=1e-3)


def test_implicit_circle(engine):
    y = engine.call("sym.symbols", {"names": "y"})["value"][0]
    engine.call("plot.implicit", {"equation": "x**2 + y**2 = 1", "x": engine.x, "y": y, "id": "c"})
    view = {"x": [-2, 2], "y": [-1.5, 1.5]}
    result = sample(engine, "c", view)
    assert result["status"] == "sampled" and "marching squares" in result["reason"]
    pts = np.array(points_of(result))
    px = 4 / 800
    assert np.all(np.abs(np.hypot(pts[:, 0], pts[:, 1]) - 1) < px)
    angles = np.degrees(np.arctan2(pts[:, 1], pts[:, 0]))
    assert len(np.unique(np.floor(angles) % 360)) == 360  # the whole circle is traced


def test_region_disk(engine):
    y = engine.call("sym.symbols", {"names": "y"})["value"][0]
    engine.call("plot.region", {"condition": "x**2 + y**2 < 1", "x": engine.x, "y": y, "id": "c"})
    result = sample(engine, "c", {"x": [-2, 2], "y": [-1.5, 1.5]})
    assert result["status"] == "certified", result.get("reason")
    rects = np.array(result["fill"]["rects"])
    area = ((rects[:, 2] - rects[:, 0]) * (rects[:, 3] - rects[:, 1])).sum()
    assert area == pytest.approx(math.pi, rel=0.01)
    centres = np.stack([(rects[:, 0] + rects[:, 2]) / 2, (rects[:, 1] + rects[:, 3]) / 2], axis=1)
    assert np.all(np.hypot(centres[:, 0], centres[:, 1]) < 1 + 1e-9)


def test_simple_kinds(engine):
    plot(engine, "x**2", "q")
    engine.call("plot.area", {"of": "q", "over": [0, 1], "id": "area"})
    engine.call("plot.riemann", {"of": "q", "over": [0, 1], "n": 4, "rule": "left", "id": "sum"})
    engine.call("plot.point", {"at": [1, "pi/4"], "id": "pt", "label": "P"})
    engine.call("plot.vector", {"origin": [0, 0], "vector": [1, 2], "id": "v"})
    engine.call("plot.data", {"x": [0, 1, 2], "y": [1, 2, 3], "id": "d"})
    engine.call("scene.highlight", {"target": "q", "over": [0, 1], "id": "hl"})
    engine.call("note.label", {"text": "here", "at": [1, 1], "id": "lab"})
    view = {"x": [-2, 2], "y": [-1, 3]}

    area = sample(engine, "area", view)
    assert area["status"] == "certified" and len(area["fill"]["polygons"]) == 1
    riemann = sample(engine, "sum", view)
    assert riemann["total"] == pytest.approx((0 + 1 / 16 + 4 / 16 + 9 / 16) / 4)
    assert len(riemann["fill"]["polygons"]) == 4
    point = sample(engine, "pt", view)
    assert point["markers"] == [{"kind": "point", "x": 1.0, "y": math.pi / 4, "closed": True, "label": "P"}]
    assert sample(engine, "v", view)["arrow"] == {"from": [0.0, 0.0], "to": [1.0, 2.0]}
    assert sample(engine, "d", view)["points"] == [[0.0, 1.0], [1.0, 2.0], [2.0, 3.0]]
    hl = points_of(sample(engine, "hl", view))
    assert min(p[0] for p in hl) == 0 and max(p[0] for p in hl) == 1
    assert sample(engine, "lab", view)["markers"] == [{"kind": "point", "x": 1.0, "y": 1.0}]


def test_bad_requests(engine):
    plot(engine, "x")
    with pytest.raises(Exception, match="no object"):
        sample(engine, "nope", {"x": [0, 1], "y": [0, 1]})
    with pytest.raises(Exception, match="viewport"):
        sample(engine, "c", {"x": [1, 0], "y": [0, 1]})


def test_rpc(engine):
    methods = Methods(engine)
    from mathimator_engine.sample import register

    register(methods)
    hello, _ = handle_line(
        methods, b'{"jsonrpc":"2.0","id":1,"method":"engine.hello","params":{"protocol_version":1}}'
    )
    assert "result" in hello
    plot(engine, "1/x")
    req = {
        "jsonrpc": "2.0",
        "id": 2,
        "method": "sample",
        "params": {"object": "c", "viewport": {"x": [-1, 1], "y": [-1, 1]}, "pixels": {"w": 400, "h": 300}},
    }
    resp, _ = handle_line(methods, json.dumps(req).encode())
    assert resp["result"]["status"] == "certified"
    assert [m["kind"] for m in resp["result"]["markers"]] == ["asymptote"]
    req["params"]["object"] = "missing"
    resp, _ = handle_line(methods, json.dumps(req).encode())
    assert resp["result"]["status"] == "error" and resp["result"]["error"]["kind"] == "bad_args"


def test_never_certified_without_breakpoint_analysis(engine, monkeypatch):
    """If the symbolic analysis cannot run, a jump must not come back certified, even though
    a near-vertical chord across it passes the box test."""
    from mathimator_engine import sample as sampler
    from mathimator_engine.errors import CallError

    def timed_out(*args, **kwargs):
        raise CallError("timeout", "simulated")

    monkeypatch.setattr(sampler, "_analyze", timed_out)
    plot(engine, "heaviside(x)")
    result = sample(engine, "c", {"x": [-1, 1], "y": [-1, 2]})
    assert result["status"] == "sampled"
    assert "ran out of time" in result["reason"]


def test_sympy_piecewise_limit_is_not_trusted():
    """SymPy's limit(f, x, 1, '-') returns 2 for this Piecewise; the sampler's limit must not."""
    from mathimator_engine.sample import one_sided_limit

    x = sp.Symbol("x", real=True)
    f = sp.Piecewise((x**2, x < 1), (x + 1, True))
    assert one_sided_limit(f, x, 1, "left") == 1
    assert one_sided_limit(f, x, 1, "right") == 2
