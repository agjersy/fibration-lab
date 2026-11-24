"""Shared AST helpers. The kernel never imports user code: everything here is static."""

from __future__ import annotations

import ast
import glob
import os
from dataclasses import dataclass
from pathlib import Path

from .config import ProjectConfig
from .errors import PATH_OUTSIDE_ROOT, RpcError

# Directories that never hold project sources. ``out_dir`` is added per call.
SKIP_DIRS = frozenset({".venv", ".git", "__pycache__", "node_modules", ".analyst"})


@dataclass
class FuncDef:
    node: ast.FunctionDef | ast.AsyncFunctionDef
    qualname: str
    path: Path  # absolute
    rel: str  # relative to root, forward slashes
    is_method: bool

    @property
    def name(self) -> str:
        return self.node.name

    @property
    def line(self) -> int:
        return self.node.lineno

    @property
    def is_public(self) -> bool:
        return not self.node.name.startswith("_")


def parse_file(path: Path) -> ast.Module | None:
    try:
        source = path.read_text(encoding="utf-8", errors="replace")
        return ast.parse(source, filename=str(path))
    except (SyntaxError, ValueError, OSError, RecursionError, MemoryError):
        return None


def read_source(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def iter_functions(tree: ast.Module, path: Path, root: Path) -> list[FuncDef]:
    """Top-level functions and methods of top-level classes (qualname ``Class.method``)."""
    rel = _rel(root, path)
    out: list[FuncDef] = []
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            out.append(FuncDef(node, node.name, path, rel, False))
        elif isinstance(node, ast.ClassDef):
            for sub in node.body:
                if isinstance(sub, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    out.append(FuncDef(sub, f"{node.name}.{sub.name}", path, rel, True))
    return out


def _rel(root: Path, path: Path) -> str:
    try:
        return path.resolve().relative_to(root.resolve()).as_posix()
    except ValueError:
        return path.as_posix()


def import_aliases(tree: ast.Module) -> dict[str, str]:
    """Map local names to the top-level module they come from.

    ``import numpy as np`` -> ``{"np": "numpy"}``; ``from numpy import exp`` -> ``{"exp": "numpy"}``;
    ``import scipy.optimize`` -> ``{"scipy": "scipy"}``.
    """
    out: dict[str, str] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                top = alias.name.split(".")[0]
                out[alias.asname or top] = top
        elif isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
            top = node.module.split(".")[0]
            for alias in node.names:
                out[alias.asname or alias.name] = top
    return out


def _skipped(rel_parts: tuple[str, ...], out_dir: str | None) -> bool:
    if any(part in SKIP_DIRS for part in rel_parts[:-1]):
        return True
    if out_dir:
        out_parts = Path(out_dir).parts
        return rel_parts[: len(out_parts)] == out_parts
    return False


def in_skipped_dir(root: Path, directory: Path, out_dir: str | None = None) -> bool:
    """True when ``directory`` is, or lies inside, ``.venv``, ``.git`` or the output directory."""
    try:
        rel = directory.resolve().relative_to(root.resolve())
    except ValueError:
        return True
    return _skipped((*rel.parts, ""), out_dir)


def check_glob(pattern: str) -> None:
    """Error 1002 for a glob that can match outside the repo root."""
    parts = Path(pattern).parts
    if not pattern or Path(pattern).is_absolute() or pattern.startswith("~") or ".." in parts:
        raise RpcError(PATH_OUTSIDE_ROOT, f"glob {pattern!r} is outside the repo root")


def script_files(root: Path, globs: list[str], out_dir: str | None = None) -> list[Path]:
    """Python files matching ``globs`` under root, never from ``.venv``, ``.git`` or ``out_dir``.

    Raises error 1002 for a glob that is absolute or contains ``..``; matches that resolve
    outside the root through a symlink are skipped.
    """
    root_r = root.resolve()
    seen: dict[str, Path] = {}
    for pattern in globs:
        check_glob(pattern)
        for match in sorted(glob.glob(os.path.join(str(root), pattern), recursive=True)):
            p = Path(match)
            if not (p.is_file() and p.suffix == ".py"):
                continue
            real = p.resolve()
            try:
                rel = real.relative_to(root_r)
            except ValueError:
                continue
            if _skipped(rel.parts, out_dir):
                continue
            seen.setdefault(str(real), p)
    return [seen[k] for k in sorted(seen)]


def python_files_under(root: Path, directory: Path, out_dir: str | None = None) -> list[Path]:
    """Every ``.py`` file below ``directory``, pruning ``.venv``, ``.git``, ``out_dir`` and symlinked dirs."""
    root_r = root.resolve()
    out: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(directory):
        here = Path(dirpath)
        keep = []
        for d in sorted(dirnames):
            child = here / d
            if d in SKIP_DIRS or child.is_symlink():
                continue
            try:
                rel = child.resolve().relative_to(root_r)
            except ValueError:
                continue
            if out_dir and rel.parts == Path(out_dir).parts:
                continue
            keep.append(d)
        dirnames[:] = keep
        for name in sorted(filenames):
            if name.endswith(".py") and (here / name).is_file():
                out.append(here / name)
    return sorted(out)


def library_files(root: Path, package: str, out_dir: str | None = None) -> list[Path]:
    pkg_dir = root / package.replace(".", "/")
    if not pkg_dir.is_dir():
        return []
    return python_files_under(root, pkg_dir, out_dir)


def classify_path(root: Path, path: Path, config: ProjectConfig) -> str:
    """'library' when under the library package, else 'script'."""
    lib_dir = (root / config.library.replace(".", "/")).resolve()
    rp = path.resolve()
    if rp == lib_dir or lib_dir in rp.parents:
        return "library"
    return "script"


def module_name(root: Path, path: Path) -> str:
    rel = Path(_rel(root, path))
    parts = list(rel.with_suffix("").parts)
    if parts and parts[-1] == "__init__":
        parts = parts[:-1]
    return ".".join(parts)


def is_main_guard(node: ast.stmt) -> bool:
    if not isinstance(node, ast.If):
        return False
    test = node.test
    if not isinstance(test, ast.Compare) or len(test.ops) != 1 or not isinstance(test.ops[0], ast.Eq):
        return False
    left, right = test.left, test.comparators[0]

    def is_name(n: ast.AST) -> bool:
        return isinstance(n, ast.Name) and n.id == "__name__"

    def is_main(n: ast.AST) -> bool:
        return isinstance(n, ast.Constant) and n.value == "__main__"

    return (is_name(left) and is_main(right)) or (is_name(right) and is_main(left))


def callee_name(call: ast.Call) -> str | None:
    """Bare function name of a call: ``f()`` -> ``f``, ``a.b.f()`` -> ``f``."""
    func = call.func
    if isinstance(func, ast.Name):
        return func.id
    if isinstance(func, ast.Attribute):
        return func.attr
    return None


def dotted(node: ast.AST) -> str | None:
    """``np.random.default_rng`` -> that string; None for anything that is not a name chain."""
    parts: list[str] = []
    cur = node
    while isinstance(cur, ast.Attribute):
        parts.append(cur.attr)
        cur = cur.value
    if isinstance(cur, ast.Name):
        parts.append(cur.id)
        return ".".join(reversed(parts))
    return None


def literal_or_none(node: ast.AST | None):
    if node is None:
        return None
    try:
        return ast.literal_eval(node)
    except (ValueError, TypeError, SyntaxError, MemoryError, RecursionError):
        return None
