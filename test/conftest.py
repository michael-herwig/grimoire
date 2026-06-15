# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Shared fixtures for the Grimoire acceptance-test suite."""
from __future__ import annotations

import os
import subprocess
import sys
import time
import uuid
from pathlib import Path

import pytest

from src.helpers import PROJECT_ROOT
from src.registry import REGISTRY_HOST, registry_reachable
from src.runner import GrimRunner

# ---------------------------------------------------------------------------
# Session-scoped fixtures
# ---------------------------------------------------------------------------

_REGISTRY_CONTAINER = "grim-acceptance-registry"


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


@pytest.fixture(scope="session")
def registry() -> str:
    """A reachable ``registry:2`` on ``localhost:5000``.

    Reuses an already-running registry (the common CI / dev setup); if none
    answers, starts a throwaway container. The repository namespace is
    isolated per test via the ``unique_repo`` fixture, so a shared registry
    is safe.

    **xdist-safe.** Under ``-n auto`` this session fixture runs once *per
    worker*. Two design choices keep concurrent workers from fighting over
    the single shared container:

    1. A lost ``docker run --name`` race (another worker already created the
       container — "name already in use") is not fatal: fall through to the
       readiness wait instead of skipping, so every worker ends up using the
       one registry whoever-won started.
    2. The container is deliberately **not** torn down at session end. With a
       per-worker ``docker rm -f`` the first worker to finish tore the shared
       container down while sibling workers were still pushing, surfacing as
       flaky ``Connection refused``. The throwaway ``registry:2`` on ``:5000``
       is instead reused by the next run via the ``registry_reachable()``
       fast path (stop it by hand with ``docker rm -f`` when done, or let CI
       discard the runner).
    """
    if registry_reachable():
        yield REGISTRY_HOST
        return

    started = subprocess.run(
        [
            "docker", "run", "-d", "--rm",
            "--name", _REGISTRY_CONTAINER,
            "-p", "5000:5000",
            "registry:2",
        ],
        capture_output=True,
        text=True,
    )
    # A name-in-use failure means a sibling xdist worker won the start race;
    # that is expected, not an error — wait for its container to come up.
    name_in_use = "already in use" in (started.stderr or "").lower()
    if started.returncode != 0 and not name_in_use:
        pytest.skip(
            f"no registry reachable and could not start one: "
            f"{started.stderr.strip()}"
        )

    deadline = time.time() + 30
    while time.time() < deadline:
        if registry_reachable():
            break
        time.sleep(0.5)
    else:
        pytest.skip("registry container did not become ready in time")

    yield REGISTRY_HOST


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


@pytest.fixture()
def unique_repo() -> str:
    """A UUID-prefixed repository name, isolated per test on the shared
    registry."""
    return f"grim-test/{uuid.uuid4().hex[:12]}"
