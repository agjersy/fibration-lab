"""RPC methods for references, embeddings, and Personal Analyst imports (PROTOCOL 1.3)."""

from __future__ import annotations

from typing import Any

from ..errors import invalid_params
from ..methods import group, hello_provider


def _str(params: dict[str, Any], key: str, required: bool = True, default: str | None = None) -> Any:
    value = params.get(key)
    if value is None and not required:
        return default
    if not isinstance(value, str) or (required and not value):
        raise invalid_params(f"{key} is required and must be a string")
    return value


def _int(params: dict[str, Any], key: str, default: int | None = None, required: bool = False) -> int | None:
    value = params.get(key)
    if value is None and not required:
        return default
    if isinstance(value, bool) or not isinstance(value, int):
        raise invalid_params(f"{key} must be an integer")
    return value


@hello_provider
def _hello(kernel) -> dict[str, Any]:
    from .embed import embedding_info

    return {"embedding": embedding_info(kernel.home)}


@group
def register(kernel) -> None:
    from .reader import ProjectRefs

    refs = ProjectRefs(kernel.project)
    kernel.refs = refs
    d = kernel.dispatcher

    def refs_ingest(params: dict[str, Any]) -> dict[str, Any]:
        from . import ingest

        doc = ingest.ingest(kernel.project, _str(params, "path"), _str(params, "ref_id", required=False),
                            _str(params, "name", required=False, default="") or "")
        refs.forget(doc["ref_id"])
        return ingest.public_result(doc)

    def _target(params: dict[str, Any]) -> tuple[str, str, str | None]:
        """``{ref_id, loc, version?}`` or, as the agent tools pass it, ``{cite}``."""
        from .locs import parse_cite

        if params.get("ref_id") is None and isinstance(params.get("cite"), str):
            cite = parse_cite(params["cite"])
            return cite.ref_id, cite.loc, cite.version
        return _str(params, "ref_id"), _str(params, "loc", required=False, default="") or "", _str(params, "version", required=False)

    def refs_read(params: dict[str, Any]) -> dict[str, Any]:
        ref_id, loc, version = _target(params)
        max_chars = _int(params, "max_chars")
        if max_chars is not None and max_chars < 0:
            raise invalid_params("max_chars must not be negative")
        return refs.read(ref_id, loc, max_chars, version)

    def refs_table(params: dict[str, Any]) -> dict[str, Any]:
        ref_id, loc, version = _target(params)
        return refs.table_at(ref_id, loc, version)

    def refs_page_image(params: dict[str, Any]) -> dict[str, Any]:
        return refs.page_image(_str(params, "ref_id"), _int(params, "page", required=True), _int(params, "dpi"),
                               _str(params, "version", required=False))  # fmt: skip

    def embed_texts(params: dict[str, Any]) -> dict[str, Any]:
        from .embed import embed_texts as run

        return run(kernel.home, params.get("texts"), _str(params, "kind", required=False, default="passage"))

    def analyst_scan(params: dict[str, Any]) -> dict[str, Any]:
        from . import analyst

        return analyst.scan(_str(params, "repo_abs"))

    def analyst_import(params: dict[str, Any]) -> dict[str, Any]:
        from . import analyst

        return analyst.import_item(kernel.project, _str(params, "repo_abs"), _str(params, "item"), _int(params, "max_rows"))

    d.register("refs.ingest", refs_ingest)
    d.register("refs.read", refs_read)
    d.register("refs.table", refs_table)
    d.register("refs.page_image", refs_page_image)
    d.register("embed.texts", embed_texts)
    d.register("analyst.scan", analyst_scan)
    d.register("analyst.import", analyst_import)
