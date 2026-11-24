"""Normalized-AST fingerprints and token-stream similarity (PROTOCOL.md section 3)."""

from __future__ import annotations

import ast
import copy
import difflib
import hashlib
import io
import keyword
import threading
import tokenize
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from .astutil import FuncDef, iter_functions, library_files, parse_file, read_source, script_files
from .config import ProjectConfig


@dataclass
class FunctionRecord:
    path: str
    qualname: str
    name: str
    line: int
    kind: str  # "script" | "library"
    fingerprint: str
    n_statements: int
    tokens: list[str] = field(default_factory=list)
    signature: str = ""
    top_level: bool = True
    counts: Counter = field(default_factory=Counter, repr=False, compare=False)
    token_key: str = field(default="", repr=False, compare=False)

    def loc(self) -> dict[str, Any]:
        return {"path": self.path, "qualname": self.qualname, "line": self.line}


# ---- fingerprint ----------------------------------------------------------------


class _Normalizer(ast.NodeTransformer):
    def __init__(self, arg_positions: dict[str, int]) -> None:
        self.arg_positions = arg_positions

    def visit_Name(self, node: ast.Name) -> ast.AST:
        pos = self.arg_positions.get(node.id)
        node.id = f"A{pos}" if pos is not None else "N"
        return node

    def visit_Constant(self, node: ast.Constant) -> ast.AST:
        node.value = f"C:{type(node.value).__name__}"
        return node

    def visit_arg(self, node: ast.arg) -> ast.AST:
        pos = self.arg_positions.get(node.arg)
        node.arg = f"A{pos}" if pos is not None else "N"
        node.annotation = None
        return node


def _strip_docstring(body: list[ast.stmt]) -> list[ast.stmt]:
    if body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant) and isinstance(body[0].value.value, str):
        return body[1:]
    return body


def fingerprint(node: ast.FunctionDef | ast.AsyncFunctionDef) -> str:
    args = node.args
    names = [a.arg for a in args.posonlyargs + args.args + args.kwonlyargs]
    if args.vararg:
        names.append(args.vararg.arg)
    if args.kwarg:
        names.append(args.kwarg.arg)
    positions = {name: i for i, name in enumerate(names)}
    body = [copy.deepcopy(s) for s in _strip_docstring(node.body)]
    module = ast.Module(body=body, type_ignores=[])
    normalized = _Normalizer(positions).visit(module)
    text = ast.dump(normalized, include_attributes=False)
    return hashlib.sha1(text.encode("utf-8")).hexdigest()


def statement_count(node: ast.FunctionDef | ast.AsyncFunctionDef) -> int:
    body = _strip_docstring(node.body)
    return sum(1 for s in body for _ in _walk_stmts(s))


def _walk_stmts(stmt: ast.stmt):
    yield stmt
    for child in ast.iter_child_nodes(stmt):
        if isinstance(child, ast.stmt):
            yield from _walk_stmts(child)
        elif isinstance(child, list):  # pragma: no cover - iter_child_nodes flattens lists
            pass
    for name in ("body", "orelse", "finalbody", "handlers"):
        for child in getattr(stmt, name, []) or []:
            if isinstance(child, ast.ExceptHandler):
                for sub in child.body:
                    yield from _walk_stmts(sub)


# ---- token stream ----------------------------------------------------------------

_DROP = {
    tokenize.COMMENT,
    tokenize.NL,
    tokenize.INDENT,
    tokenize.DEDENT,
    tokenize.ENCODING,
    tokenize.ENDMARKER,
}
_STRING_KINDS = {tokenize.STRING} | {
    getattr(tokenize, n) for n in ("FSTRING_START", "FSTRING_MIDDLE", "FSTRING_END") if hasattr(tokenize, n)
}


def token_stream(source: str) -> list[str]:
    out: list[str] = []
    prev_op_dot = False
    try:
        toks = list(tokenize.generate_tokens(io.StringIO(source).readline))
    except (tokenize.TokenError, SyntaxError, IndentationError):
        return []
    for tok in toks:
        kind = tok.type
        if kind in _DROP:
            continue
        if kind == tokenize.NAME:
            if keyword.iskeyword(tok.string) or prev_op_dot:
                out.append(tok.string)
            else:
                out.append("N")
        elif kind == tokenize.NUMBER:
            out.append("0")
        elif kind in _STRING_KINDS:
            out.append("S")
        elif kind == tokenize.NEWLINE:
            out.append("\n")
        else:
            out.append(tok.string)
        prev_op_dot = kind == tokenize.OP and tok.string == "."
    return out


