"""``contract.check``: every rule of PROTOCOL.md section 3 by static analysis."""

from __future__ import annotations

import ast
import builtins
import fnmatch
import re
from pathlib import Path
from typing import Any

from .astutil import (
    callee_name,
    classify_path,
    dotted,
    import_aliases,
    in_skipped_dir,
    is_main_guard,
    iter_functions,
    library_files,
    module_name,
    parse_file,
    python_files_under,
    script_files,
)
from .config import ProjectConfig
from .docstrings import function_docstring, has_math
from .index_scripts import index_script, is_add_argument_call, is_argparser_call, kw
from .paths import resolve_in_root
from .similarity import collect_functions, find_pairs, find_twins

DEFAULT_SEVERITY = {
    "cli.parser-required": "error",
    "cli.help-text": "error",
    "cli.main-function": "error",
    "cli.main-guard": "error",
    "cli.option-for-every-input": "error",
    "cli.no-absolute-paths": "error",
    "lib.script-defines-logic": "error",
    "lib.duplicate": "error",
    "lib.near-duplicate": "warn",
    "lib.unused-twin": "warn",
    "out.layout": "warn",
    "seed.option": "warn",
    "math.required": "warn",
    "doc.docstring": "warn",
}
RULES = tuple(DEFAULT_SEVERITY)

ALLOWED_NUMBERS = {0, 1, -1, 2, 0.5}
DATA_EXTS = (
    ".csv", ".tsv", ".parquet", ".npy", ".npz", ".json", ".txt", ".h5", ".hdf5", ".nc",
    ".pkl", ".pickle", ".dat", ".xlsx", ".feather", ".fits", ".mat", ".zarr",
)
_ABS_RE = re.compile(r"^(/|~|[A-Za-z]:\\)")
_BUILTINS = set(dir(builtins))
_PARSER_METHODS = {
    "add_argument",
    "set_defaults",
    "add_parser",
    "add_subparsers",
    "add_argument_group",
    "add_mutually_exclusive_group",
    "ArgumentParser",
    "parse_args",
    "parse_known_args",
}
_LOG_METHODS = {"debug", "info", "warning", "warn", "error", "exception", "critical", "fatal", "log"}
_LABEL_CALLS = {
    "set_title", "set_xlabel", "set_ylabel", "set_zlabel", "set_label", "title", "suptitle",
    "xlabel", "ylabel", "zlabel", "supxlabel", "supylabel", "legend", "text", "annotate", "figtext",
    "set_xticklabels", "set_yticklabels",
}
_FILE_EXT_RE = re.compile(r"[^/.][^/]*\.[A-Za-z0-9]{1,8}$")
_SAVE_CALLEES = {"savefig", "to_csv", "to_parquet"}
_NP_SAVE = {"save", "savez", "savez_compressed"}


