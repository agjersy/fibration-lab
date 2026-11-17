#!/usr/bin/env python3
"""Build the relocatable Python runtime that ships inside the installer (BUILD-SPEC W3).

    python3 tools/bundle_runtime.py --out dist/runtime [--python 3.12]

The result is a standalone CPython (python-build-standalone, fetched by `uv`) with the fixed
package set, `present_sdk`, and `present_kernel` installed into it, plus the generated JSON
Schemas. Nothing in it refers to this checkout, and it needs no network at run time: the
script finishes by starting the kernel from the bundle with networking pointed at a dead proxy.

Building needs network access (to fetch the interpreter and wheels); the installer then
carries the output directory as is. Run it once per operating system and architecture.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def run(cmd: list[str], **kwargs) -> subprocess.CompletedProcess:
    print("+", " ".join(str(c) for c in cmd), flush=True)
    return subprocess.run(cmd, check=True, **kwargs)


def find_python(root: Path) -> Path:
    names = ("python.exe",) if sys.platform == "win32" else ("python3", "python")
    for path in sorted(root.rglob("*")):
        if path.name in names and path.is_file() and "site-packages" not in path.parts:
            if sys.platform == "win32" or path.parent.name == "bin":
                return path
    raise SystemExit(f"no interpreter found under {root}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--out", required=True, type=Path, help="output directory (replaced if it exists)")
    ap.add_argument("--python", default="3.12", help="CPython version to bundle")
    args = ap.parse_args()
    out: Path = args.out.resolve()
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)

    # 1. A standalone interpreter, copied rather than linked so the directory can be moved.
    install = out / "_install"
    run(["uv", "python", "install", args.python, "--install-dir", str(install)])
    found = find_python(install)
    home = found.parent.parent if found.parent.name == "bin" else found.parent
    target = out / "python"
    shutil.copytree(home, target, symlinks=False)
    shutil.rmtree(install)
    python = find_python(target)
    for marker in target.rglob("EXTERNALLY-MANAGED"):
        marker.unlink()

    # 2. The fixed package set at the locked versions, then the SDK and the kernel themselves.
    requirements = out / "requirements.txt"
    exported = run(["uv", "export", "--frozen", "--no-dev", "--no-emit-project", "--no-emit-package", "present-sdk",
                    "--format", "requirements-txt"], cwd=REPO / "python" / "kernel", capture_output=True, text=True)  # fmt: skip
    requirements.write_text(exported.stdout, encoding="utf-8")
    run(["uv", "pip", "install", "--python", str(python), "--require-hashes", "-r", str(requirements)])
    run(["uv", "pip", "install", "--python", str(python), "--no-deps", str(REPO / "python" / "sdk"), str(REPO / "python" / "kernel")])

    # 3. The JSON Schemas travel inside the kernel package (validate.py looks there first).
    site = subprocess.run([str(python), "-I", "-c", "import present_kernel, pathlib; print(pathlib.Path(present_kernel.__file__).parent)"],
                          capture_output=True, text=True, check=True).stdout.strip()  # fmt: skip
    shutil.copytree(REPO / "schemas", Path(site) / "schemas")

    # 3b. The built-in skills.
    shutil.copytree(REPO / "skills", out / "skills")

    # 4. Prove it runs with no network and no reference to the checkout.
    env = {k: v for k, v in os.environ.items() if k.upper() not in ("PYTHONPATH", "VIRTUAL_ENV", "UV_PROJECT_ENVIRONMENT")}
    dead = "http://127.0.0.1:9"
    env.update({"HTTP_PROXY": dead, "HTTPS_PROXY": dead, "http_proxy": dead, "https_proxy": dead, "NO_PROXY": "", "PIP_NO_INDEX": "1"})
    version = subprocess.run([str(python), "-I", "-m", "present_kernel", "--version"], capture_output=True, text=True, env=env, cwd=out)
    if version.returncode != 0:
        print(version.stderr, file=sys.stderr)
        raise SystemExit("the bundled kernel does not start")
    probe = ("import json, present_kernel, present_sdk, pptx, pandas, pypdfium2, uharfbuzz, onnxruntime;"
             "print(json.dumps({'kernel': present_kernel.__file__, 'sdk': present_sdk.__file__}))")
    where = json.loads(subprocess.run([str(python), "-I", "-c", probe], capture_output=True, text=True, env=env, cwd=out, check=True).stdout)
    for name, path in where.items():
        if not Path(path).resolve().is_relative_to(out):
            raise SystemExit(f"{name} was imported from outside the bundle: {path}")
    size = sum(f.stat().st_size for f in out.rglob("*") if f.is_file()) / 1e6
    manifest = {"python": args.python, "platform": sys.platform, "interpreter": str(python.relative_to(out)),
                "kernel": version.stdout.strip(), "size_mb": round(size, 1)}  # fmt: skip
    (out / "runtime.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
