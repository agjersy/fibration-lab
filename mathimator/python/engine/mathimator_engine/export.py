"""Publication export: a scene drawn by Matplotlib from checked samples (ARCHITECTURE 9.3, WP-0.9).

The exporter never evaluates an expression. Every curve, fill, and marker it draws comes from
``sample`` at the output's resolution, so what is on paper carries the same guarantee as what is
on the canvas. When an object could only be ``sampled``, the figure says so in a small footnote
and the sidecar records why: a plot that could not be certified says so.

Contract
--------

JSON-RPC method ``export.render``::

    params: {format: "svg" | "pdf" | "png", path: string,       # absolute; the directory must exist
             viewport?: {x: [lo, hi], y: [lo, hi]},
             size?: {w_in, h_in},                                 # default 6.4 x 4.0 inches
             dpi?: number,                                        # png only; default 200
             style?: {mplstyle?: path}, title?: string,
             params?: {name: number}}                             # parameter overrides for this export
    result: {path, sidecar, format, viewport, objects: [{id, kind, status, reason?}],
             certified: bool, hash, revision}
    error:  {status: "error", error: {kind, detail, hint?}}

* **Viewport.** Given, else the scene's ``cam.view`` window (its y range fitted when the hint
  has none), else fitted to the objects with a margin, else [-10, 10] x [-10, 10].
* **Resolution.** Objects are sampled on the output's pixel grid: ``size * dpi`` for png, a
  300 dpi grid for svg and pdf. The grid is the whole figure, slightly finer than the axes area,
  so the half-pixel guarantee holds with room to spare.
* **Sidecar.** ``<stem>.json`` next to the figure (schema 1, kind ``scene-export``): the scene
  revision and hash, the viewport, size, the parameter values used, and for every object its
  status, reason, and expressions by content hash, text, and LaTeX. This is the record a
  ``scene_cite`` reads.
* **Determinism.** The same scene and arguments give byte-identical svg and identical png
  pixels: no pyplot state, a fixed svg hash salt, text kept as text, and no dates in metadata.
  Only the sidecar's ``created_at`` differs between runs.
* Both files are written to a temporary name and renamed, so a failed export leaves no partial
  file behind.

Drawn per kind: curves (explicit, parametric, polar, implicit) as one line per segment, never
joined across a break; asymptotes as thin dashed verticals; holes and open jump ends as open
circles, closed ends and isolated points as dots; regions, areas, and Riemann sums as fills;
data as points or a line; points with their labels; vectors and note arrows as arrows; labels
and formulas (mathtext, plain text when mathtext cannot read the LaTeX); highlights as a wide
translucent overlay.
"""

from __future__ import annotations

import datetime
import json
import math
import os
import tempfile
from pathlib import Path
from typing import Any

import sympy as sp

from . import __version__
from .errors import CallError
from .handles import content_hash
from .registry import API_VERSION
from .sample import _check_viewport, _substitution, sample

FORMATS = ("svg", "pdf", "png")
DEFAULT_SIZE = (6.4, 4.0)
DEFAULT_DPI = 200
VECTOR_DPI = 300  # the sampling grid for svg and pdf
MAX_PIXELS = 16384
DEFAULT_VIEW = (-10.0, 10.0, -10.0, 10.0)
PROBE_PIXELS = (400, 400)
PROBE_Y = 1e4  # explicit curves are probed over y in [-PROBE_Y, PROBE_Y] to find their extent
PROBE_BOX = 50.0  # other objects are probed over [-PROBE_BOX, PROBE_BOX] squared
MARGIN = 0.08
SVG_HASH_SALT = "mathimator"

# A fixed palette (the Okabe-Ito colours, readable with common colour-vision deficiencies).
PALETTE = ("#0072B2", "#D55E00", "#009E73", "#CC79A7", "#E69F00", "#56B4E9", "#000000")
DASHES = {"solid": "-", "dashed": "--", "dotted": ":"}
CURVE_KINDS = ("explicit", "parametric", "polar", "implicit")
NOTE_KINDS = ("label", "latex", "arrow")

RC = {
    "svg.hashsalt": SVG_HASH_SALT,
    "svg.fonttype": "none",
    "pdf.fonttype": 42,
    "font.family": "DejaVu Sans",
    "mathtext.fontset": "dejavusans",
    "path.simplify": False,
    "axes.unicode_minus": True,
}


