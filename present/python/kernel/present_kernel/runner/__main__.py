"""``python -I -m present_kernel.runner <job.json>``: the process that runs inside the sandbox.

Job kinds: ``build`` (a code-mode build script), ``render`` (a structured render that contains
generated slides), ``script`` (a skill script). Nothing here trusts the code it runs; isolation
comes from the wrapper that launched this process.
"""

from __future__ import annotations

import json
import os
import runpy
import sys
import traceback
from pathlib import Path


def _apply_limits() -> None:
    limit = int(os.environ.get("PRESENT_RLIMIT_AS") or 0)
    if not limit:
        return
    try:
        import resource
    except ImportError:  # Windows: the job object enforces memory
        return
    try:
        _, hard = resource.getrlimit(resource.RLIMIT_AS)
        soft = limit if hard == resource.RLIM_INFINITY else min(limit, hard)
        resource.setrlimit(resource.RLIMIT_AS, (soft, hard))
    except (ValueError, OSError):
        pass


def _run_generated(slide, doc) -> None:
    script = (doc.get("generated") or {}).get("script") or ""
    scope = {"slide": slide, "deck": slide.deck, "__name__": "__generated__"}
    exec(compile(script, f"<generated slide {doc.get('id')}>", "exec"), scope)


def main(argv: list[str]) -> int:
    job = json.loads(Path(argv[1]).read_text(encoding="utf-8"))
    _apply_limits()
    kind = job["kind"]
    try:
        if kind == "build":
            script = Path(job["script"])
            sys.path.insert(0, str(script.parent))
            os.chdir(job["project"])
            runpy.run_path(str(script), run_name="__main__")
        elif kind == "render":
            from present_kernel.render import render_deck

            result = render_deck(Path(job["project"]), job["out_rel"], job.get("snapshot_id", ""), job.get("slides"),
                                 generated=_run_generated, out_abs=Path(job["out_abs"]))  # fmt: skip
            Path(job["map"]).write_text(json.dumps(result), encoding="utf-8")
        elif kind == "script":
            script = Path(job["script"])
            sys.argv = [str(script), *job.get("args", [])]
            sys.path.insert(0, str(script.parent))
            runpy.run_path(str(script), run_name="__main__")
        else:
            print(f"unknown job kind {kind!r}", file=sys.stderr)
            return 2
    except SystemExit as exc:
        return int(exc.code) if isinstance(exc.code, int) else (0 if exc.code is None else 1)
    except BaseException:
        traceback.print_exc()
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