class Checker:
    def __init__(self, root: Path, config: ProjectConfig) -> None:
        self.root = root.resolve()
        self.config = config
        self.diags: list[dict[str, Any]] = []
        self.severity = dict(DEFAULT_SEVERITY)
        self.severity.update({k: v for k, v in config.severity.items() if k in DEFAULT_SEVERITY})

    # ---- plumbing --------------------------------------------------------------
    def add(
        self,
        rule: str,
        path: str,
        line: int,
        col: int,
        message: str,
        hint: str | None = None,
        related: list[dict[str, Any]] | None = None,
    ) -> None:
        self.diags.append(
            {
                "path": path,
                "line": int(line),
                "col": int(col),
                "rule": rule,
                "severity": self.severity[rule],
                "message": message,
                "hint": hint,
                "related": related or [],
            }
        )

    def rel(self, path: Path) -> str:
        return path.resolve().relative_to(self.root).as_posix()

    # ---- entry points -------------------------------------------------------------
    def _all_files(self) -> list[tuple[Path, str]]:
        out_dir = self.config.out_dir
        scripts = script_files(self.root, self.config.scripts, out_dir)
        libs = library_files(self.root, self.config.library, out_dir)
        return [(p, "script") for p in scripts] + [(p, "library") for p in libs]

    def check_all(self) -> list[dict[str, Any]]:
        files = self._all_files()
        return self.check_files(files, files, listed=None)

    def check_paths(self, paths: list[str]) -> list[dict[str, Any]]:
        files: list[tuple[Path, str]] = []
        for p in paths:
            full = resolve_in_root(self.root, p)
            if full.is_dir():
                # never descends into .venv, .git or the output directory
                if in_skipped_dir(self.root, full, self.config.out_dir):
                    continue
                for f in python_files_under(self.root, full, self.config.out_dir):
                    files.append((f, classify_path(self.root, f, self.config)))
            elif full.is_file() and full.suffix == ".py":
                files.append((full, classify_path(self.root, full, self.config)))
        # Cross-file rules compare the listed files against every script and the whole
        # library, not only against each other.
        universe = files + self._all_files()
        return self.check_files(files, universe, listed={self.rel(f) for f, _ in files})

    def check_files(
        self,
        files: list[tuple[Path, str]],
        universe: list[tuple[Path, str]] | None = None,
        listed: set[str] | None = None,
    ) -> list[dict[str, Any]]:
        self.diags = []
        seen: set[Path] = set()
        for path, kind in files:
            real = path.resolve()
            if real in seen:
                continue
            seen.add(real)
            tree = parse_file(path)
            if tree is None:
                continue
            if kind == "script":
                self.check_script(path, tree)
            else:
                self.check_library_module(path, tree)
        self.check_similarity(universe if universe is not None else files, listed)
        self.diags.sort(key=lambda d: (d["path"], d["line"], d["col"], d["rule"]))
        return self.diags

    # ---- script rules -------------------------------------------------------------
    def check_script(self, path: Path, tree: ast.Module) -> None:
        rel = self.rel(path)
        info = index_script(path, self.root)
        out_dir = self.config.out_dir

        if info["parser_calls"] == 0:
            self.add("cli.parser-required", rel, 1, 1, "script builds no argparse.ArgumentParser",
                     hint="build a parser in build_parser() or main() with a description and one option per input")
        for opt in info["options"]:
            if not opt["has_help"]:
                flags = ", ".join(opt["flags"]) or opt["dest"] or "argument"
                self.add("cli.help-text", rel, opt["line"], 1, f"add_argument({flags}) has no help text",
                         hint='add help="..." describing the option and its unit')
        if not info["has_main"]:
            self.add("cli.main-function", rel, 1, 1, "no top-level def main(", hint="define def main(argv=None) that parses arguments and calls the library")
        if not info["has_main_guard"]:
            self.add("cli.main-guard", rel, 1, 1, 'no if __name__ == "__main__": guard', hint='end the file with if __name__ == "__main__": main()')

        self._check_inputs(rel, tree)
        self._check_absolute_paths(rel, tree)
        self._check_script_logic(rel, tree)
        self._check_out_layout(rel, tree, out_dir)
        self._check_seed(rel, tree, info)

    def _main_regions(self, tree: ast.Module) -> list[ast.AST]:
        regions: list[ast.AST] = []
        for node in tree.body:
            if (isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name == "main") or is_main_guard(node):
                regions.append(node)
        return regions

    def _check_inputs(self, rel: str, tree: ast.Module) -> None:
        for region in self._main_regions(tree):
            self._walk_calls(rel, region)

    def _walk_calls(self, rel: str, node: ast.AST) -> None:
        for child in ast.iter_child_nodes(node):
            if isinstance(child, ast.Call):
                name = callee_name(child)
                if name in _PARSER_METHODS or is_argparser_call(child):
                    continue  # add_argument(default=...) and friends: literals belong there
                if not _exempt_call(child, name):
                    self._check_call_literals(rel, child, name or "<call>")
            self._walk_calls(rel, child)

    def _check_call_literals(self, rel: str, call: ast.Call, name: str) -> None:
        # Positional arguments only: a keyword names what the literal is (alpha=0.4, dpi=150),
        # which is a presentation choice, not an unnamed input.
        for i, node in enumerate(call.args):
            problem = _literal_problem(node, first_positional=i == 0)
            if problem is None:
                continue
            self.add(
                "cli.option-for-every-input",
                rel,
                node.lineno,
                node.col_offset + 1,
                f"{problem} passed as argument to {name}()",
                hint="make it a command-line option with a default (add_argument(..., default=...)) and pass args.<dest>",
            )

    def _check_absolute_paths(self, rel: str, tree: ast.Module) -> None:
        skip: set[int] = set()
        for node in ast.walk(tree):
            if is_add_argument_call(node):
                h = kw(node, "help")
                if h is not None:
                    for sub in ast.walk(h):
                        skip.add(id(sub))
        skip |= {id(n.value) for n in _docstring_nodes(tree)}
        candidates: list[ast.Constant] = []
        for node in ast.walk(tree):
            if isinstance(node, ast.JoinedStr):
                # f"{out_dir}/spectrum.png": fragments after a substitution are not absolute
                # paths. Only an f-string that itself starts with one is.
                parts = [sub for sub in ast.walk(node) if isinstance(sub, ast.Constant)]
                first = node.values[0] if node.values else None
                if id(node) not in skip and isinstance(first, ast.Constant) and not any(id(c) in skip for c in parts):
                    candidates.append(first)
                skip |= {id(c) for c in parts}
        for node in ast.walk(tree):
            if isinstance(node, ast.Constant) and id(node) not in skip:
                candidates.append(node)
        for node in candidates:
            if isinstance(node.value, str) and _ABS_RE.match(node.value):
                self.add("cli.no-absolute-paths", rel, node.lineno, node.col_offset + 1,
                         f"absolute path literal {node.value!r}",
                         hint="use a repo-relative default or a configured data directory")

    def _check_script_logic(self, rel: str, tree: ast.Module) -> None:
        aliases = import_aliases(tree)
        numerical = set(self.config.numerical_modules)
        allowed = set(self.config.script_allowed_functions)
        stem = Path(rel).stem
        for node in tree.body:
            if not isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                continue
            if node.name in allowed:
                continue
            length = (node.end_lineno or node.lineno) - node.lineno + 1
            refs = sorted(_numerical_refs(node, aliases, numerical))
            private = node.name.startswith("_")
            if private and length <= self.config.max_helper_lines and not refs:
                continue
            why = []
            if not private:
                why.append("is not private")
            if length > self.config.max_helper_lines:
                why.append(f"is {length} lines (max {self.config.max_helper_lines})")
            if refs:
                why.append("uses " + ", ".join(refs))
            self.add(
                "lib.script-defines-logic",
                rel,
                node.lineno,
                node.col_offset + 1,
                f"script defines {node.name}() which " + " and ".join(why),
                hint=f"move to {self.config.library}/{stem}.py and import it",
            )

    def _check_out_layout(self, rel: str, tree: ast.Module, out_dir: str) -> None:
        for node in ast.walk(tree):
            if not isinstance(node, ast.Call):
                continue
            name = callee_name(node)
            d = dotted(node.func) or ""
            is_save = name in _SAVE_CALLEES or (
                name in _NP_SAVE and d.split(".")[0] in ("np", "numpy")
            )
            is_open_w = False
            if isinstance(node.func, ast.Name) and node.func.id == "open":
                mode = node.args[1] if len(node.args) > 1 else kw(node, "mode")
                if isinstance(mode, ast.Constant) and isinstance(mode.value, str) and mode.value.startswith("w"):
                    is_open_w = True
            if not (is_save or is_open_w) or not node.args:
                continue
            first = node.args[0]
            if not (isinstance(first, ast.Constant) and isinstance(first.value, str)):
                continue
            value = first.value
            if value == out_dir or value.startswith(out_dir.rstrip("/") + "/"):
                continue
            self.add("out.layout", rel, first.lineno, first.col_offset + 1,
                     f"{name}() writes to {value!r}, outside {out_dir}/",
                     hint=f"write under {out_dir}/<script>/ (ANALYST_OUT_DIR when run by the app)")

    def _check_seed(self, rel: str, tree: ast.Module, info: dict[str, Any]) -> None:
        if any(opt["dest"] == "seed" for opt in info["options"]):
            return
        for node in ast.walk(tree):
            d = dotted(node) if isinstance(node, (ast.Attribute, ast.Name)) else None
            if d is None:
                continue
            hit = (
                d.startswith("random.")
                or d.startswith("np.random")
                or d.startswith("numpy.random")
                or d == "default_rng"
                or d.endswith(".default_rng")
            )
            if hit:
                self.add("seed.option", rel, node.lineno, node.col_offset + 1,
                         f"uses randomness ({d}) without a --seed option",
                         hint='add parser.add_argument("--seed", type=int, default=0, help="random seed")')
                return

    # ---- library rules ----------------------------------------------------------------
    def check_library_module(self, path: Path, tree: ast.Module) -> None:
        rel = self.rel(path)
        mod = module_name(self.root, path)
        requires_math = any(fnmatch.fnmatch(rel, g) or fnmatch.fnmatch(mod, g) for g in self.config.math_required)
        for f in iter_functions(tree, path, self.root):
            if not f.is_public or f.is_method:
                continue
            doc = function_docstring(f.node)
            if doc is None:
                self.add("doc.docstring", rel, f.line, f.node.col_offset + 1,
                         f"public function {f.qualname}() has no docstring",
                         hint="add a docstring with parameter units and, if it computes a formula, a Math: section")
            if requires_math and not has_math(doc):
                self.add("math.required", rel, f.line, f.node.col_offset + 1,
                         f"{f.qualname}() has no Math: docstring section",
                         hint="add a Math: section with the formula in SymPy syntax using the parameter names")
        self._check_out_layout(rel, tree, self.config.out_dir)

    # ---- cross-file rules ---------------------------------------------------------------
    def check_similarity(self, files: list[tuple[Path, str]], listed: set[str] | None = None) -> None:
        records = collect_functions(self.root, self.config, files)

        def anchored(a, b):
            # With ``paths``, report on the listed file and point at the other one.
            if listed is not None and a.path not in listed and b.path in listed:
                return b, a
            return a, b

        dups, near = find_pairs(
            records,
            self.config.near_duplicate_threshold,
            self.config.min_tokens_for_similarity,
            listed=listed,
            allowed=set(self.config.script_allowed_functions),
        )
        for a, b in dups:
            a, b = anchored(a, b)
            self.add("lib.duplicate", a.path, a.line, 1,
                     f"{a.qualname}() duplicates {b.qualname}() in {b.path}:{b.line}",
                     hint=f"keep one implementation in {self.config.library}/ and import it in both places",
                     related=[{"path": b.path, "line": b.line}])
        for a, b, score in near:
            a, b = anchored(a, b)
            self.add("lib.near-duplicate", a.path, a.line, 1,
                     f"{a.qualname}() is {score:.0%} similar to {b.qualname}() in {b.path}:{b.line}",
                     hint="promote the shared logic to the library and reuse it",
                     related=[{"path": b.path, "line": b.line}])
        for a, b in find_twins(records):
            if listed is not None and a.path not in listed and b.path not in listed:
                continue
            self.add("lib.unused-twin", a.path, a.line, 1,
                     f"script defines {a.name}() but the library already has {b.qualname}() in {b.path}:{b.line}",
                     hint=f"delete the local copy and import {b.qualname} from the library",
                     related=[{"path": b.path, "line": b.line}])


