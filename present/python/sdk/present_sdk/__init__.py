"""Deck SDK for Personal Present (PROTOCOL.md section 5)."""

from pptx.util import Emu, Inches, Pt

from . import theme
from .content import ContentError
from .deck import Deck, Placeholder, Slide, alt, cite

__version__ = "0.0.1"
SCHEMA_VERSION = 1

__all__ = ["SCHEMA_VERSION", "ContentError", "Deck", "Emu", "Inches", "Placeholder", "Pt", "Slide", "alt", "cite", "theme"]
