"""``deck.build``, ``slide.generate``, ``deck.eject``, sandboxed renders, and skill scripts."""

from __future__ import annotations

import hashlib
import json
import shutil
import sys
import tempfile
from pathlib import Path
from typing import Any

from ..errors import BUILD_FAILED, NOT_FOUND, TIMEOUT, RpcError, invalid_params
from ..methods import group, hello_provider
from ..paths import resolve_in_root
from ..render import has_generated, load_deck, render_deck
from . import sandbox as sb
from .eject import compare, eject_source
from .lint import lint_script


def sandbox_info(kernel) -> dict[str, Any]:
    info = getattr(kernel, "_sandbox_info", None)
    if info is None:
        pref = {"auto": "auto", "native": "native", "limits": "limits"}.get(kernel.sandbox_pref, "auto")
        info = sb.detect(pref, kernel.home)
        kernel._sandbox_info = info
    return info


@hello_provider
def _hello(kernel) -> dict[str, Any]:
    return {"sandbox": sandbox_info(kernel)}


def _spec(kernel, scratch: Path, extra_ro: list[Path] | None = None) -> sb.SandboxSpec:
    project = kernel.project
    from present_sdk.fonts import system_font_dirs

    ro = [*sb.interpreter_dirs(Path(sys.executable)), *sb.code_dirs(), *system_font_dirs(),
          project / "template", project / "assets", project / "deck", project / "references",
          project / ".present" / "cache" / "refs", *(extra_ro or [])]  # fmt: skip
    return sb.SandboxSpec(cwd=scratch, scratch=scratch, read_only=ro, enter=[project])


def run_job(kernel, job: dict[str, Any], timeout: float, scratch: Path, env: dict[str, str] | None = None,
            extra_ro: list[Path] | None = None) -> sb.Result:
    job_path = scratch / "job.json"
    job_path.write_text(json.dumps(job), encoding="utf-8")
    cmd = [sys.executable, "-I", "-m", "present_kernel.runner", str(job_path)]
    kind = sandbox_info(kernel)["kind"]
    return sb.run(kind, _spec(kernel, scratch, extra_ro), cmd, env or {}, timeout, kernel.home)


def _fail(result: sb.Result, timeout: float, what: str) -> RpcError:
    if result.timed_out:
        return RpcError(TIMEOUT, f"{what} did not finish within {timeout:.0f} s", stdout=result.stdout[-4000:],
                        stderr=result.stderr[-4000:])  # fmt: skip
    return RpcError(BUILD_FAILED, f"{what} failed with exit code {result.exit_code}", stdout=result.stdout[-4000:],
                    stderr=result.stderr[-8000:], exit_code=result.exit_code)  # fmt: skip


def _deck_env(kernel, deck: dict[str, Any], out: Path, map_path: Path, snapshot_id: str) -> dict[str, str]:
    return {"PRESENT_PROJECT": str(kernel.project), "PRESENT_TEMPLATE": deck["template"], "PRESENT_OUT": str(out),
            "PRESENT_MAP": str(map_path), "PRESENT_SNAPSHOT": snapshot_id}  # fmt: skip


def build(kernel, script_rel: str, out_rel: str, snapshot_id: str, timeout: float, out_abs: Path | None = None) -> dict[str, Any]:
    script = kernel.path(script_rel)
    if not script.is_file():
        raise RpcError(NOT_FOUND, f"{script_rel} not found")
    deck, _ = load_deck(kernel.project) if (kernel.project / "deck/deck.json").is_file() else ({}, {})
    if not deck.get("template"):
        raise RpcError(NOT_FOUND, "deck/deck.json with a template is required to build")
    target = out_abs or resolve_in_root(kernel.project, out_rel)
    with tempfile.TemporaryDirectory(prefix="present-build-") as tmp:
        scratch = Path(tmp)
        produced, map_path = scratch / "out.pptx", scratch / "map.json"
        job = {"kind": "build", "script": str(script), "project": str(kernel.project)}
        result = run_job(kernel, job, timeout, scratch, _deck_env(kernel, deck, produced, map_path, snapshot_id))
        if result.exit_code != 0 or result.timed_out:
            raise _fail(result, timeout, "the build script")
        if not produced.is_file():
            raise RpcError(BUILD_FAILED, "the build script finished without calling deck.save()", stdout=result.stdout[-4000:],
                           stderr=result.stderr[-4000:], exit_code=0)  # fmt: skip
        meta = json.loads(map_path.read_text(encoding="utf-8")) if map_path.is_file() else {"element_map": [], "warnings": []}
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(produced), target)
    warnings = list(meta.get("warnings", [])) + lint_script(script.read_text(encoding="utf-8"), script_rel)
    if result.kind == "limits":
        warnings.append({"rule": "build", "severity": "warn", "message": "the script ran with resource limits only (unsandboxed)"})
    return {"out": out_rel, "sha256": hashlib.sha256(target.read_bytes()).hexdigest(), "element_map": meta.get("element_map", []),
            "stdout": result.stdout, "stderr": result.stderr, "warnings": warnings, "sandbox": result.kind}  # fmt: skip


