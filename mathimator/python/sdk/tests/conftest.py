from __future__ import annotations

import pytest

from mathimator import _client


@pytest.fixture(autouse=True)
def fresh_engine():
    """Each test gets its own headless engine, so scenes never leak between tests."""
    c = _client.Client(_client.HeadlessTransport())
    _client.use(c)
    yield c
    c.close()
    _client.use(None)
