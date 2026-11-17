from __future__ import annotations

import shutil
import subprocess
import time
from pathlib import Path

import pytest

from present_kernel.errors import RpcError
from present_kernel.fidelity.engines import EngineError, Engines

HAVE_LO = shutil.which("soffice") is not None or shutil.which("libreoffice") is not None
needs_lo = pytest.mark.skipif(not HAVE_LO, reason="LibreOffice is not installed")


def _rendered(kernel_for, project):
    k = kernel_for(project)
    k.dispatcher.call("deck.render", {"out": "exports/deck.pptx", "snapshot_id": "h-1"})
    return k


@needs_lo
def test_png_and_pdf_through_libreoffice(kernel_for, corp_project):
    k = _rendered(kernel_for, corp_project)
    started = time.monotonic()
    out = k.dispatcher.call("render.fidelity", {"pptx": "exports/deck.pptx", "format": "png", "out_dir": ".present/cache/t", "engine": "libreoffice"})
    elapsed = time.monotonic() - started
    assert out["engine"] == "libreoffice"
    assert len(out["files"]) == 31  # 32 slides, one hidden
    assert all((corp_project / f).stat().st_size > 1000 for f in out["files"])
    assert elapsed < 30, f"render took {elapsed:.1f}s"
    pdf = k.dispatcher.call("render.fidelity", {"pptx": "exports/deck.pptx", "format": "pdf", "out_dir": "exports"})
    assert (corp_project / pdf["files"][0]).read_bytes().startswith(b"%PDF")
    again = time.monotonic()
    k.dispatcher.call("render.fidelity", {"pptx": "exports/deck.pptx", "format": "png", "out_dir": ".present/cache/t", "slides": [0, 2]})
    assert time.monotonic() - again < 5  # served from the PDF cache


@needs_lo
def test_template_thumbnails(kernel_for, corp_project):
    out = kernel_for(corp_project).dispatcher.call("template.thumbnails", {"path": "template/corp.potx"})
    assert len(out) == 13 and out[10]["layout"] == "Layout 7"
    assert all((corp_project / t["png"]).is_file() for t in out)


def test_hello_reports_engines(kernel_for, tmp_path):
    hello = kernel_for(tmp_path).dispatcher.call("kernel.hello", {"core_version": "0", "protocol_version": 1})
    kinds = {e["kind"]: e for e in hello["engines"]}
    assert set(kinds) == {"powerpoint", "libreoffice"}
    assert kinds["libreoffice"]["available"] is HAVE_LO


def test_engine_none_is_unavailable(tmp_path, corp_project):
    from present_kernel.kernel import Kernel

    k = Kernel(corp_project, tmp_path, engine="none")
    k.dispatcher.call("deck.render", {"out": "exports/deck.pptx", "snapshot_id": "h-1"})
    with pytest.raises(RpcError) as err:
        k.dispatcher.call("render.fidelity", {"pptx": "exports/deck.pptx", "format": "pdf", "out_dir": "exports"})
    assert err.value.code == 2008


class FakeRunner:
    def __init__(self, behaviour):
        self.calls: list[list[str]] = []
        self.behaviour = behaviour

    def __call__(self, cmd, **kwargs):
        self.calls.append(cmd)
        return self.behaviour(cmd, kwargs)


def _engines(tmp_path, runner, platform, ppt=True, soffice=True):
    e = Engines(tmp_path, runner=runner, platform=platform)
    e._ppt = "/Applications/Microsoft PowerPoint.app" if ppt else None
    e._soffice = "/usr/bin/soffice" if soffice else None
    return e


def test_powerpoint_is_preferred_and_macos_uses_osascript(tmp_path):
    def ok(cmd, kwargs):
        Path(cmd[-1]).write_bytes(b"%PDF-1.7")
        return subprocess.CompletedProcess(cmd, 0, b"", b"")

    runner = FakeRunner(ok)
    e = _engines(tmp_path, runner, "darwin")
    src = tmp_path / "a.pptx"
    src.write_bytes(b"x")
    assert e.to_pdf(src, tmp_path / "out" / "a.pdf") == "powerpoint"
    assert runner.calls[0][0] == "osascript" and "save as PDF" in runner.calls[0][2]


def test_windows_uses_the_com_helper(tmp_path):
    def ok(cmd, kwargs):
        Path(cmd[-1]).write_bytes(b"%PDF-1.7")
        return subprocess.CompletedProcess(cmd, 0, b"", b"")

    runner = FakeRunner(ok)
    e = _engines(tmp_path, runner, "win32")
    src = tmp_path / "a.pptx"
    src.write_bytes(b"x")
    e.to_pdf(src, tmp_path / "a.pdf", engine="powerpoint")
    assert runner.calls[0][2:4] == ["-m", "present_kernel.fidelity.ppt_win"]


def test_auto_falls_back_to_libreoffice_when_powerpoint_fails(tmp_path):
    def behaviour(cmd, kwargs):
        if cmd[0] == "osascript":
            return subprocess.CompletedProcess(cmd, 1, b"", b"execution error: boom (-2700)")
        outdir = Path(cmd[cmd.index("--outdir") + 1])
        (outdir / "deck.pdf").write_bytes(b"%PDF-1.7")
        return subprocess.CompletedProcess(cmd, 0, b"", b"")

    runner = FakeRunner(behaviour)
    e = _engines(tmp_path, runner, "darwin")
    src = tmp_path / "a.pptx"
    src.write_bytes(b"x")
    assert e.to_pdf(src, tmp_path / "a.pdf") == "libreoffice"
    assert "--headless" in runner.calls[1] and any(a.startswith("-env:UserInstallation=") for a in runner.calls[1])
    e.close()


def test_automation_permission_denial_is_reported(tmp_path):
    def denied(cmd, kwargs):
        return subprocess.CompletedProcess(cmd, 1, b"", b"Not authorized to send Apple events to Microsoft PowerPoint. (-1743)")

    e = _engines(tmp_path, FakeRunner(denied), "darwin", soffice=False)
    src = tmp_path / "a.pptx"
    src.write_bytes(b"x")
    with pytest.raises(EngineError) as err:
        e.to_pdf(src, tmp_path / "a.pdf")
    assert err.value.unavailable and "Automation" in err.value.detail


def test_timeout_is_an_engine_error(tmp_path):
    def slow(cmd, kwargs):
        raise subprocess.TimeoutExpired(cmd, kwargs.get("timeout", 1))

    e = _engines(tmp_path, FakeRunner(slow), "linux", ppt=False)
    src = tmp_path / "a.pptx"
    src.write_bytes(b"x")
    with pytest.raises(EngineError):
        e.to_pdf(src, tmp_path / "a.pdf", timeout=1)
    e.close()


def test_no_engine(tmp_path):
    e = _engines(tmp_path, FakeRunner(lambda c, k: None), "linux", ppt=False, soffice=False)
    with pytest.raises(EngineError) as err:
        e.choose()
    assert err.value.unavailable
