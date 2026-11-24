"""The Matplotlib exporter: drawn from checked samples, deterministic, and honest about them."""

from __future__ import annotations

import json
import re
import xml.etree.ElementTree as ET
from pathlib import Path

import pytest

from fibration_math_engine import export
from fibration_math_engine.engine import Engine
from fibration_math_engine.errors import CallError

SMALL = {"w_in": 3.0, "h_in": 2.0}
SVG = "{http://www.w3.org/2000/svg}"


def scene(*calls) -> Engine:
    engine = Engine()
    for fn, args in calls:
        out = engine.call(fn, args)
        assert out["status"] == "ok", (fn, out)
    return engine


def curve(text: str, **extra) -> Engine:
    return scene(("plot.explicit", {"expr": text, "var": "x", "id": "c", **extra}))


def groups(svg_path: str, prefix: str) -> dict[str, ET.Element]:
    """Elements of an exported svg whose id starts with ``prefix``."""
    root = ET.parse(svg_path).getroot()
    return {el.get("id"): el for el in root.iter() if (el.get("id") or "").startswith(prefix)}


def path_xs(group: ET.Element) -> list[float]:
    """The x coordinates (in svg units) of the path drawn in a group."""
    d = group.find(f"{SVG}path").get("d")
    return [float(x) for x, _ in re.findall(r"(-?\d+(?:\.\d+)?) (-?\d+(?:\.\d+)?)", d)]


def render(engine, tmp_path, fmt="svg", name="fig", **kw):
    kw.setdefault("size", SMALL)
    return export.render(engine, fmt, str(tmp_path / f"{name}.{fmt}"), **kw)


# ---- what is drawn ----------------------------------------------------------------------


def test_a_pole_is_two_lines_and_an_asymptote_never_a_connector(tmp_path):
    out = render(curve("1/x"), tmp_path, viewport={"x": [-3, 3], "y": [-3, 3]})
    assert out["certified"] is True
    segs = groups(out["path"], "obj-c-seg-")
    assert len(segs) == 2
    left, right = (path_xs(segs[f"obj-c-seg-{i}"]) for i in (0, 1))
    assert max(left) < min(right)  # neither line reaches across the pole
    assert list(groups(out["path"], "obj-c-marker-asymptote-")) == ["obj-c-marker-asymptote-0"]


def test_a_removable_hole_is_marked(tmp_path):
    out = render(curve("sin(x)/x"), tmp_path, viewport={"x": [-6, 6], "y": [-1, 2]})
    assert "obj-c-marker-hole-0" in groups(out["path"], "obj-c-marker-")
    # Whether the sampler can certify right up to the hole depends on the viewport; what the
    # exporter owes is the marker, and a footnote exactly when the result is not certified.
    assert out["certified"] is ("not certified" not in Path(out["path"]).read_text())


def test_a_sampled_plot_says_so_on_the_figure(tmp_path):
    out = render(
        curve("sin(1/x)", domain=[-1, 1]), tmp_path, name="osc", viewport={"x": [-1, 1], "y": [-2, 2]}
    )
    assert out["certified"] is False
    assert out["objects"][0]["status"] == "sampled" and out["objects"][0]["reason"]
    assert "sampled, not certified: c" in Path(out["path"]).read_text()
    assert "footnote-sampled" in groups(out["path"], "footnote")

    clean = render(curve("x**2"), tmp_path, name="clean")
    assert clean["certified"] is True
    assert "not certified" not in Path(clean["path"]).read_text()
    assert not groups(clean["path"], "footnote")


MIXED = [
    ("param.declare", {"name": "a", "value": 1, "min": 0, "max": 3}),
    (
        "plot.explicit",
        {"expr": "a*sin(x)", "var": "x", "id": "c", "domain": [-3, 3], "style": {"label": "a sin x"}},
    ),
    ("plot.area", {"of": "c", "over": [0, "pi/2"], "id": "area"}),
    ("plot.riemann", {"of": "c", "over": [-2, -1], "n": 4, "id": "bars"}),
    ("plot.tangent", {"of": "c", "at": 1, "id": "tan", "style": {"dash": "dashed", "color": "#333333"}}),
    ("plot.parametric", {"x": "cos(t)", "y": "sin(t)", "var": "t", "range": [0, "2*pi"], "id": "circle"}),
    ("plot.polar", {"r": "1 + cos(t)/2", "var": "t", "id": "polar"}),
    ("plot.region", {"condition": "x**2 + y**2 < 1/4", "x": "x", "y": "y", "id": "disk"}),
    ("plot.implicit", {"equation": "x**2/4 + y**2 = 1", "x": "x", "y": "y", "id": "ellipse"}),
    ("plot.data", {"x": [-2, -1, 0], "y": [1, 2, 1.5], "id": "pts", "style": {"label": "data"}}),
    ("plot.data", {"x": [-2, -1, 0], "y": [2, 2.5, 2], "mode": "line", "id": "series"}),
    ("plot.point", {"at": [2, "a"], "label": "P", "id": "p"}),
    ("plot.vector", {"origin": [0, 0], "vector": [1, 2], "id": "v"}),
    ("note.label", {"text": "costs $5 & <more>", "at": "c", "id": "n1"}),
    ("note.label", {"text": "here", "at": [-2, -2], "id": "n2"}),
    ("note.arrow", {"start": [-2, -2], "end": [-1, -1], "id": "n4"}),
    ("scene.highlight", {"target": "c", "over": [1, 2], "id": "hl"}),
]


