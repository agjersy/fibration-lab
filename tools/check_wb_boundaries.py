#!/usr/bin/env python3
"""Fail if a shared core crate (crates/wb-*) depends on a workbench-specific crate (analyst-* or present-*)."""
import pathlib
import sys
import tomllib

root = pathlib.Path(__file__).resolve().parent.parent
bad = []
for manifest in sorted((root / "crates").glob("wb-*/Cargo.toml")):
    data = tomllib.loads(manifest.read_text())
    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        for name in data.get(section, {}):
            if name.startswith(("present-", "analyst-")):
                bad.append(f"{manifest.parent.name} -> {name} ({section})")
if bad:
    print("shared crates depend on workbench crates:\n  " + "\n  ".join(bad))
    sys.exit(1)
print("ok: wb-* crates are independent of analyst-* and present-* crates")
