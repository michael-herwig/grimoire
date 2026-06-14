# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Multi-registry acceptance tests (`[[registries]]` config table).

These tests exercise three distinct behaviors introduced by the multi-registry
feature (ADR: adr_multi_registry_mcp.md):

1. ``grim search`` (no --registry flag) browses ALL declared ``[[registries]]``
   entries in the project config.
2. ``grim add alias/repo:tag`` expands a qualified alias reference against the
   configured registry URL and persists the fully-qualified name.
3. A project using only the legacy ``[options].default_registry`` (no
   ``[[registries]]``) still works correctly on a cold cache — backward
   compatibility guard.

Registry simulation strategy
-----------------------------
The acceptance suite runs against a single shared ``localhost:5000`` registry.
To simulate two independent registries we use two DISTINCT NAMESPACE prefixes
on the same host and declare them as two ``[[registries]]`` entries:

    [[registries]]
    alias = "ns1"
    url = "localhost:5000/<namespace1>"

    [[registries]]
    alias = "ns2"
    url = "localhost:5000/<namespace2>"

This mirrors real multi-registry usage (namespaced orgs on ghcr.io, etc.) and
is the recommended pattern in ``test_search_namespaced.py``.
"""
from __future__ import annotations

import json
import uuid
from pathlib import Path

import pytest

from src.helpers import make_artifact
from src.registry import REGISTRY_HOST


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _two_namespace_config(project_dir: Path, ns1: str, ns2: str) -> None:
    """Write a grimoire.toml with two ``[[registries]]`` entries (two namespaces)."""
    text = (
        f'[[registries]]\n'
        f'alias = "reg1"\n'
        f'url = "{REGISTRY_HOST}/{ns1}"\n'
        f'default = true\n'
        f'\n'
        f'[[registries]]\n'
        f'alias = "reg2"\n'
        f'url = "{REGISTRY_HOST}/{ns2}"\n'
        f'\n'
        f'[skills]\n'
        f'\n'
        f'[rules]\n'
    )
    (project_dir / "grimoire.toml").write_text(text)


# ---------------------------------------------------------------------------
# Test 1 — multi-registry default browse
# ---------------------------------------------------------------------------


def test_search_multi_registry_browses_all_declared(
    grim_at, project_dir: Path, registry: str
) -> None:
    """``grim search`` (no --registry flag) must browse ALL declared
    ``[[registries]]`` entries in grimoire.toml and surface packages from each.

    Implementation note: the shared localhost:5000 registry distinguishes
    registries by namespace prefix. We declare two namespaces as two
    ``[[registries]]`` entries, publish one artifact in each, then assert both
    appear in the search results.
    """
    # Use distinct unique segments for each namespace to avoid cross-test
    # collisions on the shared session-scoped registry.
    ns1 = f"grim-test/{uuid.uuid4().hex[:12]}"
    ns2 = f"grim-test/{uuid.uuid4().hex[:12]}"

    # Publish one artifact under each namespace.
    make_artifact(
        f"{ns1}/skill-in-ns1",
        "skill",
        {"skill-in-ns1/SKILL.md": "---\nname: skill-in-ns1\ndescription: from ns1\n---\n# S1\n"},
        tag="latest",
        annotations={
            "org.opencontainers.image.description": "Skill from namespace 1",
        },
    )
    make_artifact(
        f"{ns2}/rule-in-ns2",
        "rule",
        {"rule-in-ns2.md": "---\npaths: ['**/*.rs']\n---\n# R2\n"},
        tag="latest",
        annotations={
            "org.opencontainers.image.description": "Rule from namespace 2",
        },
    )

    _two_namespace_config(project_dir, ns1, ns2)
    runner = grim_at(project_dir)

    # Run grim search WITHOUT --registry so it uses the declared [[registries]].
    # --refresh forces a catalog rebuild from both registries.
    result = runner.run("--format", "json", "search", "--refresh", check=False)
    assert result.returncode == 0, (
        f"multi-registry search must exit 0, got {result.returncode}; "
        f"stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)
    assert isinstance(rows, list), f"search must return a JSON array, got {rows!r}"

    repos = [r.get("repo", "") for r in rows]

    assert any("skill-in-ns1" in repo for repo in repos), (
        f"search must surface the artifact from namespace 1 (reg1), "
        f"but got repos: {repos}"
    )
    assert any("rule-in-ns2" in repo for repo in repos), (
        f"search must surface the artifact from namespace 2 (reg2), "
        f"but got repos: {repos}"
    )


# ---------------------------------------------------------------------------
# Test 2 — qualified alias reference resolves via [[registries]] alias
# ---------------------------------------------------------------------------


def test_add_qualified_alias_reference_resolves(
    grim_at, project_dir: Path, registry: str
) -> None:
    """``grim add alias/repo:tag`` resolves the alias to its configured URL.

    With a ``[[registries]]`` entry ``alias="reg1", url="localhost:5000/<ns>"``,
    the short form ``reg1/<repo>:<tag>`` must expand to
    ``localhost:5000/<ns>/<repo>:<tag>`` and that fully-qualified name must
    appear in both grimoire.toml and grimoire.lock.
    """
    ns1 = f"grim-test/{uuid.uuid4().hex[:12]}"

    art = make_artifact(
        f"{ns1}/my-tool",
        "skill",
        {"my-tool/SKILL.md": "---\nname: my-tool\ndescription: d\n---\n# T\n"},
        tag="v1",
    )

    # Config declares one registry with alias "reg1".
    cfg_text = (
        f'[[registries]]\n'
        f'alias = "reg1"\n'
        f'url = "{REGISTRY_HOST}/{ns1}"\n'
        f'default = true\n'
        f'\n'
        f'[skills]\n'
        f'\n'
        f'[rules]\n'
    )
    (project_dir / "grimoire.toml").write_text(cfg_text)
    runner = grim_at(project_dir)

    # Use the qualified alias/repo:tag form — the leading segment "reg1" is
    # the alias; grim must substitute the configured URL.
    qualified_ref = f"reg1/my-tool:v1"
    out = runner.json("add", qualified_ref)

    assert out["status"] == "added", f"add must report 'added', got {out!r}"
    assert out["kind"] == "skill", f"kind must be 'skill', got {out!r}"

    # The alias must be EXPANDED to the full path, not persisted: the
    # qualified form `reg1/my-tool` must appear nowhere in the config (the
    # `alias = "reg1"` declaration line is unaffected — it has no `/my-tool`),
    # and the full expanded path must be present.
    cfg = (project_dir / "grimoire.toml").read_text()
    assert "reg1/my-tool" not in cfg, (
        f"the alias-qualified form 'reg1/my-tool' must be expanded, not stored; got:\n{cfg}"
    )
    assert f"{REGISTRY_HOST}/{ns1}/my-tool" in cfg, (
        f"grimoire.toml must carry the expanded path "
        f"'{REGISTRY_HOST}/{ns1}/my-tool'; got:\n{cfg}"
    )

    lock = (project_dir / "grimoire.lock").read_text()
    assert REGISTRY_HOST in lock, (
        f"grimoire.lock must contain the registry host '{REGISTRY_HOST}'; "
        f"got:\n{lock}"
    )
    assert f"{REGISTRY_HOST}/{ns1}/my-tool" in lock, (
        f"lock must carry the full expanded path '{REGISTRY_HOST}/{ns1}/my-tool'; "
        f"got:\n{lock}"
    )


# ---------------------------------------------------------------------------
# Test 3 — legacy single default_registry cold-cache backward compat
# ---------------------------------------------------------------------------


def test_search_single_default_registry_cold_cache(
    grim_at, project_dir: Path, registry: str
) -> None:
    """A project using only ``[options].default_registry`` (no ``[[registries]]``)
    must still work on a cold cache.

    This guards the legacy path: when no ``[[registries]]`` is declared,
    grim falls back to the single-registry resolve chain (project config
    ``[options].default_registry`` > GRIM_DEFAULT_REGISTRY > built-in
    fallback). The test uses a fresh per-test GRIM_HOME (cold cache) and
    asserts exit 0 + valid JSON array — the same behavioral contract as
    existing search tests.
    """
    ns = f"grim-test/{uuid.uuid4().hex[:12]}"
    make_artifact(
        f"{ns}/legacy-skill",
        "skill",
        {"legacy-skill/SKILL.md": "---\nname: legacy-skill\ndescription: legacy\n---\n# L\n"},
        tag="latest",
        annotations={
            "org.opencontainers.image.description": "Legacy single-registry skill",
        },
    )

    # Legacy config: no [[registries]], only [options].default_registry.
    cfg_text = (
        f'[options]\n'
        f'default_registry = "{REGISTRY_HOST}/{ns}"\n'
        f'\n'
        f'[skills]\n'
        f'\n'
        f'[rules]\n'
    )
    (project_dir / "grimoire.toml").write_text(cfg_text)

    runner = grim_at(project_dir)
    # grim_home is from tmp_path so this is always a cold cache.
    # Do NOT set GRIM_DEFAULT_REGISTRY — use only the config default.
    runner.env.pop("GRIM_DEFAULT_REGISTRY", None)

    # --refresh forces a real registry walk even from a cold cache.
    result = runner.run("--format", "json", "search", "--refresh", check=False)
    assert result.returncode == 0, (
        f"legacy single-registry search must exit 0 on cold cache, "
        f"got {result.returncode}; stderr: {result.stderr}"
    )
    arr = json.loads(result.stdout)
    assert isinstance(arr, list), f"search must return a JSON array, got {arr!r}"

    # The scoped namespace was used as the default_registry, so the skill
    # published there must appear in results.
    repos = [r.get("repo", "") for r in arr]
    assert any("legacy-skill" in repo for repo in repos), (
        f"cold-cache search with legacy default_registry must find 'legacy-skill', "
        f"got repos: {repos}"
    )
