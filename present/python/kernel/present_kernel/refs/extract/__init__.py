"""One extractor per reference kind; ``EXTRACTORS`` maps a RefKind to its ``extract(path, ctx)``."""

from __future__ import annotations

from importlib import import_module
from pathlib import Path
from typing import Any

from ..model import Ctx

# extension -> (RefKind, module, function)
BY_EXTENSION: dict[str, tuple[str, str, str]] = {
    ".pdf": ("pdf", "pdf", "extract"),
    ".docx": ("docx", "docx", "extract"),
    ".docm": ("docx", "docx", "extract"),
    ".pptx": ("pptx", "pptx", "extract"),
    ".pptm": ("pptx", "pptx", "extract"),
    ".xlsx": ("xlsx", "xlsx", "extract"),
    ".xlsm": ("xlsx", "xlsx", "extract"),
    ".csv": ("csv", "csvx", "extract"),
    ".tsv": ("csv", "csvx", "extract"),
    ".md": ("markdown", "textmd", "extract_markdown"),
    ".markdown": ("markdown", "textmd", "extract_markdown"),
    ".txt": ("text", "textmd", "extract_text"),
    ".text": ("text", "textmd", "extract_text"),
    ".log": ("text", "textmd", "extract_text"),
    ".png": ("image", "image", "extract"),
    ".jpg": ("image", "image", "extract"),
    ".jpeg": ("image", "image", "extract"),
    ".gif": ("image", "image", "extract"),
    ".bmp": ("image", "image", "extract"),
    ".tif": ("image", "image", "extract"),
    ".tiff": ("image", "image", "extract"),
    ".webp": ("image", "image", "extract"),
    ".eml": ("eml", "eml", "extract"),
    ".msg": ("msg", "msg", "extract"),
    ".html": ("html", "html", "extract"),
    ".htm": ("html", "html", "extract"),
    ".mhtml": ("html", "html", "extract_mhtml"),
    ".mht": ("html", "html", "extract_mhtml"),
}


def kind_for(name: str) -> str | None:
    entry = BY_EXTENSION.get(Path(name).suffix.lower())
    return entry[0] if entry else None


def run_extractor(path: Path, ctx: Ctx) -> dict[str, Any]:
    _, module, func = BY_EXTENSION[path.suffix.lower()]
    return getattr(import_module(f"{__name__}.{module}"), func)(path, ctx)