def similarity(a: list[str], b: list[str]) -> float:
    if not a or not b:
        return 0.0
    sm = difflib.SequenceMatcher(None, a, b, autojunk=False)
    return sm.ratio()


# ---- collection -----------------------------------------------------------------


def function_source(source: str, node: ast.AST) -> str:
    seg = ast.get_source_segment(source, node)
    if seg is None:
        return ""
    # dedent so nested definitions tokenize cleanly
    lines = seg.splitlines()
    if not lines:
        return ""
    import textwrap

    return textwrap.dedent(seg)


def record_for(f: FuncDef, source: str, kind: str) -> FunctionRecord:
    tokens = token_stream(function_source(source, f.node))
    return FunctionRecord(
        top_level=not f.is_method,
        counts=Counter(tokens),
        token_key=hashlib.sha1("\x00".join(tokens).encode("utf-8")).hexdigest(),
        path=f.rel,
        qualname=f.qualname,
        name=f.name,
        line=f.line,
        kind=kind,
        fingerprint=fingerprint(f.node),
        n_statements=statement_count(f.node),
        tokens=tokens,
        signature=ast.unparse(f.node.args),
    )


# Per-file records keyed by (path, kind) and validated by (mtime_ns, size): tokenizing and
# fingerprinting every function on every check is most of the cost on a large repo.
_RECORD_CACHE: dict[tuple[str, str], tuple[tuple[int, int], list[FunctionRecord]]] = {}
# Scores of token-stream pairs already compared, keyed by the two stream digests.
_SCORE_CACHE: dict[tuple[str, str], float] = {}
_SCORE_CACHE_MAX = 200_000
_CACHE_LOCK = threading.Lock()


def records_for_file(root: Path, path: Path, kind: str) -> list[FunctionRecord]:
    try:
        st = path.stat()
    except OSError:
        return []
    key = (str(path.resolve()), kind)
    stamp = (st.st_mtime_ns, st.st_size)
    with _CACHE_LOCK:
        hit = _RECORD_CACHE.get(key)
    if hit is not None and hit[0] == stamp:
        return hit[1]
    tree = parse_file(path)
    if tree is None:
        records: list[FunctionRecord] = []
    else:
        source = read_source(path)
        records = [record_for(f, source, kind) for f in iter_functions(tree, path, root)]
    with _CACHE_LOCK:
        if len(_RECORD_CACHE) > 5000:
            _RECORD_CACHE.clear()
        _RECORD_CACHE[key] = (stamp, records)
    return records


def collect_functions(root: Path, config: ProjectConfig, files: list[tuple[Path, str]] | None = None) -> list[FunctionRecord]:
    """All functions in scripts and the library (or in ``files``: list of (path, kind))."""
    if files is None:
        files = [(p, "script") for p in script_files(root, config.scripts, config.out_dir)]
        files += [(p, "library") for p in library_files(root, config.library, config.out_dir)]
    records: list[FunctionRecord] = []
    seen: set[str] = set()
    for path, kind in files:
        key = str(path.resolve())
        if key in seen:
            continue
        seen.add(key)
        records.extend(records_for_file(root, path, kind))
    return records


def _exempt(r: FunctionRecord, allowed: set[str]) -> bool:
    """Contract-mandated script functions (``main``, ``build_parser`` ...) look alike by design."""
    return r.kind == "script" and r.top_level and r.name in allowed


def _pair_score(ra: FunctionRecord, rb: FunctionRecord, threshold: float, matcher: difflib.SequenceMatcher) -> float | None:
    """Similarity of two token streams when it can reach ``threshold``, else None.

    Cheap upper bounds first: the length ratio bounds ``ratio()`` by ``2*min/(la+lb)`` and
    the multiset overlap bounds it by ``2*overlap/(la+lb)``. Only survivors pay for the
    quadratic matcher, and each distinct pair of streams is scored once per process.
    """
    la, lb = len(ra.tokens), len(rb.tokens)
    total = la + lb
    if 2.0 * min(la, lb) / total < threshold:
        return None
    if ra.token_key == rb.token_key:
        return 1.0
    overlap = sum((ra.counts & rb.counts).values())
    if 2.0 * overlap / total < threshold:
        return None
    key = (ra.token_key, rb.token_key) if ra.token_key <= rb.token_key else (rb.token_key, ra.token_key)
    with _CACHE_LOCK:
        cached = _SCORE_CACHE.get(key)
    if cached is not None:
        return cached
    matcher.set_seq1(ra.tokens)
    score = matcher.ratio()
    with _CACHE_LOCK:
        if len(_SCORE_CACHE) >= _SCORE_CACHE_MAX:
            _SCORE_CACHE.clear()
        _SCORE_CACHE[key] = score
    return score


