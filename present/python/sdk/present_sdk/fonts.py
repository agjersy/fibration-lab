"""Font discovery by family name, for text measurement and the ``fonts_missing`` report."""

from __future__ import annotations

import os
import sys
import threading
from pathlib import Path

from PIL import ImageFont

_EXTS = {".ttf", ".otf", ".ttc"}


def system_font_dirs() -> list[Path]:
    home = Path.home()
    if sys.platform == "win32":
        dirs = [Path(os.environ.get("WINDIR", r"C:\Windows")) / "Fonts"]
        local = os.environ.get("LOCALAPPDATA")
        if local:
            dirs.append(Path(local) / "Microsoft" / "Windows" / "Fonts")
    elif sys.platform == "darwin":
        dirs = [Path("/System/Library/Fonts"), Path("/Library/Fonts"), home / "Library" / "Fonts",
                Path("/Applications/Microsoft PowerPoint.app/Contents/Resources/DFonts")]  # fmt: skip
    else:
        dirs = [Path("/usr/share/fonts"), Path("/usr/local/share/fonts"), home / ".fonts", home / ".local/share/fonts"]
    extra = os.environ.get("PRESENT_FONT_DIRS", "")
    dirs = [Path(p) for p in extra.split(os.pathsep) if p] + dirs
    return [d for d in dirs if d.is_dir()]


class FontIndex:
    """Maps ``(family, bold, italic)`` to a font file. Built lazily on first lookup."""

    _default: FontIndex | None = None
    _default_lock = threading.Lock()

    def __init__(self, dirs: list[Path] | None = None) -> None:
        self._dirs = dirs
        self._faces: dict[tuple[str, bool, bool], tuple[str, int]] | None = None
        self._lock = threading.Lock()

    @classmethod
    def default(cls) -> FontIndex:
        with cls._default_lock:
            if cls._default is None:
                cls._default = cls()
            return cls._default

    @classmethod
    def reset_default(cls) -> None:
        with cls._default_lock:
            cls._default = None

    def _scan(self) -> dict[tuple[str, bool, bool], tuple[str, int]]:
        faces: dict[tuple[str, bool, bool], tuple[str, int]] = {}
        dirs = self._dirs if self._dirs is not None else system_font_dirs()
        for d in dirs:
            for path in sorted(d.rglob("*")):
                if path.suffix.lower() not in _EXTS:
                    continue
                count = 8 if path.suffix.lower() == ".ttc" else 1
                for i in range(count):
                    try:
                        family, style = ImageFont.truetype(str(path), 10, index=i).getname()
                    except Exception:
                        break
                    if not family:
                        continue
                    st = (style or "").lower()
                    key = (family.lower(), "bold" in st, "italic" in st or "oblique" in st)
                    faces.setdefault(key, (str(path), i))
        return faces

    def faces(self) -> dict[tuple[str, bool, bool], tuple[str, int]]:
        with self._lock:
            if self._faces is None:
                self._faces = self._scan()
            return self._faces

    def has_family(self, family: str) -> bool:
        fam = family.lower()
        return any(k[0] == fam for k in self.faces())

    def find(self, family: str, bold: bool = False, italic: bool = False) -> tuple[str, int] | None:
        faces = self.faces()
        fam = family.lower()
        for key in ((fam, bold, italic), (fam, bold, False), (fam, False, italic), (fam, False, False)):
            if key in faces:
                return faces[key]
        return None

    def fallback(self, bold: bool = False, italic: bool = False) -> tuple[str, int] | None:
        for fam in ("liberation sans", "arial", "helvetica", "dejavu sans", "carlito", "calibri"):
            hit = self.find(fam, bold, italic)
            if hit:
                return hit
        faces = self.faces()
        return next(iter(faces.values()), None)