def _matplotlib():
    """Import Matplotlib for headless use. The sandbox has no writable home directory, so the
    configuration directory is pointed at a temporary one before the first import."""
    if not os.environ.get("MPLCONFIGDIR"):
        os.environ["MPLCONFIGDIR"] = tempfile.mkdtemp(prefix="mathimator-mpl-")
    import matplotlib

    return matplotlib


# ---- arguments -------------------------------------------------------------------------


def _number(value: Any, what: str, lo: float, hi: float) -> float:
    ok = isinstance(value, int | float) and not isinstance(value, bool) and math.isfinite(value)
    if not ok or not lo <= value <= hi:
        raise CallError("bad_args", f"{what} must be a number between {lo:g} and {hi:g}")
    return float(value)


def _check_path(path: Any, fmt: str) -> Path:
    if not isinstance(path, str) or not path:
        raise CallError("bad_args", "path must be the absolute path of the file to write")
    out = Path(path)
    if not out.is_absolute():
        raise CallError("bad_args", f"path must be absolute, not {path!r}")
    if not out.parent.is_dir():
        raise CallError("bad_args", f"the directory {str(out.parent)!r} does not exist")
    if out.is_dir():
        raise CallError("bad_args", f"{path!r} is a directory")
    if out.suffix.lower() == ".json":
        raise CallError("bad_args", "path must not end in .json: that name is the sidecar's")
    if out.suffix and out.suffix.lower().lstrip(".") != fmt:
        raise CallError("bad_args", f"the file name ends in {out.suffix} but the format is {fmt}")
    return out


def _check_size(size: Any) -> tuple[float, float]:
    if size is None:
        return DEFAULT_SIZE
    if not isinstance(size, dict) or set(size) - {"w_in", "h_in"}:
        raise CallError("bad_args", "size must be {w_in, h_in} in inches")
    return (
        _number(size.get("w_in", DEFAULT_SIZE[0]), "size.w_in", 1, 50),
        _number(size.get("h_in", DEFAULT_SIZE[1]), "size.h_in", 1, 50),
    )


def _check_style(style: Any) -> str | None:
    if style is None:
        return None
    if not isinstance(style, dict) or set(style) - {"mplstyle"}:
        raise CallError("bad_args", "style must be {mplstyle?: path}")
    sheet = style.get("mplstyle")
    if sheet is None:
        return None
    if not isinstance(sheet, str) or not Path(sheet).is_file():
        raise CallError("bad_args", f"the style sheet {sheet!r} does not exist")
    return sheet


def _read_style(matplotlib, sheet: str) -> dict:
    """The settings of a style sheet. Matplotlib only logs a warning for a setting it cannot
    read and carries on; here that is an error, so a figure never silently ignores its style."""
    import logging

    problems: list[str] = []

    class Collect(logging.Handler):
        def emit(self, record: logging.LogRecord) -> None:
            problems.append(" ".join(record.getMessage().split()))

    logger = logging.getLogger("matplotlib")
    handler = Collect(level=logging.WARNING)
    logger.addHandler(handler)
    try:
        settings = dict(matplotlib.rc_params_from_file(sheet, use_default_template=False))
    except Exception as exc:  # unreadable, or not a style sheet at all
        raise CallError("bad_args", f"cannot read the style sheet {sheet!r}: {exc}") from None
    finally:
        logger.removeHandler(handler)
    if problems:
        raise CallError(
            "bad_args", f"the style sheet {sheet!r} has a setting Matplotlib cannot read: {problems[0][:200]}"
        )
    return settings


# ---- viewport --------------------------------------------------------------------------


def _extent_of(result: dict) -> tuple[list[float], list[float], bool]:
    """The x and y values an object occupies, and whether it has an asymptote."""
    xs: list[float] = []
    ys: list[float] = []
    for seg in result.get("segments", []):
        for x, y in seg:
            xs.append(x)
            ys.append(y)
    for m in result.get("markers", []):
        if m.get("kind") == "asymptote":
            continue
        xs.append(m["x"])
        if m.get("y") is not None:
            ys.append(m["y"])
    for x, y in result.get("points", []):
        xs.append(x)
        ys.append(y)
    fill = result.get("fill") or {}
    for poly in fill.get("polygons", []):
        for x, y in poly:
            xs.append(x)
            ys.append(y)
    for x0, y0, x1, y1 in fill.get("rects", []):
        xs += [x0, x1]
        ys += [y0, y1]
    poles = any(m.get("kind") == "asymptote" for m in result.get("markers", []))
    return xs, ys, poles