@pytest.fixture(scope="module")
def mixed() -> Engine:
    engine = scene(*MIXED)
    f = engine.call("sym.define", {"text": "a*sin(x)/x"})["value"]
    assert engine.call("note.latex", {"expr": f, "at": [1, -2], "id": "n3"})["status"] == "ok"
    return engine


@pytest.mark.parametrize("fmt", ["svg", "pdf", "png"])
def test_every_kind_renders_in_every_format(mixed, tmp_path, fmt):
    out = render(mixed, tmp_path, fmt, viewport={"x": [-3, 3], "y": [-3, 3]}, title="Everything at $0")
    data = Path(out["path"]).read_bytes()
    assert data.startswith({"svg": b"<?xml", "pdf": b"%PDF", "png": b"\x89PNG"}[fmt])
    assert {o["id"] for o in out["objects"]} == {m[1]["id"] for m in MIXED if "id" in m[1]} | {"n3"}
    # Implicit curves are never certified, so the export as a whole is not.
    assert [o["id"] for o in out["objects"] if o["status"] != "certified"] == ["ellipse"]
    assert not list(tmp_path.glob(".*.tmp"))


def test_drawn_elements_of_the_mixed_scene(mixed, tmp_path):
    out = render(mixed, tmp_path, viewport={"x": [-3, 3], "y": [-3, 3]})
    ids = set(groups(out["path"], "obj-")) | set(groups(out["path"], "legend"))
    for expected in (
        "obj-c-seg-0",
        "obj-area-fill",
        "obj-bars-fill",
        "obj-tan-seg-0",
        "obj-circle-seg-0",
        "obj-polar-seg-0",
        "obj-disk-fill",
        "obj-ellipse-seg-0",
        "obj-pts-points",
        "obj-series-seg-0",
        "obj-p-marker-point-0",
        "obj-p-label",
        "obj-v-arrow",
        "obj-n1-text",
        "obj-n2-text",
        "obj-n3-text",
        "obj-n4-arrow",
        "obj-hl-highlight-0",
        "legend",
    ):
        assert expected in ids, expected
    text = Path(out["path"]).read_text()
    assert "costs $5 &amp; &lt;more&gt;" in text  # a label is plain text, escaped by the writer
    assert "sampled, not certified: ellipse" in text


def test_latex_falls_back_to_plain_text(mixed, tmp_path, monkeypatch):
    monkeypatch.setattr(export, "_mathtext_ok", lambda mpl, text: False)
    out = render(mixed, tmp_path, viewport={"x": [-3, 3], "y": [-3, 3]})
    assert "a*sin(x)/x" in Path(out["path"]).read_text()


def test_region_cells_are_merged():
    rects = [[0, 0, 1, 1], [1, 0, 2, 1], [2, 0, 2.5, 1], [4, 0, 5, 1], [0, 1, 1, 2]]
    assert export._merge_rects(rects) == [[0, 0, 2.5, 1], [4, 0, 5, 1], [0, 1, 1, 2]]


# ---- determinism ------------------------------------------------------------------------


@pytest.mark.parametrize("fmt", ["svg", "png", "pdf"])
def test_the_same_scene_gives_the_same_bytes(tmp_path, fmt):
    engine = scene(
        ("plot.explicit", {"expr": "tan(x)", "var": "x", "id": "c", "style": {"label": "tan"}}),
        ("plot.region", {"condition": "y < x and y > 0", "x": "x", "y": "y", "id": "r"}),
        ("note.label", {"text": "t", "at": "c"}),
    )
    view = {"x": [-3, 3], "y": [-3, 3]}
    first = Path(render(engine, tmp_path, fmt, name="a", viewport=view)["path"]).read_bytes()
    second = Path(render(engine, tmp_path, fmt, name="b", viewport=view)["path"]).read_bytes()
    assert first == second
    # A fresh engine (new handle ids, cold caches) draws the same file too.
    other = scene(
        ("plot.explicit", {"expr": "tan(x)", "var": "x", "id": "c", "style": {"label": "tan"}}),
        ("plot.region", {"condition": "y < x and y > 0", "x": "x", "y": "y", "id": "r"}),
        ("note.label", {"text": "t", "at": "c"}),
    )
    assert Path(render(other, tmp_path, fmt, name="c", viewport=view)["path"]).read_bytes() == first


