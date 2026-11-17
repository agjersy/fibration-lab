"""``code.hardcoded-style``: literal colors and font names in deck scripts (PROTOCOL.md section 7)."""

from __future__ import annotations

import ast
import re
from typing import Any

_HEX = re.compile(r"^#?[0-9A-Fa-f]{6}$")
_COLOR_CALLS = {"RGBColor", "from_string"}


def lint_script(source: str, path: str = "deck/build.py") -> list[dict[str, Any]]:
    try:
        tree = ast.parse(source)
    except SyntaxError as exc:
        return [{"rule": "build", "severity": "error", "message": f"{path}:{exc.lineno}: syntax error: {exc.msg}"}]
    out: list[dict[str, Any]] = []

    def add(node: ast.AST, what: str) -> None:
        out.append({"rule": "code.hardcoded-style", "severity": "warn",
                    "message": f"{path}:{getattr(node, 'lineno', 0)}: {what}",
                    "fix_hint": "use theme.color(...) / theme.font(...) so the deck follows the template"})  # fmt: skip

    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            name = node.func.attr if isinstance(node.func, ast.Attribute) else getattr(node.func, "id", "")
            if name in _COLOR_CALLS:
                add(node, "literal RGB color")
            for kw in node.keywords:
                if kw.arg in ("typeface", "font_name") and isinstance(kw.value, ast.Constant) and isinstance(kw.value.value, str) \
                        and not kw.value.value.startswith("+"):
                    add(node, f"literal font name {kw.value.value!r}")
        elif isinstance(node, ast.Assign) and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
            for target in node.targets:
                if not isinstance(target, ast.Attribute):
                    continue
                chain = ast.unparse(target)
                if target.attr == "name" and ".font" in chain and not node.value.value.startswith("+"):
                    add(node, f"literal font name {node.value.value!r}")
                elif target.attr == "rgb" and _HEX.match(node.value.value):
                    add(node, f"literal color {node.value.value!r}")
    return sorted(out, key=lambda d: int(d["message"].split(":")[1]))