def _spread(values: list[float], trim: bool) -> tuple[float, float] | None:
    """The range of the values; with ``trim``, the central 90 % (a curve with a pole)."""
    finite = sorted(v for v in values if math.isfinite(v))
    if not finite:
        return None
    if trim and len(finite) >= 20:
        k = len(finite) // 20
        finite = finite[k : len(finite) - k]
    return finite[0], finite[-1]


def _pad(lo: float, hi: float) -> tuple[float, float]:
    if hi - lo < 1e-12 * max(1.0, abs(lo), abs(hi)):
        half = max(1.0, abs(lo) * 0.5)
        return lo - half, hi + half
    pad = (hi - lo) * MARGIN
    return lo - pad, hi + pad


def _spec_float(item: dict, default: float) -> float:
    """A scene-state scalar ({text, value}) as a float; ``default`` when it is infinite."""
    value = item.get("value")
    return float(value) if isinstance(value, int | float) and math.isfinite(value) else default


def _fit_x(engine, params: dict | None) -> tuple[float, float]:
    """The x range the scene's objects ask for: curve domains, and where everything else lies."""
    los: list[float] = []
    his: list[float] = []
    box = {"x": [-PROBE_BOX, PROBE_BOX], "y": [-PROBE_BOX, PROBE_BOX]}
    for obj in [*engine.scene.objects.values(), *engine.scene.notes.values()]:
        if obj.kind == "explicit":
            lo, hi = obj.spec["domain"]
            # An infinite domain is shown over the default window.
            los.append(_spec_float(lo, DEFAULT_VIEW[0]))
            his.append(_spec_float(hi, DEFAULT_VIEW[1]))
            continue
        if obj.kind in ("area", "riemann"):
            lo, hi = obj.spec["over"]
            los.append(_spec_float(lo, DEFAULT_VIEW[0]))
            his.append(_spec_float(hi, DEFAULT_VIEW[1]))
            continue
        if obj.kind == "highlight" or (obj.kind in ("label", "latex") and "object" in obj.spec["at"]):
            continue
        xs, _, _ = _extent_of(sample(engine, obj.id, box, PROBE_PIXELS, params))
        span = _spread(xs, trim=False)
        if span is not None:
            los.append(span[0])
            his.append(span[1])
    if not los:
        return DEFAULT_VIEW[0], DEFAULT_VIEW[1]
    lo, hi = min(los), max(his)
    curves_only = all(
        o.kind in ("explicit", "area", "riemann", "highlight") for o in engine.scene.objects.values()
    )
    return (lo, hi) if curves_only and hi > lo else _pad(lo, hi)


def _fit_y(engine, xlo: float, xhi: float, params: dict | None) -> tuple[float, float]:
    """The y range that shows the objects over [xlo, xhi]; curves with poles are trimmed."""
    ys: list[float] = []
    trim = False
    probe = {"x": [xlo, xhi], "y": [-PROBE_Y, PROBE_Y]}
    for obj in [*engine.scene.objects.values(), *engine.scene.notes.values()]:
        if obj.kind == "highlight" or (obj.kind in ("label", "latex") and "object" in obj.spec["at"]):
            continue
        if obj.kind in ("implicit", "region"):
            # A y range this wide would make their grid cells useless; probe a square instead.
            half = max(abs(xlo), abs(xhi), 1.0)
            view = {"x": [xlo, xhi], "y": [-half, half]}
        else:
            view = probe
        result = sample(engine, obj.id, view, PROBE_PIXELS, params)
        xs_o, ys_o, poles = _extent_of(result)
        on_screen = [y for x, y in zip(xs_o, ys_o) if xlo <= x <= xhi] if len(xs_o) == len(ys_o) else ys_o
        clipped = any(abs(y) >= PROBE_Y * 0.999 for y in on_screen)
        if obj.kind in ("explicit", "area") and (poles or clipped):
            trim = True
            span = _spread([y for y in on_screen if abs(y) < PROBE_Y * 0.999], trim=True)
            if span is not None:
                ys += list(span)
            continue
        ys += on_screen
    span = _spread(ys, trim=False)
    if span is None:
        return DEFAULT_VIEW[2], DEFAULT_VIEW[3]
    lo, hi = _pad(*span)
    if trim:
        # Keep the axis in sight for a curve that runs off to infinity on both sides.
        lo, hi = min(lo, 0.0), max(hi, 0.0)
    return lo, hi