def test_png_pixels_and_size(tmp_path):
    from PIL import Image

    engine = curve("x**2")
    a = Image.open(render(engine, tmp_path, "png", name="a", dpi=100)["path"])
    b = Image.open(render(engine, tmp_path, "png", name="b", dpi=100)["path"])
    assert a.size == (300, 200)
    assert a.tobytes() == b.tobytes()
    assert "Software" not in a.info


# ---- sidecar ----------------------------------------------------------------------------


def test_sidecar_describes_the_export(tmp_path):
    engine = scene(
        ("param.declare", {"name": "a", "value": 2, "min": 0, "max": 3}),
        ("plot.explicit", {"expr": "a/x", "var": "x", "id": "c"}),
        ("note.label", {"text": "n", "at": [1, 1], "id": "n"}),
    )
    out = render(engine, tmp_path, viewport={"x": [-2, 2], "y": [-4, 4]}, title="T")
    assert out["sidecar"] == str(tmp_path / "fig.json")
    side = json.loads(Path(out["sidecar"]).read_text())
    assert side["schema"] == 1 and side["kind"] == "scene-export" and side["format"] == "svg"
    assert (
        side["scene"]
        == {"revision": engine.scene.revision, "hash": engine.scene.hash()}
        == {
            "revision": out["revision"],
            "hash": out["hash"],
        }
    )
    assert side["api_version"] == 1 and side["engine_version"]
    assert side["viewport"] == {"x": [-2.0, 2.0], "y": [-4.0, 4.0]} == out["viewport"]
    assert side["size"] == {"w_in": 3.0, "h_in": 2.0}
    assert side["params"] == {"a": 2.0}
    assert side["certified"] is True and side["title"] == "T"
    assert re.fullmatch(r"\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ", side["created_at"])
    c, n = side["objects"]
    assert (c["id"], c["kind"], c["status"]) == ("c", "explicit", "certified")
    assert c["exprs"]["f"]["text"] == "a/x" and c["exprs"]["f"]["latex"] == r"\frac{a}{x}"
    assert re.fullmatch(r"[0-9a-f]{64}", c["exprs"]["f"]["hash"])
    assert (n["id"], n["kind"], n["exprs"]) == ("n", "label", {})


def test_parameter_overrides_change_the_figure_and_are_recorded(tmp_path):
    engine = scene(
        ("param.declare", {"name": "a", "value": 1, "min": 0, "max": 3}),
        ("plot.explicit", {"expr": "a*x", "var": "x", "id": "c"}),
    )
    view = {"x": [-2, 2], "y": [-4, 4]}
    base = render(engine, tmp_path, name="base", viewport=view)
    steep = render(engine, tmp_path, name="steep", viewport=view, params={"a": 2.5})
    assert Path(base["path"]).read_bytes() != Path(steep["path"]).read_bytes()
    assert json.loads(Path(steep["sidecar"]).read_text())["params"] == {"a": 2.5}
    assert engine.scene.params["a"].value == 1.0  # an override never changes the scene
    assert base["hash"] == steep["hash"]
    with pytest.raises(CallError):
        render(engine, tmp_path, params={"b": 1})


# ---- viewport ---------------------------------------------------------------------------


def test_default_viewport_rules():
    assert export.default_viewport(Engine()) == {"x": [-10.0, 10.0], "y": [-10.0, 10.0]}

    fitted = export.default_viewport(curve("x**2", domain=[-2, 2]))
    assert fitted["x"] == [-2.0, 2.0]
    assert -0.5 < fitted["y"][0] <= 0 and 4 <= fitted["y"][1] < 4.5  # the curve's range plus a margin

    unbounded = export.default_viewport(curve("sin(x)"))
    assert unbounded["x"] == [-10.0, 10.0] and -1.3 < unbounded["y"][0] < -1 < 1 < unbounded["y"][1] < 1.3

    pole = export.default_viewport(curve("1/x"))
    assert pole["y"][0] < 0 < pole["y"][1] and pole["y"][1] < 50  # trimmed, not run off to the pole

    flat = export.default_viewport(curve("3"))
    assert flat["y"][0] < 3 < flat["y"][1]

    circle = scene(("plot.parametric", {"x": "cos(t)", "y": "sin(t)", "var": "t", "range": [0, "2*pi"]}))
    box = export.default_viewport(circle)
    assert -1.3 < box["x"][0] < -1 and 1 < box["x"][1] < 1.3 and -1.3 < box["y"][0] < -1


