"""Citation strings and location grammar (PROTOCOL 2.3).

``ref:<ref_id>@<version>#<loc>``. The parser is tolerant on input (the prefix, the version, and
the loc may each be missing); everything the kernel emits follows the grammar exactly.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from ..errors import invalid_params

REF_ID_RE = re.compile(r"^r-[0-9a-z]{4,12}$")
_CITE_RE = re.compile(r"^\s*(?:ref:)?(?P<id>r-[0-9a-z]{4,12})(?:@(?P<version>[0-9a-fA-F]{6,64}))?(?:#(?P<loc>.*))?$", re.S)
_TABLE_SUFFIX = re.compile(r"^(?P<base>.*?)/?t(?P<n>\d+)$", re.S)
_CHARS = re.compile(r"^c(\d+)-(\d+)$")
_PDF = re.compile(r"^p(\d+)(?::c(\d+)-(\d+))?$")
_PPTX = re.compile(r"^s(\d+)(?:/sh(\d+))?$")
_CSV = re.compile(r"^r(\d+)-(\d+)(?::c(\d+)-(\d+))?$")
_ATT = re.compile(r"^att(\d+)(?:/(.*))?$", re.S)
_A1 = re.compile(r"^\$?([A-Za-z]{1,3})\$?(\d+)(?::\$?([A-Za-z]{1,3})\$?(\d+))?$")


@dataclass(frozen=True)
class Cite:
    ref_id: str
    version: str | None
    loc: str

    def __str__(self) -> str:
        return make_cite(self.ref_id, self.version, self.loc)


def make_cite(ref_id: str, version: str | None, loc: str = "") -> str:
    out = f"ref:{ref_id}"
    if version:
        out += f"@{version}"
    if loc:
        out += f"#{loc}"
    return out


def parse_cite(cite: str) -> Cite:
    m = _CITE_RE.match((cite or "").strip()) if isinstance(cite, str) else None
    if not m:
        raise invalid_params(f"not a citation: {cite!r} (expected ref:<ref_id>@<version>#<loc>)")
    version = m.group("version")
    return Cite(m.group("id"), version.lower()[:12] if version else None, (m.group("loc") or "").strip())


def split_attachment(loc: str) -> tuple[int, str] | None:
    """``att2/r1-3`` -> ``(2, "r1-3")``; None when the loc does not address an attachment."""
    m = _ATT.match(loc)
    return (int(m.group(1)), m.group(2) or "") if m else None


def split_table(loc: str) -> tuple[str, int] | None:
    """``p12/t1`` -> ``("p12", 1)``; a bare ``t1`` has an empty base."""
    m = _TABLE_SUFFIX.match(loc)
    return (m.group("base"), int(m.group("n"))) if m else None


def parse_chars(loc: str) -> tuple[int, int] | None:
    m = _CHARS.match(loc)
    return (int(m.group(1)), int(m.group(2))) if m else None


def parse_pdf(loc: str) -> tuple[int, int | None, int | None] | None:
    m = _PDF.match(loc)
    if not m:
        return None
    return int(m.group(1)), (int(m.group(2)) if m.group(2) else None), (int(m.group(3)) if m.group(3) else None)


def parse_pptx(loc: str) -> tuple[int, int | None] | None:
    m = _PPTX.match(loc)
    return (int(m.group(1)), int(m.group(2)) if m.group(2) is not None else None) if m else None


def parse_csv(loc: str) -> tuple[int, int, int | None, int | None] | None:
    m = _CSV.match(loc)
    if not m:
        return None
    return int(m.group(1)), int(m.group(2)), (int(m.group(3)) if m.group(3) else None), (int(m.group(4)) if m.group(4) else None)


def col_letter(n: int) -> str:
    """1 -> A, 27 -> AA."""
    out = ""
    while n > 0:
        n, rem = divmod(n - 1, 26)
        out = chr(65 + rem) + out
    return out


def col_number(letters: str) -> int:
    n = 0
    for ch in letters.upper():
        n = n * 26 + (ord(ch) - 64)
    return n


def a1_range(r1: int, c1: int, r2: int, c2: int) -> str:
    return f"{col_letter(c1)}{r1}:{col_letter(c2)}{r2}"


def parse_a1(text: str) -> tuple[int, int, int, int] | None:
    """``B2:F14`` (or one cell) -> (row1, col1, row2, col2), 1-based and normalized."""
    m = _A1.match(text.strip())
    if not m:
        return None
    c1, r1 = col_number(m.group(1)), int(m.group(2))
    c2, r2 = (col_number(m.group(3)), int(m.group(4))) if m.group(3) else (c1, r1)
    return min(r1, r2), min(c1, c2), max(r1, r2), max(c1, c2)


def parse_sheet_range(loc: str) -> tuple[str, tuple[int, int, int, int] | None] | None:
    """``Q3!B2:F14`` -> ("Q3", (2, 2, 14, 6)); ``Q3`` alone is not a sheet range."""
    if "!" not in loc:
        return None
    sheet, _, rng = loc.rpartition("!")
    if len(sheet) >= 2 and sheet[0] == sheet[-1] == "'":
        sheet = sheet[1:-1].replace("''", "'")
    cells = parse_a1(rng)
    return (sheet, cells) if cells else None
