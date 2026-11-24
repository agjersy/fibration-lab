"""``out.opens-clean``: schema validation when the ECMA-376 XSDs are present, and a fidelity conversion."""

from __future__ import annotations

import functools
import zipfile
from pathlib import Path

from lxml import etree


def schema_dir() -> Path | None:
    here = Path(__file__).resolve()
    for cand in (here.parents[1] / "ooxml-schemas", here.parents[4] / "vendor" / "ooxml-schemas"):
        if (cand / "pml.xsd").is_file():
            return cand
    return None


@functools.lru_cache(maxsize=1)
def _pml_schema(path: str):
    return etree.XMLSchema(etree.parse(str(Path(path) / "pml.xsd")))


def validate_parts(pptx: Path, limit: int = 20) -> list[str]:
    """Validate presentation, slide, layout, and master parts against the transitional schema."""
    directory = schema_dir()
    if directory is None:
        return []
    schema = _pml_schema(str(directory))
    problems: list[str] = []
    with zipfile.ZipFile(pptx) as z:
        for name in sorted(z.namelist()):
            if not (name == "ppt/presentation.xml" or (name.startswith(("ppt/slides/", "ppt/slideLayouts/", "ppt/slideMasters/"))
                                                       and name.endswith(".xml") and "/_rels/" not in name)):  # fmt: skip
                continue
            doc = etree.fromstring(z.read(name))
            if not schema.validate(doc):
                for err in list(schema.error_log)[:3]:
                    problems.append(f"{name}: line {err.line}: {err.message}")
                if len(problems) >= limit:
                    break
    return problems
