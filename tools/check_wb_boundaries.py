#!/usr/bin/env python3
"""Crate boundaries.

1. A shared core crate (crates/wb-*) does not depend on a workbench-specific crate (fibration-*,
   which includes the deck crates fibration-deck and fibration-deck-protocol).
2. The desktop app does not link the evaluation harness (fibration-evals) or the development
   bridge (fibration-serve), checked through Cargo.lock, transitively.
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
            if name.startswith(("fibration-", "present-")):
                bad.append(f"{manifest.parent.name} -> {name} ({section})")

packages = tomllib.loads((root / "Cargo.lock").read_text()).get("package", [])
deps: dict[str, set[str]] = {}
for p in packages:
    deps.setdefault(p["name"], set()).update(d.split(" ")[0] for d in p.get("dependencies", []))
seen, todo = set(), ["fibration-desktop"]
while todo:
    name = todo.pop()
    if name not in seen:
        seen.add(name)
        todo.extend(deps.get(name, ()))
if "fibration-desktop" not in deps:
    bad.append("Cargo.lock has no fibration-desktop (update tools/check_wb_boundaries.py)")
for crate in ("fibration-evals", "fibration-serve"):
    if crate in seen:
        bad.append(f"fibration-desktop links {crate}")

if bad:
    print("crate boundaries crossed:\n  " + "\n  ".join(bad))
    sys.exit(1)
print("ok: wb-* crates are independent of the fibration-* crates; the desktop app links no evals or bridge")
