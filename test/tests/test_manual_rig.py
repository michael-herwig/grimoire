# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Integrity of the manual rolling-release rig.

Two regression surfaces:

1. **Script mode bits.** The rig scripts under ``test/manual/scripts/``
   are documented as *directly invocable* (each carries a
   ``#   test/manual/scripts/<name>.sh`` usage header). If one is
   committed without its execute bit the documented rolling-release
   reproduction silently no-ops with exit 126 ("permission denied") —
   ``grim release`` never runs, the registry keeps the old cascade, and
   the rolling-release feature *appears* broken when it is not.

2. **Catalog validity.** ``bootstrap.sh`` publishes every artifact under
   ``test/manual/catalog/`` with ``grim release``. The catalog sources
   are only exercised when someone runs the rig by hand, so a change to
   frontmatter validation, vendor-key registries, or the bundle schema
   can silently break the rig. Building each catalog artifact in CI
   pins them to the current validation rules.
"""
from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

from src.helpers import PROJECT_ROOT

_RIG_SCRIPTS_DIR = PROJECT_ROOT / "test" / "manual" / "scripts"
_RIG_CATALOG_DIR = PROJECT_ROOT / "test" / "manual" / "catalog"

# Driver scripts a user (or the documented repro) invokes directly. ``env.sh``
# is sourced, not executed, so it is exempt from the execute-bit contract.
_DRIVER_SCRIPTS = ("bootstrap.sh", "release-update.sh", "teardown.sh")


@pytest.mark.parametrize("name", _DRIVER_SCRIPTS)
def test_rig_driver_script_is_executable_on_disk(name: str) -> None:
    script = _RIG_SCRIPTS_DIR / name
    assert script.is_file(), f"missing rig script {script}"
    assert os.access(script, os.X_OK), (
        f"{script} is not executable; the documented reproduction invokes "
        f"it directly and would fail with exit 126 (permission denied), "
        f"making the rolling-release feature appear broken"
    )


@pytest.mark.parametrize("name", _DRIVER_SCRIPTS)
def test_rig_driver_script_is_executable_in_git(name: str) -> None:
    rel = f"test/manual/scripts/{name}"
    out = subprocess.run(
        ["git", "ls-files", "-s", rel],
        cwd=PROJECT_ROOT,
        capture_output=True,
        text=True,
    )
    if out.returncode != 0 or not out.stdout.strip():
        pytest.skip(f"{rel} not tracked by git in this checkout")
    mode = out.stdout.split()[0]
    assert mode == "100755", (
        f"{rel} is committed with git mode {mode}; rig driver scripts "
        f"must be committed executable (100755) so a fresh checkout can "
        f"run the documented rolling-release reproduction"
    )


# ---------------------------------------------------------------------------
# Catalog validity — every artifact bootstrap.sh publishes must build
# ---------------------------------------------------------------------------


def _catalog_artifacts() -> list[pytest.param]:
    """Every artifact path the rig publishes, with its forced kind.

    Mirrors the publish matrix in ``scripts/bootstrap.sh``: skills are
    ``skills/<name>/`` directories, rules are top-level ``rules/*.md``
    (support-directory files are packed with their index, never built
    alone), agents are ``agents/*.md`` and need the explicit
    ``--kind agent``, bundles are ``bundles/*.toml``.
    """
    skills = sorted(p.parent for p in _RIG_CATALOG_DIR.glob("skills/*/SKILL.md"))
    rules = sorted(_RIG_CATALOG_DIR.glob("rules/*.md"))
    agents = sorted(_RIG_CATALOG_DIR.glob("agents/*.md"))
    bundles = sorted(_RIG_CATALOG_DIR.glob("bundles/*.toml"))
    params = [
        pytest.param(p, "skill", None, id=f"skill:{p.name}") for p in skills
    ]
    params += [pytest.param(p, "rule", None, id=f"rule:{p.stem}") for p in rules]
    params += [
        pytest.param(p, "agent", "agent", id=f"agent:{p.stem}") for p in agents
    ]
    params += [
        pytest.param(p, "bundle", None, id=f"bundle:{p.stem}") for p in bundles
    ]
    return params


def test_rig_catalog_is_not_empty() -> None:
    """The parametrization below must never silently collapse to zero."""
    assert len(_catalog_artifacts()) >= 4, (
        "expected at least one artifact per kind under test/manual/catalog"
    )


@pytest.mark.parametrize(("path", "kind", "forced_kind"), _catalog_artifacts())
def test_rig_catalog_artifact_builds(
    grim_at, tmp_path: Path, path: Path, kind: str, forced_kind: str | None
) -> None:
    """`grim build` accepts every artifact the rig's bootstrap publishes."""
    runner = grim_at(tmp_path)
    args = ["build", str(path)]
    if forced_kind is not None:
        args += ["--kind", forced_kind]
    out = runner.json(*args)
    assert out["status"] == "built", f"{path} no longer builds: {out}"
    assert out["kind"] == kind
