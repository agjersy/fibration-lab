"""Stdlib-only Matplotlib capture backend injected into user script processes.

See docs/PROTOCOL.md sections 1.6 and 9. Selected with ``MPLBACKEND=module://fibration_mpl``.
It subclasses the Agg backend:

- ``show()`` saves every open figure and closes it. One file per figure, PNG, unless
  ``FIBRATION_FIGURE_FORMATS`` (a comma list of ``png``, ``svg``, ``pdf``) asks for more;
- ``show(block=False)``, ``plt.pause()`` and ``fig.show()`` save without closing, under a
  stem that stays the same for that figure, so a live-update loop ends with its last frame;
- figures still open when the interpreter exits are saved;
- ``savefig()`` works as usual.

Every ``.png``, ``.svg`` or ``.pdf`` written gets a sidecar named ``<file name>.meta.json``
(``residuals.png.meta.json``). A sidecar never replaces a file that is not itself a sidecar.

Imports only the standard library and Matplotlib, and runs on Python 3.9 and later.
"""

from __future__ import annotations

import atexit
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path

from matplotlib import _pylab_helpers
from matplotlib.backend_bases import FigureManagerBase
from matplotlib.backends.backend_agg import FigureCanvasAgg

__all__ = [
    "FigureCanvas",
    "FigureManager",
    "figure_formats",
    "figures_dir",
    "new_figure_manager",
    "show",
    "sidecar_path",
    "write_sidecar",
]

_UTC = timezone.utc  # noqa: UP017 - the UTC alias needs 3.11; session venvs may be 3.9
BACKEND_NAME = "module://fibration_mpl"
SIDECAR_SUFFIX = ".meta.json"
_FIGURE_EXTS = (".png", ".svg", ".pdf")
FORMATS_ENV = "FIBRATION_FIGURE_FORMATS"
DEFAULT_FORMATS = (".png",)
_SIDECAR_KEYS = ("run_id", "script", "args", "title", "axes", "created_at", "width_in", "height_in", "dpi")


def figures_dir() -> Path:
    """``$FIBRATION_OUT_DIR/figures`` or ``./out/figures`` when the variable is unset."""
    base = os.environ.get("FIBRATION_OUT_DIR")
    root = Path(base) if base else Path.cwd() / "out"
    return root / "figures"


def figure_formats() -> tuple[str, ...]:
    """Extensions ``show()`` saves, from ``FIBRATION_FIGURE_FORMATS`` (default: PNG only).

    The kernel sets the variable from ``[figures] formats`` in ``project.toml``. Unknown
    names are ignored; if nothing usable is left the default applies.
    """
    out: list[str] = []
    for part in os.environ.get(FORMATS_ENV, "").split(","):
        ext = "." + part.strip().lower().lstrip(".")
        if ext in _FIGURE_EXTS and ext not in out:
            out.append(ext)
    return tuple(out) or DEFAULT_FORMATS


