"""``python -m present_kernel.fidelity.ppt_win <in.pptx> <out.pdf>``: export through PowerPoint COM.

Runs in its own process so the kernel can enforce a timeout; quits PowerPoint only when this
process started it.
"""

from __future__ import annotations

import sys

PP_SAVE_AS_PDF = 32


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print("usage: ppt_win <in.pptx> <out.pdf>", file=sys.stderr)
        return 2
    try:
        import pythoncom  # type: ignore[import-not-found]
        import win32com.client  # type: ignore[import-not-found]
    except ImportError as exc:
        print(f"pywin32 is not available: {exc}", file=sys.stderr)
        return 3
    pythoncom.CoInitialize()
    app = None
    started = False
    try:
        try:
            app = win32com.client.GetActiveObject("PowerPoint.Application")
        except Exception:
            app = win32com.client.Dispatch("PowerPoint.Application")
            started = True
        pres = app.Presentations.Open(argv[1], ReadOnly=True, Untitled=False, WithWindow=False)
        try:
            pres.SaveAs(argv[2], PP_SAVE_AS_PDF)
        finally:
            pres.Close()
        return 0
    except Exception as exc:
        print(f"PowerPoint automation failed: {exc}", file=sys.stderr)
        return 1
    finally:
        if app is not None and started:
            try:
                app.Quit()
            except Exception:
                pass
        pythoncom.CoUninitialize()


if __name__ == "__main__":
    sys.exit(main(sys.argv))