def test_view_hint_is_used(tmp_path):
    engine = curve("x**2")
    engine.call("cam.view", {"x": [-1, 3], "y": [0, 5]})
    assert export.default_viewport(engine) == {"x": [-1.0, 3.0], "y": [0.0, 5.0]}
    engine.call("cam.view", {"x": [-1, 2]})
    hinted = export.default_viewport(engine)
    assert hinted["x"] == [-1.0, 2.0] and hinted["y"][1] >= 4
    assert render(engine, tmp_path)["viewport"] == hinted
    # An explicit viewport wins over the hint.
    assert render(engine, tmp_path, viewport={"x": [0, 1], "y": [0, 1]})["viewport"] == {
        "x": [0.0, 1.0],
        "y": [0.0, 1.0],
    }


# ---- errors -----------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("kwargs", "needle"),
    [
        ({"format": "gif", "path": "{tmp}/a.gif"}, "format"),
        ({"format": "svg", "path": "relative.svg"}, "absolute"),
        ({"format": "svg", "path": "{tmp}/missing/a.svg"}, "does not exist"),
        ({"format": "svg", "path": "{tmp}/a.png"}, "ends in .png"),
        ({"format": "svg", "path": "{tmp}/a.json"}, "sidecar"),
        ({"format": "svg", "path": "{tmp}"}, "directory"),
        ({"format": "svg", "path": "{tmp}/a.svg", "dpi": 100}, "png only"),
        ({"format": "png", "path": "{tmp}/a.png", "dpi": 5}, "dpi"),
        ({"format": "svg", "path": "{tmp}/a.svg", "size": {"w_in": 0.1}}, "size.w_in"),
        ({"format": "svg", "path": "{tmp}/a.svg", "size": {"width": 3}}, "size"),
        ({"format": "svg", "path": "{tmp}/a.svg", "viewport": {"x": [1, 0], "y": [0, 1]}}, "viewport.x"),
        (
            {"format": "svg", "path": "{tmp}/a.svg", "style": {"mplstyle": "{tmp}/none.mplstyle"}},
            "style sheet",
        ),
        ({"format": "svg", "path": "{tmp}/a.svg", "title": 3}, "title"),
    ],
)
def test_bad_arguments_are_reported_and_write_nothing(tmp_path, kwargs, needle):
    kwargs = {k: (v.replace("{tmp}", str(tmp_path)) if isinstance(v, str) else v) for k, v in kwargs.items()}
    if isinstance(kwargs.get("style"), dict):
        kwargs["style"] = {"mplstyle": kwargs["style"]["mplstyle"].replace("{tmp}", str(tmp_path))}
    with pytest.raises(CallError) as info:
        export.render(curve("x"), **kwargs)
    assert info.value.kind == "bad_args" and needle in info.value.detail
    assert list(tmp_path.iterdir()) == []


def test_style_sheet_is_applied(tmp_path):
    sheet = tmp_path / "house.mplstyle"
    sheet.write_text("axes.facecolor: fafad2\n")
    plain = render(curve("x"), tmp_path, name="plain")
    styled = render(curve("x"), tmp_path, name="styled", style={"mplstyle": str(sheet)})
    assert "#fafad2" in Path(styled["path"]).read_text() and "#fafad2" not in Path(plain["path"]).read_text()
    sheet.write_text("no.such.setting: 1\nlines.linewidth: banana\n")
    with pytest.raises(CallError):
        render(curve("x"), tmp_path, name="broken", style={"mplstyle": str(sheet)})


def test_rpc_handler(tmp_path):
    class Methods:
        def __init__(self, engine):
            self.engine = engine
            self.table = {}

        def register(self, name, fn):
            self.table[name] = fn

    methods = Methods(curve("x**2"))
    export.register(methods)
    handler = methods.table["export.render"]
    ok = handler({"format": "svg", "path": str(tmp_path / "a.svg"), "size": SMALL})
    assert ok["certified"] is True and Path(ok["path"]).exists() and Path(ok["sidecar"]).exists()
    assert handler({"format": "gif", "path": str(tmp_path / "a.gif")})["error"]["kind"] == "bad_args"
    assert handler({"format": "svg"})["status"] == "error"
    assert (
        "unknown"
        in handler({"format": "svg", "path": str(tmp_path / "b.svg"), "colour": 1})["error"]["detail"]
    )