def slugify(text: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")
    return slug[:80]


def _suptitle(fig) -> str | None:
    getter = getattr(fig, "get_suptitle", None)
    text = getter() if callable(getter) else (fig._suptitle.get_text() if getattr(fig, "_suptitle", None) else "")
    return text or None


def figure_title(fig) -> str | None:
    title = _suptitle(fig)
    if title:
        return title
    if fig.axes:
        t = fig.axes[0].get_title()
        if t:
            return t
    return None


def _script_name() -> str:
    argv0 = sys.argv[0] if sys.argv else ""
    root = os.environ.get("FIBRATION_ROOT")
    if argv0 and root:
        try:
            return Path(argv0).resolve().relative_to(Path(root).resolve()).as_posix()
        except (ValueError, OSError):
            pass
    return argv0


def sidecar_for(fig) -> dict:
    axes = []
    for ax in fig.axes:
        axes.append(
            {
                "title": ax.get_title() or None,
                "xlabel": ax.get_xlabel() or None,
                "ylabel": ax.get_ylabel() or None,
            }
        )
    w, h = fig.get_size_inches()
    return {
        "run_id": os.environ.get("FIBRATION_RUN_ID"),
        "script": _script_name(),
        "args": [str(a) for a in sys.argv[1:]],
        "title": figure_title(fig),
        "axes": axes,
        "created_at": datetime.now(_UTC).isoformat(timespec="milliseconds").replace("+00:00", "Z"),
        "width_in": float(w),
        "height_in": float(h),
        "dpi": float(fig.dpi),
    }


def sidecar_path(image_path) -> Path:
    """``residuals.png`` -> ``residuals.png.meta.json``."""
    image_path = Path(image_path)
    return image_path.with_name(image_path.name + SIDECAR_SUFFIX)


def _is_sidecar_file(path: Path) -> bool:
    """True when ``path`` holds a sidecar written by this module (so it may be replaced)."""
    try:
        if path.stat().st_size > 1_000_000:
            return False
        obj = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    return isinstance(obj, dict) and all(k in obj for k in _SIDECAR_KEYS) and isinstance(obj.get("axes"), list)


def write_sidecar(fig, image_path) -> Path | None:
    """Write the sidecar for a saved figure file; None when no sidecar applies.

    Only ``.png``, ``.svg`` and ``.pdf`` get one, and an existing file under the sidecar's
    name is left alone unless it is itself a sidecar: the user's data always wins.
    """
    image_path = Path(image_path)
    if image_path.suffix.lower() not in _FIGURE_EXTS:
        return None
    path = sidecar_path(image_path)
    if path.exists() and not _is_sidecar_file(path):
        return None
    tmp = path.with_name(f"{path.name}.{os.getpid()}.tmp")
    tmp.write_text(json.dumps(sidecar_for(fig), indent=2) + "\n", encoding="utf-8")
    os.replace(tmp, path)
    return path


_used_stems: set[str] = set()


def unique_stem(directory: Path, stem: str) -> str:
    """De-duplicate with ``-2``, ``-3`` ... against stems used in this process and files on disk."""

    def taken(name: str) -> bool:
        if name in _used_stems:
            return True
        return any(
            (directory / f"{name}{ext}").exists() or (directory / f"{name}{ext}{SIDECAR_SUFFIX}").exists()
            for ext in _FIGURE_EXTS
        )

    if not taken(stem):
        return stem
    n = 2
    while taken(f"{stem}-{n}"):
        n += 1
    return f"{stem}-{n}"


_counter = 0


def _next_number() -> int:
    """Untitled figures are numbered 1, 2, ... in the order they are first saved, for the
    whole process. ``Figure.number`` restarts after ``show()`` closes figures, so it is not used."""
    global _counter
    _counter += 1
    return _counter


def _stem_for(fig, directory: Path) -> str:
    """The figure's stem, assigned at its first save and stable afterwards."""
    stem = getattr(fig, "_fibration_stem", None)
    if stem:
        return stem
    title = figure_title(fig)
    stem = slugify(title) if title else ""
    if not stem:
        stem = f"fig-{_next_number()}"
        while stem in _used_stems:
            stem = f"fig-{_next_number()}"
    stem = unique_stem(directory, stem)
    _used_stems.add(stem)
    fig._fibration_stem = stem
    return stem


def save_figure(fig) -> list[Path]:
    """Save one figure in each configured format (PNG by default) with a sidecar per file.

    Returns the written image paths.

    Saving the same figure again overwrites its earlier files.
    """
    directory = figures_dir()
    directory.mkdir(parents=True, exist_ok=True)
    stem = _stem_for(fig, directory)
    written: list[Path] = []
    canvas = fig.canvas
    for ext in figure_formats():
        target = directory / f"{stem}{ext}"
        FigureCanvasAgg.print_figure(canvas, str(target))
        write_sidecar(fig, target)
        written.append(target)
    fig._fibration_captured = True
    return written


class FigureCanvas(FigureCanvasAgg):
    """Agg canvas whose ``print_figure`` (used by ``savefig``) also writes the sidecar."""

    def print_figure(self, filename, *args, **kwargs):
        result = super().print_figure(filename, *args, **kwargs)
        if isinstance(filename, (str, os.PathLike)):
            self.figure._fibration_explicit = True
            try:
                write_sidecar(self.figure, Path(os.fspath(filename)))
            except OSError:
                pass
        return result


class FigureManager(FigureManagerBase):
    def show(self):  # fig.show() on a single figure: save it, keep it open
        save_figure(self.canvas.figure)


FigureCanvas.manager_class = FigureManager


def new_figure_manager(num, *args, **kwargs):
    from matplotlib.figure import Figure

    FigureClass = kwargs.pop("FigureClass", Figure)
    fig = FigureClass(*args, **kwargs)
    return new_figure_manager_given_figure(num, fig)


def new_figure_manager_given_figure(num, figure):
    canvas = FigureCanvas(figure)
    return FigureManager(canvas, num)


def show(*args, **kwargs):
    """Save every open figure (and its sidecar) under the figures directory.

    A blocking ``show()`` then closes the figures, as an interactive backend would once its
    windows are closed. ``show(block=False)`` and ``plt.pause()`` leave them open so the
    script can keep drawing; later saves replace the same files.
    """
    block = kwargs.get("block", args[0] if args else None)
    managers = list(_pylab_helpers.Gcf.get_all_fig_managers())
    for manager in managers:
        save_figure(manager.canvas.figure)
    if block is False:
        return
    for manager in managers:
        _pylab_helpers.Gcf.destroy(manager)


def draw_if_interactive():
    return None


def _current_backend() -> str | None:
    try:
        import matplotlib

        getter = getattr(matplotlib.rcParams, "_get_backend_or_none", None)
        name = getter() if callable(getter) else dict.__getitem__(matplotlib.rcParams, "backend")
        return name if isinstance(name, str) else None
    except Exception:
        return None


def _at_exit() -> None:
    """Save figures still open at interpreter exit, and say so when capture was switched off."""
    try:
        backend = _current_backend()
        if backend and backend.lower() != BACKEND_NAME:
            sys.stderr.write(
                f"fibration: the script switched the Matplotlib backend to {backend!r} "
                "(matplotlib.use / plt.switch_backend), so plt.show() figures after that were not "
                "captured. Remove the call to let the app collect figures.\n"
            )
            return
        for manager in list(_pylab_helpers.Gcf.get_all_fig_managers()):
            fig = manager.canvas.figure
            if not isinstance(manager.canvas, FigureCanvas):
                continue
            # A figure that was only written with savefig() already has its file; one that was
            # shown without blocking is saved again so the file holds its final state.
            if getattr(fig, "_fibration_explicit", False) and not getattr(fig, "_fibration_captured", False):
                continue
            if not fig.axes and not getattr(fig, "_fibration_captured", False):
                continue  # an empty figure nobody drew on
            try:
                save_figure(fig)
            except Exception as exc:  # never turn a finished script into a failure
                sys.stderr.write(f"fibration: could not save an open figure at exit: {exc}\n")
    except Exception:
        pass


atexit.register(_at_exit)
