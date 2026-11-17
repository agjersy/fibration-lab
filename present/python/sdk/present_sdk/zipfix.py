"""Deterministic packaging: fixed timestamps and sorted members, recursively for embedded workbooks."""

from __future__ import annotations

import io
import re
import zipfile

_FIXED = (2000, 1, 1, 0, 0, 0)
_DCTERMS = re.compile(rb"(<dcterms:(created|modified)[^>]*>)[^<]*(</dcterms:\2>)")


def normalize_zip(data: bytes, stamp: str = "2000-01-01T00:00:00Z") -> bytes:
    """Rewrite an OPC zip so equal content gives equal bytes."""
    src = zipfile.ZipFile(io.BytesIO(data))
    out = io.BytesIO()
    names = src.namelist()
    # [Content_Types].xml first (OPC convention), then the rest sorted.
    names = sorted(names, key=lambda n: (n != "[Content_Types].xml", n))
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=6) as dst:
        for name in names:
            blob = src.read(name)
            if name.lower().endswith(".xlsx"):
                blob = normalize_zip(blob, stamp)
            elif name == "docProps/core.xml":
                blob = _DCTERMS.sub(lambda m: m.group(1) + stamp.encode() + m.group(3), blob)
            info = zipfile.ZipInfo(name, date_time=_FIXED)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o600 << 16
            info.create_system = 0
            dst.writestr(info, blob)
    return out.getvalue()