def find_pairs(
    records: list[FunctionRecord],
    threshold: float,
    min_tokens: int,
    listed: set[str] | None = None,
    allowed: set[str] | None = None,
) -> tuple[list[tuple[FunctionRecord, FunctionRecord]], list[tuple[FunctionRecord, FunctionRecord, float]]]:
    """Exact duplicates (>= 3 statements) and near-duplicates at or above ``threshold``.

    ``listed`` restricts the result to pairs with at least one side in those paths (the
    other side may be anywhere in ``records``). ``allowed`` names the script functions the
    contract requires; they are compared with nothing.
    """
    allowed = allowed or set()

    def wanted(ra: FunctionRecord, rb: FunctionRecord) -> bool:
        return listed is None or ra.path in listed or rb.path in listed

    usable = [i for i, r in enumerate(records) if not _exempt(r, allowed)]
    duplicates: list[tuple[FunctionRecord, FunctionRecord]] = []
    by_fp: dict[str, list[int]] = {}
    for i in usable:
        if records[i].n_statements >= 3:
            by_fp.setdefault(records[i].fingerprint, []).append(i)
    for idxs in by_fp.values():
        for a in range(len(idxs)):
            for b in range(a + 1, len(idxs)):
                ra, rb = records[idxs[a]], records[idxs[b]]
                if wanted(ra, rb):
                    duplicates.append((ra, rb))
    scored: list[tuple[int, int, float]] = []
    eligible = [i for i in usable if len(records[i].tokens) >= min_tokens]
    matcher = difflib.SequenceMatcher(None, autojunk=False)
    for y in range(1, len(eligible)):
        rb = records[eligible[y]]
        seq2_set = False
        for x in range(y):
            ra = records[eligible[x]]
            if ra.fingerprint == rb.fingerprint or not wanted(ra, rb):
                continue
            if not seq2_set:
                matcher.set_seq2(rb.tokens)  # the matcher indexes seq2 once for all partners
                seq2_set = True
            score = _pair_score(ra, rb, threshold, matcher)
            if score is not None and score >= threshold:
                scored.append((eligible[x], eligible[y], score))
    scored.sort()
    near = [(records[i], records[j], score) for i, j, score in scored]
    return duplicates, near


def find_twins(records: list[FunctionRecord]) -> list[tuple[FunctionRecord, FunctionRecord]]:
    """Script functions whose name equals a library function's name."""
    lib_by_name: dict[str, list[FunctionRecord]] = {}
    for r in records:
        if r.kind == "library":
            lib_by_name.setdefault(r.name, []).append(r)
    twins: list[tuple[FunctionRecord, FunctionRecord]] = []
    for r in records:
        if r.kind == "script":
            for lib in lib_by_name.get(r.name, []):
                twins.append((r, lib))
    return twins


def similar_pairs(root: Path, config: ProjectConfig, threshold: float | None = None) -> list[dict[str, Any]]:
    thr = float(threshold) if threshold is not None else config.near_duplicate_threshold
    records = collect_functions(root, config)
    duplicates, near = find_pairs(
        records, thr, config.min_tokens_for_similarity, allowed=set(config.script_allowed_functions)
    )
    pairs: list[dict[str, Any]] = []
    for a, b in duplicates:
        pairs.append({"kind": "duplicate", "score": 1.0, "a": a.loc(), "b": b.loc()})
    for a, b, score in near:
        pairs.append({"kind": "near-duplicate", "score": round(score, 4), "a": a.loc(), "b": b.loc()})
    for a, b in find_twins(records):
        pairs.append(
            {"kind": "unused-twin", "score": round(similarity(a.tokens, b.tokens), 4), "a": a.loc(), "b": b.loc()}
        )
    return pairs
