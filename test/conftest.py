"""Shared fixtures for the Grimoire acceptance-test suite."""
from __future__ import annotations

import os
import sys
from pathlib import Path

import pytest

from src.helpers import PROJECT_ROOT
from src.runner import GrimRunner

# ---------------------------------------------------------------------------
# Session-scoped fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def grim_binary() -> Path:
    if env_path := os.environ.get("GRIM_COMMAND"):
        p = Path(env_path)
    else:
        p = PROJECT_ROOT / "test" / "bin" / "grim"
        if sys.platform == "win32" and not p.suffix:
            p = p.with_suffix(".exe")
    assert p.exists(), f"grim binary not found at {p}"
    return p


# ---------------------------------------------------------------------------
# Function-scoped fixtures
# ---------------------------------------------------------------------------


@pytest.fixture()
def grim_home(tmp_path: Path) -> Path:
    home = tmp_path / "grim-home"
    home.mkdir()
    return home


@pytest.fixture()
def grim(grim_binary: Path, grim_home: Path) -> GrimRunner:
    return GrimRunner(grim_binary, grim_home)
