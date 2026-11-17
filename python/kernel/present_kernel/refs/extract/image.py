"""Images: no text; one ImageRef with loc ``full``."""

from __future__ import annotations

from pathlib import Path
from typing import Any

from ...errors import UNSUPPORTED_REFERENCE, RpcError
from ..model import Ctx, new_doc


def extract(path: Path, ctx: Ctx) -> dict[str, Any]:
    from PIL import Image

    try:
        with Image.open(path) as image:
            image.load()
            width, height = image.size
            ref = ctx.save_image("full", image.copy(), "full")
    except Exception as exc:
        raise RpcError(UNSUPPORTED_REFERENCE, f"{ctx.label}: not a readable image ({exc})") from exc
    doc = new_doc("image", ctx.name or f"Image {width}×{height}")
    doc["root_loc"] = "full"
    doc["size"] = [width, height]
    if ref:
        doc["images"].append(ref)
    return doc
