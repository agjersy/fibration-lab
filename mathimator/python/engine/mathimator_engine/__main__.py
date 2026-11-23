"""``python -m mathimator_engine [--memo-dir DIR]``: serve the engine on stdio."""

from __future__ import annotations

import argparse
import logging
import sys
from pathlib import Path


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="mathimator-engine", description=__doc__)
    parser.add_argument("--memo-dir", type=Path, help="directory that keeps memoized results between runs")
    parser.add_argument("--log-level", default="WARNING")
    args = parser.parse_args(argv)
    logging.basicConfig(stream=sys.stderr, level=args.log_level.upper(), format="%(name)s %(levelname)s %(message)s")
    sys.setrecursionlimit(max(sys.getrecursionlimit(), 4000))

    from .engine import Engine
    from .server import serve

    return serve(Engine(memo_dir=args.memo_dir))


if __name__ == "__main__":
    sys.exit(main())
