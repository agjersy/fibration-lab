"""Theme access for scripts: colors and fonts by role, never by literal value."""

from __future__ import annotations

from .template import THEME_COLORS


def color(name: str) -> str:
    """Validate and return a theme color name usable wherever the SDK takes a color."""
    if name not in THEME_COLORS:
        raise ValueError(f"unknown theme color {name!r}; use one of {', '.join(THEME_COLORS)}")
    return name


def font(role: str = "minor") -> str:
    """The OOXML theme font reference for ``major`` (headings) or ``minor`` (body)."""
    if role not in ("major", "minor"):
        raise ValueError("font role must be 'major' or 'minor'")
    return "+mj-lt" if role == "major" else "+mn-lt"