def default_viewport(engine, params: dict | None = None) -> dict:
    view = engine.scene.view or {}
    if view.get("mode") == "window":
        xlo, xhi = (_spec_float(v, d) for v, d in zip(view["x"], DEFAULT_VIEW[:2]))
        if "y" in view:
            ylo, yhi = (_spec_float(v, d) for v, d in zip(view["y"], DEFAULT_VIEW[2:]))
        else:
            ylo, yhi = _fit_y(engine, xlo, xhi, params)
        return {"x": [xlo, xhi], "y": [ylo, yhi]}
    if not engine.scene.objects and not engine.scene.notes:
        return {"x": list(DEFAULT_VIEW[:2]), "y": list(DEFAULT_VIEW[2:])}
    xlo, xhi = _fit_x(engine, params)
    ylo, yhi = _fit_y(engine, xlo, xhi, params)
    return {"x": [xlo, xhi], "y": [ylo, yhi]}


# ---- drawing ---------------------------------------------------------------------------


def _merge_rects(rects: list[list[float]]) -> list[list[float]]:
    """Join region cells that share a row and touch, so a filled disk is hundreds of
    rectangles in the file instead of ten thousand."""
    rows: dict[tuple[float, float], list[list[float]]] = {}
    for x0, y0, x1, y1 in rects:
        rows.setdefault((y0, y1), []).append([x0, x1])
    out: list[list[float]] = []
    for (y0, y1), spans in sorted(rows.items()):
        spans.sort()
        cur = spans[0][:]
        for x0, x1 in spans[1:]:
            if x0 <= cur[1] + 1e-12 * max(1.0, abs(cur[1])):
                cur[1] = max(cur[1], x1)
            else:
                out.append([cur[0], y0, cur[1], y1])
                cur = [x0, x1]
        out.append([cur[0], y0, cur[1], y1])
    return out


def _mathtext_ok(matplotlib, text: str) -> bool:
    from matplotlib.mathtext import MathTextParser

    try:
        MathTextParser("agg").parse(text)
    except Exception:  # mathtext raises ValueError or a pyparsing error; any failure means "no"
        return False
    return True


def _anchor_point(result: dict | None, view: tuple[float, float, float, float]) -> tuple[float, float]:
    """Where a note attached to an object goes: the object's highest visible point."""
    xlo, xhi, ylo, yhi = view
    best: tuple[float, float] | None = None
    if result is not None:
        candidates = [p for seg in result.get("segments", []) for p in seg]
        candidates += [[m["x"], m["y"]] for m in result.get("markers", []) if m.get("y") is not None]
        candidates += result.get("points", [])
        for poly in (result.get("fill") or {}).get("polygons", []):
            candidates += poly
        for x, y in candidates:
            if xlo <= x <= xhi and ylo <= y <= yhi and (best is None or y > best[1]):
                best = (x, y)
    return best if best is not None else ((xlo + xhi) / 2, (ylo + yhi) / 2)


