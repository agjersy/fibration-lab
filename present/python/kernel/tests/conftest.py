from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(Path(__file__).resolve().parent))
# Deterministic font lookup: vendored test fonts first.
os.environ["PRESENT_FONT_DIRS"] = str(REPO / "vendor" / "fonts")

from corpus import make_project  # noqa: E402

from present_kernel.kernel import Kernel  # noqa: E402


@pytest.fixture(scope="session", params=["corp", "plain", "wide-dense"])
def corpus_project(request, tmp_path_factory) -> Path:
    return make_project(tmp_path_factory.mktemp(f"corpus-{request.param}"), request.param)


@pytest.fixture(scope="session")
def corp_project(tmp_path_factory) -> Path:
    return make_project(tmp_path_factory.mktemp("corp"), "corp")


@pytest.fixture()
def kernel_for(tmp_path):
    made = []

    def factory(project: Path) -> Kernel:
        k = Kernel(project=project, home=tmp_path / "home")
        made.append(k)
        return k

    yield factory
    for k in made:
        k.close()


def pytest_addoption(parser):
    parser.addoption("--update-golden", action="store_true", default=False, help="rewrite golden files")
