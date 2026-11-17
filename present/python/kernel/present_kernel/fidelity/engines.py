"""Engine discovery and conversion (ARCHITECTURE.md section 10, PROTOCOL.md 1.3).

Order of preference: PowerPoint when installed, a LibreOffice installed as the optional Present
component under ``<home>/libreoffice``, then a system LibreOffice. Every engine converts
``.pptx`` to PDF; slide images are rasterized from that PDF with PDFium so both engines share
one path. One conversion runs at a time per engine (PowerPoint automation is not concurrent).
"""

from __future__ import annotations

import hashlib
import logging
import os
import shutil
import subprocess
import sys
import tempfile
import threading
from collections.abc import Callable
from pathlib import Path
from typing import Any

log = logging.getLogger("present_kernel.fidelity")

Runner = Callable[..., subprocess.CompletedProcess]

_PPT_MAC = Path("/Applications/Microsoft PowerPoint.app")
_MAC_SCRIPT = """\
on run argv
  set inPath to item 1 of argv
  set outPath to item 2 of argv
  tell application "Microsoft PowerPoint"
    set wasRunning to running
    open POSIX file inPath
    set thePres to active presentation
    save thePres in (POSIX file outPath) as save as PDF
    close thePres saving no
    if not wasRunning then quit
  end tell
end run
"""


class EngineError(Exception):
    def __init__(self, detail: str, unavailable: bool = False):
        super().__init__(detail)
        self.detail = detail
        self.unavailable = unavailable


def _which_soffice(home: Path, explicit: str | None) -> str | None:
    candidates: list[Path] = []
    if explicit:
        candidates.append(Path(explicit))
    exe = "soffice.exe" if sys.platform == "win32" else "soffice"
    bundled = home / "libreoffice"
    candidates += [bundled / "program" / exe, bundled / "Contents" / "MacOS" / "soffice",
                   bundled / "LibreOffice.app" / "Contents" / "MacOS" / "soffice"]  # fmt: skip
    for name in ("soffice", "libreoffice"):
        found = shutil.which(name)
        if found:
            candidates.append(Path(found))
    if sys.platform == "darwin":
        candidates.append(Path("/Applications/LibreOffice.app/Contents/MacOS/soffice"))
    elif sys.platform == "win32":
        for env in ("PROGRAMFILES", "PROGRAMFILES(X86)"):
            base = os.environ.get(env)
            if base:
                candidates.append(Path(base) / "LibreOffice" / "program" / "soffice.exe")
    for c in candidates:
        if c.is_file():
            return str(c)
    return None


def _powerpoint_windows() -> str | None:
    try:
        import winreg  # type: ignore[import-not-found]
    except ImportError:
        return None
    key = r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\powerpnt.exe"
    for hive in (winreg.HKEY_LOCAL_MACHINE, winreg.HKEY_CURRENT_USER):
        try:
            with winreg.OpenKey(hive, key) as k:
                path, _ = winreg.QueryValueEx(k, None)
                if path and Path(path).is_file():
                    return str(path)
        except OSError:
            continue
    return None


