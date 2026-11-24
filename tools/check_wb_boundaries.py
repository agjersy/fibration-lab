#!/usr/bin/env python3
"""Crate boundaries.

1. A shared core crate (crates/wb-*) does not depend on a workbench-specific crate (analyst-*,
   which includes the deck crates analyst-deck and analyst-deck-protocol).
2. The desktop app does not link the evaluation harness (analyst-evals) or the development
   bridge (analyst-serve), checked through Cargo.lock, transitively.
"""
import pathlib
import sys
import tomllib

root = pathlib.Path(__file__).resolve().parent.parent
bad = []
for manifest in sorted((root / "crates").glob("wb-*/Cargo.toml")):
    data = tomllib.loads(manifest.read_text())
    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        for name in data.get(section, {}):
            if name.startswith(("analyst-", "present-")):
                bad.append(f"{manifest.parent.name} -> {name} ({section})")

packages = tomllib.loads((root / "Cargo.lock").read_text()).get("package", [])
deps: dict[str, set[str]] = {}
for p in packages:
    deps.setdefault(p["name"], set()).update(d.split(" ")[0] for d in p.get("dependencies", []))
seen, todo = set(), ["analyst-desktop"]
while todo:
    name = todo.pop()
    if name not in seen:
        seen.add(name)
        todo.extend(deps.get(name, ()))
if "analyst-desktop" not in deps:
    bad.append("Cargo.lock has no analyst-desktop (update tools/check_wb_boundaries.py)")
for crate in ("analyst-evals", "analyst-serve"):
    if crate in seen:
        bad.append(f"analyst-desktop links {crate}")

if bad:
    print("crate boundaries crossed:\n  " + "\n  ".join(bad))
    sys.exit(1)
print("ok: wb-* crates are independent of the analyst-* crates; the desktop app links no evals or bridge")