class _Painter:
    def __init__(self, matplotlib, ax, view: tuple[float, float, float, float]):
        self.mpl = matplotlib
        self.ax = ax
        self.view = view
        self.next_color = 0
        self.has_label = False

    def color(self, obj) -> str:
        if obj.style.get("color"):
            return obj.style["color"]
        c = PALETTE[self.next_color % len(PALETTE)]
        self.next_color += 1
        return c

    def lines(
        self, obj, result: dict, color: str, width: float, alpha: float, tag: str = "seg", zorder: float = 3
    ):
        dash = DASHES.get(obj.style.get("dash", "solid"), "-")
        label = obj.style.get("label") if tag == "seg" else None
        for i, seg in enumerate(result.get("segments", [])):
            if len(seg) < 2:
                continue
            (line,) = self.ax.plot(
                [p[0] for p in seg],
                [p[1] for p in seg],
                linestyle=dash,
                color=color,
                linewidth=width,
                alpha=alpha,
                zorder=zorder,
                solid_capstyle="round",
                label=label if i == 0 else None,
            )
            line.set_gid(f"obj-{obj.id}-{tag}-{i}")
            if label and i == 0:
                self.has_label = True

    def markers(self, obj, result: dict, color: str):
        ax = self.ax
        for i, m in enumerate(result.get("markers", [])):
            kind = m["kind"]
            gid = f"obj-{obj.id}-marker-{kind}-{i}"
            if kind == "asymptote":
                line = ax.axvline(
                    m["x"], color=color, linewidth=0.6, linestyle=(0, (4, 3)), alpha=0.55, zorder=2
                )
                line.set_gid(gid)
                continue
            if m.get("y") is None:
                continue
            closed = bool(m.get("closed", kind == "point"))
            (dot,) = ax.plot(
                [m["x"]],
                [m["y"]],
                marker="o",
                markersize=4.5,
                markerfacecolor=color if closed else "white",
                markeredgecolor=color,
                markeredgewidth=1.1,
                linestyle="none",
                zorder=5,
                clip_on=True,
            )
            dot.set_gid(gid)
            if m.get("label"):
                text = ax.annotate(
                    m["label"],
                    (m["x"], m["y"]),
                    xytext=(5, 5),
                    textcoords="offset points",
                    fontsize=8,
                    zorder=6,
                )
                text.set_gid(f"obj-{obj.id}-label")

    def fill(self, obj, result: dict, color: str, alpha: float):
        from matplotlib.collections import PolyCollection

        fill = result.get("fill") or {}
        polys = [p for p in fill.get("polygons", []) if len(p) >= 3]
        for x0, y0, x1, y1 in _merge_rects(fill.get("rects", [])):
            polys.append([[x0, y0], [x1, y0], [x1, y1], [x0, y1]])
        if not polys:
            return
        outlined = obj.kind == "riemann"
        coll = PolyCollection(
            polys,
            facecolors=color,
            edgecolors=color if outlined else "none",
            linewidths=0.5 if outlined else 0,
            alpha=alpha,
            antialiaseds=outlined,
            zorder=1.5,
        )
        coll.set_gid(f"obj-{obj.id}-fill")
        self.ax.add_collection(coll)

    def arrow(self, obj, result: dict, color: str, width: float, alpha: float):
        a, b = result["arrow"]["from"], result["arrow"]["to"]
        note = self.ax.annotate(
            "",
            xy=b,
            xytext=a,
            arrowprops={
                "arrowstyle": "-|>",
                "color": color,
                "linewidth": width,
                "alpha": alpha,
                "shrinkA": 0,
                "shrinkB": 0,
            },
            zorder=4,
        )
        # The arrow itself is the annotation's patch; an id on the empty text would not be written.
        note.arrow_patch.set_gid(f"obj-{obj.id}-arrow")

    def text(self, obj, at: tuple[float, float], content: str, color: str):
        note = self.ax.annotate(
            content,
            at,
            xytext=(4, 6),
            textcoords="offset points",
            fontsize=9,
            color=color,
            zorder=6,
            clip_on=True,
        )
        note.set_gid(f"obj-{obj.id}-text")