def sandboxed_render(kernel, out_rel: str, snapshot_id: str, slides: list[str] | None, timeout: float = 60.0) -> dict[str, Any]:
    """A structured render that contains generated slides: the whole render runs in the sandbox."""
    target = resolve_in_root(kernel.project, out_rel)
    with tempfile.TemporaryDirectory(prefix="present-render-") as tmp:
        scratch = Path(tmp)
        produced, map_path = scratch / "out.pptx", scratch / "result.json"
        job = {"kind": "render", "project": str(kernel.project), "out_rel": out_rel, "out_abs": str(produced),
               "snapshot_id": snapshot_id, "slides": slides, "map": str(map_path)}  # fmt: skip
        result = run_job(kernel, job, timeout, scratch)
        if result.exit_code != 0 or result.timed_out or not produced.is_file():
            raise _fail(result, timeout, "a generated slide")
        out = json.loads(map_path.read_text(encoding="utf-8"))
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(produced), target)
    out["sha256"] = hashlib.sha256(target.read_bytes()).hexdigest()
    out["stdout"], out["stderr"], out["sandbox"] = result.stdout, result.stderr, result.kind
    _, docs = load_deck(kernel.project)
    for sid, doc in docs.items():
        if doc.get("kind") == "generated" and (slides is None or sid in slides):
            out["warnings"] = out.get("warnings", []) + [
                {**d, "slide_id": sid} for d in lint_script((doc.get("generated") or {}).get("script") or "", f"slide {sid}")
            ]
    return out


@group
def _register(kernel) -> None:
    def timeout_of(params: dict[str, Any]) -> float:
        value = params.get("timeout_s")
        return float(value) if isinstance(value, (int, float)) and value > 0 else 60.0

    def deck_build(params: dict[str, Any]) -> dict[str, Any]:
        for key in ("script", "out"):
            if not isinstance(params.get(key), str):
                raise invalid_params(f"{key} is required")
        return build(kernel, params["script"], params["out"], params.get("snapshot_id") or "", timeout_of(params))

    def slide_generate(params: dict[str, Any]) -> dict[str, Any]:
        sid = params.get("slide_id")
        if not isinstance(sid, str):
            raise invalid_params("slide_id is required")
        out_rel = params.get("out") or f".present/cache/generated/{sid}.pptx"
        result = sandboxed_render(kernel, out_rel, params.get("snapshot_id") or "", [sid], timeout_of(params))
        return {"out": out_rel, "stdout": result["stdout"], "stderr": result["stderr"], "warnings": result.get("warnings", [])}

    def deck_eject(params: dict[str, Any]) -> dict[str, Any]:
        out_dir = params.get("out_dir") or "deck"
        script_rel = f"{out_dir.rstrip('/')}/build.py"
        script = kernel.path(script_rel)
        source = eject_source(kernel.project)
        script.parent.mkdir(parents=True, exist_ok=True)
        script.write_text(source, encoding="utf-8")
        with tempfile.TemporaryDirectory(prefix="present-eject-") as tmp:
            rendered, built = Path(tmp) / "render.pptx", Path(tmp) / "build.pptx"
            if has_generated(kernel.project):
                tmp_rel = ".present/cache/eject-render.pptx"
                sandboxed_render(kernel, tmp_rel, "", None)
                shutil.move(str(kernel.path(tmp_rel)), rendered)
            else:
                render_deck(kernel.project, "eject", "", out_abs=rendered)
            build(kernel, script_rel, "eject", "", timeout_of(params), out_abs=built)
            diff = compare(rendered, built)
        out: dict[str, Any] = {"script": script_rel, "verified": diff is None}
        if diff is not None:
            out["diff"] = diff
        return out

    def skill_run(params: dict[str, Any]) -> dict[str, Any]:
        script = Path(params.get("script_abs") or "")
        skill_dir = Path(params.get("skill_dir_abs") or "")
        if not script.is_file() or skill_dir not in script.parents:
            raise RpcError(NOT_FOUND, "skill script not found inside its skill folder")
        out_dir = kernel.path(params.get("out_dir") or ".present/cache/skills")
        with tempfile.TemporaryDirectory(prefix="present-skill-") as tmp:
            scratch = Path(tmp)
            job = {"kind": "script", "script": str(script), "args": [str(a) for a in params.get("args") or []]}
            result = run_job(kernel, job, timeout_of(params), scratch, {"PRESENT_PROJECT": str(kernel.project)}, [skill_dir])
            files = []
            if result.exit_code == 0 and not result.timed_out:
                out_dir.mkdir(parents=True, exist_ok=True)
                for f in sorted(scratch.iterdir()):
                    if f.is_file() and f.name != "job.json":
                        shutil.copyfile(f, out_dir / f.name)
                        files.append(str((out_dir / f.name).relative_to(kernel.project).as_posix()))
        return {"exit_code": result.exit_code, "timed_out": result.timed_out, "stdout": result.stdout[-8000:],
                "stderr": result.stderr[-8000:], "files": files, "sandbox": result.kind}  # fmt: skip

    kernel.sandboxed_render = lambda out, snap, slides: sandboxed_render(kernel, out, snap, slides)
    kernel.dispatcher.register("deck.build", deck_build)
    kernel.dispatcher.register("slide.generate", slide_generate)
    kernel.dispatcher.register("deck.eject", deck_eject)
    kernel.dispatcher.register("skill.run_script", skill_run)
