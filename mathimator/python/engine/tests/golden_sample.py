"""Golden cases for the certified sampler: what each function must produce.

Each case lists the plotted expression, the viewport, the expected status, and the features
that must be found. ``must_flag`` cases are ones the sampler must not call certified.
"""

from __future__ import annotations

from dataclasses import dataclass, field

WIDE = {"x": [-10, 10], "y": [-5, 5]}
UNIT = {"x": [-1, 1], "y": [-2, 2]}


@dataclass
class Case:
    expr: str
    viewport: dict
    status: str  # "certified" or "sampled"
    asymptotes: list[float] = field(default_factory=list)
    holes: list[tuple[float, float]] = field(default_factory=list)  # (x, limit)
    # (x, y, closed) for each jump end
    jump_ends: list[tuple[float, float, bool]] = field(default_factory=list)
    points: list[tuple[float, float]] = field(default_factory=list)  # closed point markers
    reason: str | None = None  # a substring the reason must contain when sampled
    peak: float | None = None  # the polyline must reach this height (narrow spikes)
    x_min: float | None = None  # no polyline point left of this (undefined part)
    ends: tuple[float, float] | None = None  # polyline reaches exactly these x ends


CASES = {
    # Poles: asymptote markers; no segment crosses them.
    "reciprocal": Case("1/x", WIDE, "certified", asymptotes=[0.0]),
    "tangent": Case(
        "tan(x)",
        WIDE,
        "certified",
        asymptotes=[
            -7.853981633974483,
            -4.71238898038469,
            -1.5707963267948966,
            1.5707963267948966,
            4.71238898038469,
            7.853981633974483,
        ],
    ),
    "two_poles": Case("1/(x**2 - 1)", WIDE, "certified", asymptotes=[-1.0, 1.0]),
    # Jumps: both ends marked, with the defined value closed.
    "floor": Case(
        "floor(x)",
        {"x": [-2.5, 2.5], "y": [-4, 4]},
        "certified",
        jump_ends=[
            (-2, -3, False),
            (-2, -2, True),
            (-1, -2, False),
            (-1, -1, True),
            (0, -1, False),
            (0, 0, True),
            (1, 0, False),
            (1, 1, True),
            (2, 1, False),
            (2, 2, True),
        ],
    ),
    "sign": Case("sign(x)", WIDE, "certified", jump_ends=[(0, -1, False), (0, 1, False)], points=[(0, 0)]),
    "heaviside": Case(
        "heaviside(x)", WIDE, "certified", jump_ends=[(0, 0, False), (0, 1, False)], points=[(0, 0.5)]
    ),
    # Removable holes: an open marker at the limit.
    "sinc": Case("sin(x)/x", WIDE, "certified", holes=[(0.0, 1.0)]),
    "cancel": Case("(x**2 - 1)/(x - 1)", WIDE, "certified", holes=[(1.0, 2.0)]),
    # Oscillation without a limit must be flagged.
    "sin_inverse": Case("sin(1/x)", {"x": [-0.5, 0.5], "y": [-2, 2]}, "sampled", reason="oscillates"),
    # Narrow spikes must not be missed, even when narrower than a pixel.
    "spike": Case("exp(-1000*x**2)", {"x": [-10, 10], "y": [-0.5, 1.5]}, "certified", peak=1.0),
    "needle": Case("exp(-1000000*x**2)", {"x": [-10, 10], "y": [-0.5, 1.5]}, "certified", peak=1.0),
    # Undefined parts are not drawn.
    "log": Case("log(x)", UNIT, "certified", asymptotes=[0.0], x_min=0.0),
    "semicircle": Case("sqrt(1 - x**2)", {"x": [-1.5, 1.5], "y": [-1, 2]}, "certified", ends=(-1.0, 1.0)),
    # Smooth functions are certified.
    "cubic": Case("x**3 - 2*x", WIDE, "certified"),
    "quintic": Case("x**5/120 - x**3/6 + x", WIDE, "certified"),
    "sine": Case("sin(x)", WIDE, "certified"),
    "kink": Case("abs(x - 1)", WIDE, "certified"),
    "gaussian": Case("exp(-x**2)", WIDE, "certified"),
    # No interval arithmetic for gamma: drawn, but flagged.
    "gamma": Case("gamma(x)", {"x": [0.5, 5], "y": [0, 30]}, "sampled", reason="interval arithmetic"),
}
