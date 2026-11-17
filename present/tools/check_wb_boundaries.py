#!/usr/bin/env python3
"""Fail if a shared-to-be crate (crates/wb-*) depends on a Present-specific crate (BUILD-SPEC section 4)."""
import pathlib
import sys
import tomllib

root = pathlib.Path(__file__).resolve().parent.parent
bad = []
for manifest in sorted((root / "crates").glob("wb-*/Cargo.toml")):
    data = tomllib.loads(manifest.read_text())
    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        for name in data.get(section, {}):
            if name.startswith("present-"):
                bad.append(f"{manifest.parent.name} -> {name} ({section})")
if bad:
    print("shared crates depend on Present crates:\n  " + "\n  ".join(bad))
    sys.exit(1)
print("ok: wb-* crates are independent of present-* crates")
