"""Wall-clock budgets for engine calls (ARCHITECTURE 5.2).

The engine serves one call at a time on its main thread, so an interval timer can interrupt
SymPy in the middle of a computation. The exception is a ``BaseException`` so library code
that catches ``Exception`` cannot swallow it (the same technique as the Analyst kernel's
``eqcore.simplify_with_budget``). Off the main thread (tests that call the engine from a
worker) the budget cannot be enforced and the call simply runs.
"""

from __future__ import annotations

import contextlib
import signal
import threading
from collections.abc import Iterator

from .errors import CallError


class BudgetExceeded(BaseException):
    pass


def _enforceable() -> bool:
    return threading.current_thread() is threading.main_thread() and hasattr(signal, "setitimer")


@contextlib.contextmanager
def budget(seconds: float) -> Iterator[None]:
    if not _enforceable() or seconds <= 0:
        yield
        return

    def on_alarm(signum, frame):
        raise BudgetExceeded()

    previous = signal.signal(signal.SIGALRM, on_alarm)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        yield
    except BudgetExceeded:
        raise CallError(
            "timeout",
            f"the call did not finish within its budget of {seconds:g} s",
            hint="simplify the expression, narrow the interval, or ask for a numeric result",
        ) from None
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)