def _exempt_call(call: ast.Call, name: str | None) -> bool:
    """Calls whose literal arguments are not inputs: builtins, logging, labels and titles."""
    func = call.func
    if isinstance(func, ast.Name) and name in _BUILTINS and name != "open":
        return True  # print(), len(), range() ...; open() takes a path and is checked
    if name in _LABEL_CALLS:
        return True
    if isinstance(func, ast.Attribute) and name in _LOG_METHODS:
        base = (dotted(func.value) or "").rsplit(".", 1)[-1].lower()
        if "log" in base or base == "warnings":
            return True
    return False


def _path_like(value: str, first_positional: bool) -> bool:
    """A string that names a file: no whitespace, and either a directory segment followed by
    a file name with an extension, or a data extension in the first positional argument."""
    if not value or any(ch.isspace() for ch in value) or "://" in value:
        return False
    if "/" in value and _FILE_EXT_RE.search(value.rsplit("/", 1)[-1]):
        return True
    return first_positional and value.lower().endswith(DATA_EXTS)


def _docstring_nodes(tree: ast.Module) -> list[ast.Expr]:
    out: list[ast.Expr] = []
    for node in ast.walk(tree):
        if isinstance(node, (ast.Module, ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            body = node.body
            if body and isinstance(body[0], ast.Expr) and isinstance(body[0].value, ast.Constant) and isinstance(body[0].value.value, str):
                out.append(body[0])
    return out


def _literal_problem(node: ast.expr, *, first_positional: bool = True) -> str | None:
    """Describe a hard-coded input literal, or return None if the literal is acceptable.

    A string counts as a path when :func:`_path_like` says so: prose such as an axis label
    with a slash ("flux / counts") or a log message is not a path. A bare file name in a
    later position, such as ``os.path.join(out_dir, "summary.json")``, is an output name
    inside a directory that came from elsewhere and is not flagged here.
    """
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, ast.USub) and isinstance(node.operand, ast.Constant):
        inner = node.operand.value
        if isinstance(inner, (int, float)) and not isinstance(inner, bool):
            value = -inner
            return None if value in ALLOWED_NUMBERS else f"numeric literal {value!r}"
        return None
    if not isinstance(node, ast.Constant):
        return None
    v = node.value
    if isinstance(v, bool) or v is None:
        return None
    if isinstance(v, (int, float)):
        return None if v in ALLOWED_NUMBERS else f"numeric literal {v!r}"
    if isinstance(v, str) and _path_like(v, first_positional):
        return f"path literal {v!r}"
    return None


def _numerical_refs(node: ast.AST, aliases: dict[str, str], numerical: set[str]) -> set[str]:
    refs: set[str] = set()
    for sub in ast.walk(node):
        if isinstance(sub, ast.Name) and aliases.get(sub.id) in numerical:
            refs.add(aliases[sub.id])
        elif isinstance(sub, ast.Import):
            for alias in sub.names:
                if alias.name.split(".")[0] in numerical:
                    refs.add(alias.name.split(".")[0])
        elif isinstance(sub, ast.ImportFrom) and sub.module and sub.module.split(".")[0] in numerical:
            refs.add(sub.module.split(".")[0])
    return refs


def contract_check(root: Path, config: ProjectConfig, paths: list[str] | None) -> list[dict[str, Any]]:
    checker = Checker(root, config)
    if paths:
        return checker.check_paths(paths)
    return checker.check_all()