def _draw(matplotlib, ax, engine, results: dict[str, dict], view: tuple[float, float, float, float]) -> None:
    painter = _Painter(matplotlib, ax, view)
    for obj in engine.scene.objects.values():
        result = results[obj.id]
        alpha = float(obj.style.get("alpha", 1.0))
        width = float(obj.style.get("width", 1.6))
        if obj.kind == "highlight":
            color = obj.style.get("color", "#FFD400")
            painter.lines(
                obj,
                result,
                color,
                max(width * 3.5, 5.0),
                float(obj.style.get("alpha", 0.6)),
                "highlight",
                2.5,
            )
            painter.fill(obj, result, color, 0.3)
            continue
        color = painter.color(obj)
        if obj.kind in CURVE_KINDS:
            painter.lines(obj, result, color, width, alpha)
            painter.markers(obj, result, color)
        elif obj.kind in ("region", "riemann") or obj.kind == "area":
            painter.fill(obj, result, color, float(obj.style.get("alpha", 0.3)))
        elif obj.kind == "point":
            painter.markers(obj, result, color)
        elif obj.kind == "vector":
            painter.arrow(obj, result, color, width, alpha)
        elif obj.kind == "data":
            if obj.spec.get("mode") == "line":
                painter.lines(obj, result, color, width, alpha)
            else:
                pts = result.get("points", [])
                (dots,) = ax.plot(
                    [p[0] for p in pts],
                    [p[1] for p in pts],
                    marker="o",
                    markersize=3.5,
                    linestyle="none",
                    color=color,
                    alpha=alpha,
                    zorder=3.5,
                    label=obj.style.get("label"),
                )
                dots.set_gid(f"obj-{obj.id}-points")
                painter.has_label = painter.has_label or bool(obj.style.get("label"))
    for note in engine.scene.notes.values():
        result = results[note.id]
        color = note.style.get("color", "#222222")
        if note.kind == "arrow":
            painter.arrow(
                note, result, color, float(note.style.get("width", 1.0)), float(note.style.get("alpha", 1.0))
            )
            continue
        if "anchor" in result:
            at = _anchor_point(results.get(result["anchor"]["object"]), view)
        else:
            at = (result["markers"][0]["x"], result["markers"][0]["y"])
        if note.kind == "latex":
            content = f"${note.spec['latex']}$"
            if not _mathtext_ok(matplotlib, content):
                content = sp.sstr(note.exprs["expr"])
        else:
            # A label is plain text: a dollar sign in it must not start mathtext.
            content = note.spec["text"].replace("$", r"\$")
        painter.text(note, at, content, color)
    if painter.has_label:
        legend = ax.legend(loc="best", fontsize=8, frameon=True, framealpha=0.85)
        legend.set_gid("legend")


def _axes(ax, view: tuple[float, float, float, float], title: str | None) -> None:
    xlo, xhi, ylo, yhi = view
    ax.set_xlim(xlo, xhi)
    ax.set_ylim(ylo, yhi)
    ax.grid(True, color="#D9D9D9", linewidth=0.5, zorder=0)
    ax.set_axisbelow(True)
    for side in ("top", "right"):
        ax.spines[side].set_visible(False)
    # Axes through the origin when it is in view, as a mathematics figure has them.
    if xlo < 0 < xhi:
        ax.spines["left"].set_position("zero")
    if ylo < 0 < yhi:
        ax.spines["bottom"].set_position("zero")
    for side in ("left", "bottom"):
        ax.spines[side].set_linewidth(0.8)
        ax.spines[side].set_color("#444444")
    ax.tick_params(labelsize=8, color="#444444", width=0.6, length=3)
    if title:
        ax.set_title(title.replace("$", r"\$"), fontsize=10)


# ---- the export ------------------------------------------------------------------------


def _atomic_write(path: Path, data: bytes) -> None:
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        tmp.write_bytes(data)
        os.replace(tmp, path)
    finally:
        if tmp.exists():
            tmp.unlink()


def _sidecar_objects(engine, results: dict[str, dict]) -> list[dict]:
    out = []
    for obj in [*engine.scene.objects.values(), *engine.scene.notes.values()]:
        result = results[obj.id]
        entry: dict[str, Any] = {"id": obj.id, "kind": obj.kind, "status": result["status"]}
        if result.get("reason"):
            entry["reason"] = result["reason"]
        entry["exprs"] = {
            name: {"hash": content_hash(expr), "text": sp.sstr(expr), "latex": sp.latex(expr)}
            for name, expr in sorted(obj.exprs.items())
        }
        if obj.origin:
            entry["origin"] = obj.origin
        out.append(entry)
    return out


