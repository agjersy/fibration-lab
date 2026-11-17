"""``python -m present_kernel --project P --home H [--engine auto] [--sandbox auto]`` (PROTOCOL.md 1.1)."""

from __future__ import annotations

import argparse
import logging
import os
import sys
from pathlib import Path

from . import __version__
from .kernel import Kernel
from .rpc import Server


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="present_kernel", description="Personal Present deck kernel (JSON-RPC over stdio)")
    parser.add_argument("--version", action="version", version=f"present_kernel {__version__}")
    parser.add_argument("--project", required=True, help="project root directory")
    parser.add_argument("--home", required=True, help="Present home directory (config, runtime, caches)")
    parser.add_argument("--engine", default="auto", choices=("auto", "powerpoint", "libreoffice", "none"))
    parser.add_argument("--sandbox", default="auto", choices=("auto", "native", "limits"))
    parser.add_argument("--log-level", default=os.environ.get("PRESENT_KERNEL_LOG", "INFO"))
    return parser


def main(argv: list[str] | None = None) -> int:
    ns = build_parser().parse_args(argv)
    logging.basicConfig(level=ns.log_level.upper(), stream=sys.stderr, format="kernel %(levelname)s %(name)s: %(message)s")
    project = Path(ns.project).expanduser()
    if not project.is_dir():
        logging.error("project %s is not a directory", project)
        return 2
    kernel = Kernel(project=project, home=Path(ns.home).expanduser(), engine=ns.engine, sandbox=ns.sandbox)
    server = Server(kernel.dispatcher)
    kernel.set_notify(server.notify)
    try:
        server.serve()
    finally:
        kernel.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