class Engines:
    def __init__(self, home: Path, preference: str = "auto", libreoffice_path: str | None = None,
                 runner: Runner | None = None, platform: str | None = None) -> None:
        self.home = Path(home)
        self.preference = preference
        self.platform = platform or sys.platform
        self._run: Runner = runner or subprocess.run
        self._soffice = _which_soffice(self.home, libreoffice_path)
        self._ppt = self._find_powerpoint()
        self._locks = {"powerpoint": threading.Lock(), "libreoffice": threading.Lock()}
        self._profile: tempfile.TemporaryDirectory | None = None
        self._lo_version: str | None = None

    def _find_powerpoint(self) -> str | None:
        if self.platform == "win32":
            return _powerpoint_windows()
        if self.platform == "darwin" and _PPT_MAC.is_dir():
            return str(_PPT_MAC)
        return None

    def close(self) -> None:
        if self._profile is not None:
            self._profile.cleanup()
            self._profile = None

    # ---- discovery ------------------------------------------------------------------------
    def info(self) -> list[dict[str, Any]]:
        out = []
        ppt: dict[str, Any] = {"kind": "powerpoint", "available": self._ppt is not None}
        if self._ppt:
            ppt["path_abs"] = self._ppt
            ppt["detail"] = (
                "automation through COM" if self.platform == "win32"
                else "automation through AppleScript; macOS asks for the Automation permission on first use"
            )  # fmt: skip
        else:
            ppt["detail"] = "Microsoft PowerPoint was not found on this machine"
        out.append(ppt)
        lo: dict[str, Any] = {"kind": "libreoffice", "available": self._soffice is not None}
        if self._soffice:
            lo["path_abs"] = self._soffice
            lo["detail"] = "headless conversion"
            if self._lo_version:
                lo["version"] = self._lo_version
        else:
            lo["detail"] = "LibreOffice was not found; install the optional Present LibreOffice component"
        out.append(lo)
        return out

    def choose(self, requested: str = "auto") -> str:
        want = requested if requested != "auto" else self.preference
        if want == "none":
            raise EngineError("the fidelity engine is disabled by configuration", unavailable=True)
        if want == "powerpoint":
            if not self._ppt:
                raise EngineError("PowerPoint is not installed", unavailable=True)
            return "powerpoint"
        if want == "libreoffice":
            if not self._soffice:
                raise EngineError("LibreOffice is not installed", unavailable=True)
            return "libreoffice"
        if self._ppt:
            return "powerpoint"
        if self._soffice:
            return "libreoffice"
        raise EngineError("no fidelity engine: neither PowerPoint nor LibreOffice is available", unavailable=True)

    # ---- conversion -----------------------------------------------------------------------
    def libreoffice_command(self, pptx: Path, out_dir: Path) -> list[str]:
        if self._profile is None:
            self._profile = tempfile.TemporaryDirectory(prefix="present-lo-")
        profile = Path(self._profile.name).as_uri()
        return [str(self._soffice), "--headless", "--norestore", "--nolockcheck", f"-env:UserInstallation={profile}",
                "--convert-to", "pdf", "--outdir", str(out_dir), str(pptx)]  # fmt: skip

    def _lo_to_pdf(self, pptx: Path, pdf: Path, timeout: float) -> None:
        with tempfile.TemporaryDirectory(prefix="present-conv-") as tmp:
            # Convert a copy with a plain name: LibreOffice names the output after the input.
            src = Path(tmp) / "deck.pptx"
            shutil.copyfile(pptx, src)
            cmd = self.libreoffice_command(src, Path(tmp))
            try:
                proc = self._run(cmd, capture_output=True, timeout=timeout)
            except subprocess.TimeoutExpired as exc:
                raise EngineError(f"LibreOffice did not finish within {timeout:.0f} s") from exc
            produced = Path(tmp) / "deck.pdf"
            if proc.returncode != 0 or not produced.is_file():
                err = (proc.stderr or b"").decode("utf-8", "replace").strip()[-400:]
                raise EngineError(f"LibreOffice conversion failed (exit {proc.returncode}): {err}")
            pdf.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(produced), pdf)

    def powerpoint_command(self, pptx: Path, pdf: Path) -> list[str]:
        if self.platform == "win32":
            return [sys.executable, "-I", "-m", "present_kernel.fidelity.ppt_win", str(pptx), str(pdf)]
        return ["osascript", "-e", _MAC_SCRIPT, str(pptx), str(pdf)]

    def _ppt_to_pdf(self, pptx: Path, pdf: Path, timeout: float) -> None:
        pdf.parent.mkdir(parents=True, exist_ok=True)
        cmd = self.powerpoint_command(pptx.resolve(), pdf.resolve())
        try:
            proc = self._run(cmd, capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired as exc:
            raise EngineError(f"PowerPoint did not finish within {timeout:.0f} s") from exc
        err = (proc.stderr or b"").decode("utf-8", "replace").strip()
        if proc.returncode != 0 or not pdf.is_file():
            if "-1743" in err or "Not authorized" in err:
                raise EngineError(
                    "macOS blocked automation of PowerPoint: allow Present under System Settings > Privacy & "
                    "Security > Automation", unavailable=True)  # fmt: skip
            raise EngineError(f"PowerPoint export failed (exit {proc.returncode}): {err[-400:]}")

    def to_pdf(self, pptx: Path, pdf: Path, engine: str = "auto", timeout: float = 180.0) -> str:
        """Convert; returns the engine used. Falls back from PowerPoint to LibreOffice under ``auto``."""
        chosen = self.choose(engine)
        with self._locks[chosen]:
            if chosen == "libreoffice":
                self._lo_to_pdf(pptx, pdf, timeout)
                return chosen
            try:
                self._ppt_to_pdf(pptx, pdf, timeout)
                return chosen
            except EngineError as exc:
                if engine != "auto" or self.preference == "powerpoint" or not self._soffice:
                    raise
                log.warning("PowerPoint failed (%s); falling back to LibreOffice", exc.detail)
        with self._locks["libreoffice"]:
            self._lo_to_pdf(pptx, pdf, timeout)
        return "libreoffice"


def pdf_to_pngs(pdf: Path, out_dir: Path, dpi: int = 96, pages: list[int] | None = None, stem: str = "slide") -> list[Path]:
    """Rasterize PDF pages (0-based indexes) to ``<stem>-<n>.png`` with 1-based numbering."""
    import pypdfium2 as pdfium

    out_dir.mkdir(parents=True, exist_ok=True)
    doc = pdfium.PdfDocument(str(pdf))
    try:
        wanted = range(len(doc)) if pages is None else [p for p in pages if 0 <= p < len(doc)]
        files = []
        for i in wanted:
            page = doc[i]
            image = page.render(scale=dpi / 72).to_pil()
            path = out_dir / f"{stem}-{i + 1:03d}.png"
            image.save(path, optimize=False)
            files.append(path)
        return files
    finally:
        doc.close()


def sha256_path(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()
