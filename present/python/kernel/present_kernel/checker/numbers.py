"""The number check (PROTOCOL.md section 8.3): do the numbers in a cited run appear in its source?"""

from __future__ import annotations

import re
from dataclasses import dataclass

_SCALES = {
    "k": 1e3, "thousand": 1e3, "m": 1e6, "mn": 1e6, "mm": 1e6, "million": 1e6, "mio": 1e6,
    "bn": 1e9, "b": 1e9, "billion": 1e9, "tn": 1e12, "trillion": 1e12,
}  # fmt: skip
_CURRENCY = "€$£¥"
_NUM = re.compile(
    r"(?<![\w.])(?P<sign>[-−–+])?(?P<cur>[€$£¥])?\s?(?P<num>\d{1,3}(?:[,  ]\d{3})+(?:\.\d+)?|\d+(?:[.,]\d+)?)"
    r"\s?(?P<suffix>%|percent|pts?\b|pp\b|bps\b|k\b|thousand\b|mn\b|mm\b|mio\b|million\b|m\b|bn\b|billion\b|b\b|tn\b|trillion\b)?",
    re.IGNORECASE,
)
_ORDINAL = re.compile(r"^(?:Q[1-4]|H[12]|FY\d{2,4}|[12]\d{3})$", re.IGNORECASE)


@dataclass(frozen=True)
class Number:
    text: str
    value: float
    decimals: int
    percent: bool
    unit: float = 1.0  # the scale the number was written in (1e9 for "1.2bn")


def _parse_num(raw: str) -> tuple[float, int] | None:
    s = raw.replace(" ", "").replace(" ", "").replace(" ", "")
    if "," in s and "." in s:
        s = s.replace(",", "")
    elif "," in s:
        head, _, tail = s.rpartition(",")
        s = head.replace(",", "") + tail if len(tail) == 3 and head else s.replace(",", ".")
    try:
        value = float(s)
    except ValueError:
        return None
    decimals = len(s.split(".")[1]) if "." in s else 0
    return value, decimals


def extract_numbers(text: str) -> list[Number]:
    """Numeric expressions in ``text``; period labels such as Q3, FY26, and bare years are skipped."""
    out: list[Number] = []
    for m in _NUM.finditer(text):
        start = m.start()
        # Skip numbers glued to a preceding letter (Q3, FY26, H1) and bare four-digit years.
        prefix = text[max(0, start - 3):start]
        token = (re.findall(r"[A-Za-z]+$", prefix) or [""])[0] + m.group("num")
        if _ORDINAL.match(token) and not m.group("suffix") and not m.group("cur"):
            continue
        parsed = _parse_num(m.group("num"))
        if parsed is None:
            continue
        value, decimals = parsed
        if m.group("sign") in ("-", "−", "–"):
            value = -value
        suffix = (m.group("suffix") or "").lower()
        percent = suffix in ("%", "percent")
        scale = _SCALES.get(suffix)
        if scale:
            value *= scale
        out.append(Number(m.group(0).strip(), value, decimals, percent, scale or 1.0))
    return out


def _candidates(n: Number) -> list[float]:
    vals = [n.value]
    if n.percent:
        vals.append(n.value / 100)
    else:
        vals.append(n.value * 100)  # the source may state a fraction as a percentage or vice versa
    return vals


def _close(a: float, b: float) -> bool:
    return abs(a - b) <= 1e-9 * max(1.0, abs(a), abs(b))


def _rounds_to(source: float, shown: float, decimals: int, unit: float = 1.0) -> bool:
    """True when ``source`` rounds to ``shown`` at the precision and unit the run displays."""
    return _close(round(source / unit, decimals), shown / unit) and not _close(source, shown)


def match(run: Number, source_numbers: list[Number]) -> str:
    """``green`` (equal after normalization), ``amber`` (equal after rounding or rescaling), or ``red``."""
    targets = [abs(v) for v in _candidates(run)]
    sign_free = [(abs(s.value), s) for s in source_numbers]
    for sv, s in sign_free:
        if _close(sv, targets[0]) and s.percent == run.percent:
            return "green"
    for sv, s in sign_free:
        if any(_close(sv, t) for t in targets):
            return "green" if s.percent == run.percent or run.percent else "amber"
    scales = (1.0, 1e3, 1e6, 1e9, 1e-3, 1e-6, 1e-9, 100.0, 0.01)
    for sv, _ in sign_free:
        for k in scales:
            scaled = sv * k
            for t in targets:
                if _close(scaled, t) and k != 1.0:
                    return "amber"
                if _rounds_to(scaled, t, run.decimals, run.unit):
                    return "amber"
    return "red"


def check_run(run_text: str, source_texts: list[str]) -> list[tuple[Number, str]]:
    """Verdict per number in a cited run against the concatenated cited passages."""
    source_numbers = [n for text in source_texts for n in extract_numbers(text)]
    return [(n, match(n, source_numbers)) for n in extract_numbers(run_text)]