def render(
    engine,
    format: str,
    path: str,
    viewport: Any = None,
    size: Any = None,
    dpi: Any = None,
    style: Any = None,
    title: Any = None,
    params: dict | None = None,
) -> dict:
    """Draw the scene to ``path`` and write its sidecar (see the module docstring)."""
    if format not in FORMATS:
        raise CallError("bad_args", f"format must be one of {', '.join(FORMATS)}")
    out_path = _check_path(path, format)
    w_in, h_in = _check_size(size)
    if dpi is not None and format != "png":
        raise CallError("bad_args", "dpi applies to png only; svg and pdf are vector formats")
    dpi_v = _number(DEFAULT_DPI if dpi is None else dpi, "dpi", 36, 1200)
    sheet = _check_style(style)
    if title is not None and (not isinstance(title, str) or len(title) > 300):
        raise CallError("bad_args", "title must be text of at most 300 characters")
    subs = _substitution(engine, params)  # validates the overrides
    if viewport is None:
        viewport = default_viewport(engine, params)
    view = _check_viewport(viewport)
    viewport = {"x": [view[0], view[1]], "y": [view[2], view[3]]}

    grid_dpi = dpi_v if format == "png" else VECTOR_DPI
    pixels = (min(round(w_in * grid_dpi), MAX_PIXELS), min(round(h_in * grid_dpi), MAX_PIXELS))
    everything = [*engine.scene.objects.values(), *engine.scene.notes.values()]
    results = {obj.id: sample(engine, obj.id, viewport, pixels, params) for obj in everything}
    sampled = [obj.id for obj in everything if results[obj.id]["status"] != "certified"]

    matplotlib = _matplotlib()
    from matplotlib.backends.backend_agg import FigureCanvasAgg
    from matplotlib.figure import Figure

    with matplotlib.rc_context():
        matplotlib.rcdefaults()
        if sheet is not None:
            matplotlib.rcParams.update(_read_style(matplotlib, sheet))
        matplotlib.rcParams.update(RC)
        fig = Figure(figsize=(w_in, h_in), dpi=dpi_v if format == "png" else 72)
        FigureCanvasAgg(fig)
        ax = fig.add_axes((0.07, 0.09, 0.9, 0.84 if title else 0.88))
        _axes(ax, view, title)
        _draw(matplotlib, ax, engine, results, view)
        if sampled:
            # The product rule: a plot that could not be certified says so, on the figure itself.
            note = fig.text(
                0.01,
                0.012,
                "sampled, not certified: " + ", ".join(sampled),
                fontsize=5.5,
                color="#777777",
                ha="left",
            )
            note.set_gid("footnote-sampled")
        metadata = {
            "svg": {"Date": None},
            "pdf": {"CreationDate": None, "ModDate": None},
            "png": {"Software": None},
        }[format]
        tmp = out_path.with_name(f".{out_path.name}.{os.getpid()}.tmp")
        try:
            with open(tmp, "wb") as handle:
                fig.savefig(handle, format=format, metadata=metadata)
            os.replace(tmp, out_path)
        except OSError as exc:
            raise CallError("bad_args", f"cannot write {str(out_path)!r}: {exc.strerror or exc}") from None
        finally:
            if tmp.exists():
                tmp.unlink()

    objects = [
        {k: v for k, v in entry.items() if k in ("id", "kind", "status", "reason")}
        for entry in _sidecar_objects(engine, results)
    ]
    scene_hash = engine.scene.hash()
    sidecar_path = out_path.with_suffix(".json")
    sidecar = {
        "schema": 1,
        "kind": "scene-export",
        "format": format,
        "created_at": datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "scene": {"revision": engine.scene.revision, "hash": scene_hash},
        "api_version": API_VERSION,
        "engine_version": __version__,
        "viewport": viewport,
        "size": {"w_in": w_in, "h_in": h_in, **({"dpi": dpi_v} if format == "png" else {})},
        "params": {str(sym): float(value) for sym, value in sorted(subs.items(), key=lambda kv: str(kv[0]))},
        "objects": _sidecar_objects(engine, results),
        "certified": not sampled,
    }
    if title:
        sidecar["title"] = title
    try:
        _atomic_write(
            sidecar_path,
            (json.dumps(sidecar, indent=2, sort_keys=True, allow_nan=False) + "\n").encode("utf-8"),
        )
    except OSError as exc:
        raise CallError("bad_args", f"cannot write {str(sidecar_path)!r}: {exc.strerror or exc}") from None
    return {
        "path": str(out_path),
        "sidecar": str(sidecar_path),
        "format": format,
        "viewport": viewport,
        "objects": objects,
        "certified": not sampled,
        "hash": scene_hash,
        "revision": engine.scene.revision,
    }


def register(methods) -> None:
    known = ("format", "path", "viewport", "size", "dpi", "style", "title", "params")

    def handler(params: dict) -> dict:
        try:
            unknown = sorted(set(params) - set(known))
            if unknown:
                raise CallError("bad_args", f"unknown argument(s): {', '.join(unknown)}")
            if "format" not in params or "path" not in params:
                raise CallError("bad_args", "export.render needs format and path")
            return render(methods.engine, **{k: params[k] for k in known if k in params})
        except CallError as exc:
            return {"status": "error", "error": exc.to_json()}

    methods.register("export.render", handler)
