#!/usr/bin/env python3
"""Fail on copyleft licenses in the Python environment (BUILD-SPEC rule 4). Input: pip-licenses CSV."""
import csv
import re
import sys

DENY = re.compile(r"\b(A?GPL|LGPL|GNU (Affero |Lesser |Library )?General Public)\b", re.I)
# Dual-licensed packages where a permissive option exists, verified by hand.
ALLOW = {"text-unidecode"}
bad = []
with open(sys.argv[1], newline="") as fh:
    for row in csv.DictReader(fh):
        name, lic = row["Name"], row["License"]
        if DENY.search(lic) and name not in ALLOW and "MIT" not in lic and "BSD" not in lic and "Apache" not in lic:
            bad.append(f"{name}: {lic}")
if bad:
    print("copyleft-licensed packages found:\n  " + "\n  ".join(bad))
    sys.exit(1)
print("ok: no copyleft licenses")
