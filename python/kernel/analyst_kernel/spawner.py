"""Starts the kernel's child processes (the REPL, the equation worker) from one long-lived thread.

``bwrap --die-with-parent`` (and ``PR_SET_PDEATHSIG`` in general) kills a child when the
*thread* that forked it exits, not the process. The children are often started by short-lived
threads (the equation prewarm, a request on a pool thread), so the fork happens on this thread,
which lives as long as the kernel process.
"""

from __future__ import annotations

import threading
from collections.abc import Callable
from typing import Any, TypeVar

T = TypeVar("T")


class Spawner:
    def __init__(self) -> None:
        self._jobs: list[tuple[Callable[[], Any], threading.Event, list[Any]]] = []
        self._cond = threading.Condition()
        self._thread = threading.Thread(target=self._loop, name="child-spawner", daemon=True)
        self._thread.start()

    def _loop(self) -> None:
        while True:
            with self._cond:
                while not self._jobs:
                    self._cond.wait()
                fn, done, box = self._jobs.pop(0)
            try:
                box.append((True, fn()))
            except BaseException as exc:  # handed back to the caller
                box.append((False, exc))
            done.set()

    def call(self, fn: Callable[[], T]) -> T:
        """Runs ``fn`` (a ``subprocess.Popen``) on the spawner thread and returns its result."""
        if threading.current_thread() is self._thread:
            return fn()
        done = threading.Event()
        box: list[Any] = []
        with self._cond:
            self._jobs.append((fn, done, box))
            self._cond.notify()
        done.wait()
        ok, value = box[0]
        if not ok:
            raise value
        return value


_spawner: Spawner | None = None
_lock = threading.Lock()


def spawn(fn: Callable[[], T]) -> T:
    """``fn()`` run on the process-wide spawner thread (started on first use)."""
    global _spawner
    with _lock:
        if _spawner is None:
            _spawner = Spawner()
        spawner = _spawner
    return spawner.call(fn)
