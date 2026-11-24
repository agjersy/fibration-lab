#!/usr/bin/env python3
"""Drive the real desktop app (through tauri-driver) against a real repo and kernel.

Flow: launch -> new session on --repo -> kernel ready -> Scripts view -> run decay_sim.py
-> figure tab and caption appear -> screenshot -> close the session -> reopen it -> kernel
ready again, the earlier run is listed and its figure and caption are shown.
Exits non-zero on any failure.

Usage: smoke.py --app <fibration-desktop binary> --repo <absolute repo path> [--out dir]
Requires tauri-driver listening on --driver (default http://127.0.0.1:4444).
"""

from __future__ import annotations

import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from webdriver import Driver  # noqa: E402


def click_tab(d: Driver, label: str, timeout: float = 20.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        for el in d.find_all(".dv-tab"):
            try:
                if label in d._s(f"/element/{el}/text"):
                    d._s(f"/element/{el}/click", "POST", {})
                    return
            except RuntimeError:
                pass
        time.sleep(0.3)
    raise TimeoutError(f"no dock tab labelled {label!r}")


def wait_gone(d: Driver, css: str, timeout: float = 30.0) -> None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        if not d.find_all(css):
            return
        time.sleep(0.3)
    raise TimeoutError(f"{css!r} is still present")


def wait_caption(d: Driver, timeout: float = 60.0) -> str:
    """The caption under the shown figure: `<name> · <script> <args>` from the sidecar."""
    deadline = time.time() + timeout
    caption = ""
    while time.time() < deadline:
        caption = d.execute("return (document.querySelector('.figure-caption') || {}).textContent || ''") or ""
        if "decay_sim.py" in caption and "--tau" not in caption:
            return caption.strip()
        time.sleep(0.5)
    raise AssertionError(f"the figure caption did not appear or is not from this run: {caption!r}")


def page_errors(d: Driver) -> list:
    # main.tsx installs the collector (uncaught errors and unhandled rejections). A missing
    # collector is a failure too: "no errors" must not be vacuous.
    errors = d.execute("return Array.isArray(window.__fibrationErrors) ? window.__fibrationErrors : null")
    assert errors is not None, "window.__fibrationErrors is not defined: the app's error collector did not load"
    return errors


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--app", required=True)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--driver", default="http://127.0.0.1:4444")
    ap.add_argument("--out", default=".")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    d = Driver(a.driver)
    d.start(a.app)
    try:
        t0 = time.time()
        title = d.title()
        print(f"[{time.time()-t0:5.1f}s] title: {title!r}")
        assert title == "Fibration Lab", title
        d.find("[data-testid=topbar]")
        d.click("[data-testid=new-session]")
        d.type("#ns-repo", a.repo)
        d.click("[data-testid=create-session]")
        d.find("[data-testid=session-tab]", 30)
        print(f"[{time.time()-t0:5.1f}s] session created")
        d.find(".dot.ready", 90)
        print(f"[{time.time()-t0:5.1f}s] kernel ready")
        click_tab(d, "Scripts")
        d.click('[data-script="decay_sim"] .script-header')
        d.click('[data-script="decay_sim"] button.primary', 30)
        print(f"[{time.time()-t0:5.1f}s] run started")
        click_tab(d, "Runs")
        status = d.wait_text(".list-item .status-done", "exit", 180)
        print(f"[{time.time()-t0:5.1f}s] run finished: {status}")
        assert status.strip() == "exit 0", f"run did not exit 0: {status!r}"
        d.wait_count("[data-testid=figure-tab]", 1, 60)
        d.find("[data-testid=figure-frame] img", 30)
        # the new run had no arguments; the latest figure caption must not show older args
        caption = wait_caption(d)
        print(f"[{time.time()-t0:5.1f}s] figure from this run rendered, caption: {caption}")
        alt = d.execute("return (document.querySelector('[data-testid=figure-frame] img') || {}).alt || ''")
        assert alt, "the figure has no title or name"
        d.screenshot(os.path.join(a.out, "smoke-workbench.png"))
        errors = page_errors(d)
        print(f"[{time.time()-t0:5.1f}s] screenshot saved; page errors: {len(errors)}")
        assert errors == [], "page errors:\n" + "\n".join(str(e) for e in errors)

        # Close the session and reopen it (REVIEW B2): open -> subscribe -> session_info.
        d.click("[data-testid=session-tab] .close")
        wait_gone(d, "[data-testid=session-tab] .close")
        print(f"[{time.time()-t0:5.1f}s] session closed")
        d.click("[data-testid=session-tab]")
        d.find("[data-testid=session-tab] .close", 30)
        d.find(".dot.ready", 90)
        print(f"[{time.time()-t0:5.1f}s] session reopened, kernel ready")
        click_tab(d, "Runs")
        status = d.wait_text(".list-item .status-done", "exit", 60)
        assert status.strip() == "exit 0", f"the earlier run is not listed as finished: {status!r}"
        print(f"[{time.time()-t0:5.1f}s] earlier run listed after reopen: {status}")
        d.wait_count("[data-testid=figure-tab]", 1, 60)
        d.find("[data-testid=figure-frame] img", 30)
        caption = wait_caption(d)
        print(f"[{time.time()-t0:5.1f}s] figure and caption after reopen: {caption}")
        click_tab(d, "Scripts")
        d.find('[data-script="decay_sim"]', 60)
        print(f"[{time.time()-t0:5.1f}s] index loaded after reopen")
        d.screenshot(os.path.join(a.out, "smoke-reopened.png"))
        errors = page_errors(d)
        print(f"[{time.time()-t0:5.1f}s] page errors after reopen: {len(errors)}")
        assert errors == [], "page errors:\n" + "\n".join(str(e) for e in errors)
        return 0
    except Exception as e:  # noqa: BLE001
        try:
            d.screenshot(os.path.join(a.out, "smoke-failure.png"))
        except Exception:  # noqa: BLE001
            pass
        print(f"SMOKE FAILED: {e}", file=sys.stderr)
        return 1
    finally:
        d.quit()


if __name__ == "__main__":
    sys.exit(main())
