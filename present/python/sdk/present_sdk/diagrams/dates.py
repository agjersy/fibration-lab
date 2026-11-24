"""ISO date handling for timelines: ``YYYY``, ``YYYY-MM``, ``YYYY-MM-DD``, ``YYYY-Qn``."""

from __future__ import annotations

import re
from datetime import date, timedelta

from ..content import ContentError

_RE = re.compile(r"^(\d{4})(?:-(?:Q([1-4])|(\d{2})(?:-(\d{2}))?))?$", re.IGNORECASE)
MONTHS = ("Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")
UNITS = ("day", "week", "month", "quarter", "year")


def _add_months(d: date, n: int) -> date:
    m = d.year * 12 + d.month - 1 + n
    return date(m // 12, m % 12 + 1, 1)


def parse_period(value: str) -> tuple[date, date]:
    """The half-open period ``[start, end)`` a date string names."""
    m = _RE.match(str(value).strip())
    if not m:
        raise ContentError(f"not an ISO date (YYYY, YYYY-MM, YYYY-MM-DD, YYYY-Qn): {value!r}")
    year, quarter, month, day = m.groups()
    try:
        if quarter:
            start = date(int(year), 3 * int(quarter) - 2, 1)
            return start, _add_months(start, 3)
        if day:
            start = date(int(year), int(month), int(day))
            return start, start + timedelta(days=1)
        if month:
            start = date(int(year), int(month), 1)
            return start, _add_months(start, 1)
        return date(int(year), 1, 1), date(int(year) + 1, 1, 1)
    except ValueError as exc:
        raise ContentError(f"invalid date {value!r}: {exc}") from None


def parse_start(value: str) -> date:
    return parse_period(value)[0]


def parse_end(value: str) -> date:
    """End of the named period (exclusive), so ``2026-Q2`` as an end reaches the end of June."""
    return parse_period(value)[1]


def floor_unit(d: date, unit: str) -> date:
    if unit == "year":
        return date(d.year, 1, 1)
    if unit == "quarter":
        return date(d.year, 3 * ((d.month - 1) // 3) + 1, 1)
    if unit == "month":
        return date(d.year, d.month, 1)
    if unit == "week":
        return d - timedelta(days=d.weekday())
    return d


def next_unit(d: date, unit: str, n: int = 1) -> date:
    """``d`` (already on a unit boundary) advanced by ``n`` units."""
    if unit == "year":
        return date(d.year + n, 1, 1)
    if unit == "quarter":
        return _add_months(d, 3 * n)
    if unit == "month":
        return _add_months(d, n)
    if unit == "week":
        return d + timedelta(days=7 * n)
    return d + timedelta(days=n)


def ceil_unit(d: date, unit: str) -> date:
    f = floor_unit(d, unit)
    return f if f == d else next_unit(f, unit)


def auto_unit(start: date, end: date) -> str:
    days = (end - start).days
    if days > 365 * 4:
        return "year"
    if days > 365 + 190:
        return "quarter"
    if days > 75:
        return "month"
    if days > 21:
        return "week"
    return "day"


def ticks(start: date, end: date, unit: str, max_ticks: int = 12) -> tuple[list[date], int]:
    """Unit boundaries in ``[start, end]``, thinned to at most ``max_ticks`` intervals; and the step used."""
    first = ceil_unit(start, unit)
    count = 0
    d = first
    while d < end and count < 100000:
        d = next_unit(d, unit)
        count += 1
    step = 1
    nice = {"day": (1, 2, 7, 14), "week": (1, 2, 4), "month": (1, 2, 3, 6), "quarter": (1, 2, 4), "year": (1, 2, 5, 10)}[unit]
    for s in nice:
        step = s
        if count / s <= max_ticks:
            break
    else:
        step = max(1, -(-count // max_ticks))
    out = []
    d = first
    while d <= end:
        out.append(d)
        d = next_unit(d, unit, step)
    return out, step


def tick_label(d: date, unit: str) -> str:
    if unit == "year":
        return str(d.year)
    if unit == "quarter":
        return f"Q{(d.month - 1) // 3 + 1} {d.year % 100:02d}"
    if unit == "month":
        return f"{MONTHS[d.month - 1]} {d.year % 100:02d}"
    return f"{d.day} {MONTHS[d.month - 1]}"


def format_date(value: str) -> str:
    """A short display form of an item's date string."""
    m = _RE.match(str(value).strip())
    if not m:
        return str(value)
    year, quarter, month, day = m.groups()
    if quarter:
        return f"Q{quarter} {year}"
    if day:
        return f"{int(day)} {MONTHS[int(month) - 1]} {year}"
    if month:
        return f"{MONTHS[int(month) - 1]} {year}"
    return year
