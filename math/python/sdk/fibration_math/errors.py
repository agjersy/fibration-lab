"""Exceptions raised by the SDK."""

from __future__ import annotations


class FibrationMathError(Exception):
    """Base class for every SDK error."""


class EngineUnavailable(FibrationMathError):
    """The engine (or the app's router) cannot be reached."""


class ProtocolError(FibrationMathError):
    """The engine answered something the SDK did not expect."""


class MathError(FibrationMathError):
    """An engine call failed. ``kind`` is one of the engine's error kinds (ENGINE.md)."""

    def __init__(self, kind: str, detail: str, hint: str | None = None, fn: str | None = None):
        message = f"{fn}: {detail}" if fn else detail
        if hint:
            message += f" (hint: {hint})"
        super().__init__(message)
        self.kind = kind
        self.detail = detail
        self.hint = hint
        self.fn = fn


class Undecided(MathError):
    """A truth value was needed and the engine could not decide it.

    Raised when code branches on an ``unknown`` Truth, so an undecided fact is never read as
    false by accident. Handle it explicitly: ``t.value is None`` or ``t.decided``.
    """

    def __init__(self, detail: str, hint: str | None = None, fn: str | None = None):
        super().__init__("undecided", detail, hint, fn)


class NoLimit(MathError):
    """The limit does not exist (the one-sided limits differ, or the values oscillate)."""

    def __init__(self, detail: str, fn: str | None = "limit"):
        super().__init__("math", detail, None, fn)
