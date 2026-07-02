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
    oci = "localhost:5000/<namespace1>"

    [[registries]]
    alias = "ns2"
    oci = "localhost:5000/<namespace2>"

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
        f'oci = "{REGISTRY_HOST}/{ns1}"\n'
        f'default = true\n'
        f'\n'
        f'[[registries]]\n'
        f'alias = "reg2"\n'
        f'oci = "{REGISTRY_HOST}/{ns2}"\n'
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


@pytest.mark.parametrize("style", ["comma", "repeat"])
def test_search_multi_registry_flag_browses_all(
    grim_at, project_dir: Path, registry: str, style: str
) -> None:
    """``--registry`` accepts several registries — comma-separated
    (``--registry a,b``) or repeated (``--registry a --registry b``) — and
    browses all of them at once, overriding any configured ``[[registries]]``.

    Two namespaces simulate two registries (same pattern as the config test).
    The project config declares only the FIRST namespace, so if the flag were
    single-valued the second namespace's artifact would be missing — the test
    proves the flag spans both.
    """
    ns1 = f"grim-test/{uuid.uuid4().hex[:12]}"
    ns2 = f"grim-test/{uuid.uuid4().hex[:12]}"

    make_artifact(
        f"{ns1}/flag-skill-ns1",
        "skill",
        {"flag-skill-ns1/SKILL.md": "---\nname: flag-skill-ns1\ndescription: from ns1\n---\n# S1\n"},
        tag="latest",
        annotations={"org.opencontainers.image.description": "Flag skill ns1"},
    )
    make_artifact(
        f"{ns2}/flag-rule-ns2",
        "rule",
        {"flag-rule-ns2.md": "---\npaths: ['**/*.rs']\n---\n# R2\n"},
        tag="latest",
        annotations={"org.opencontainers.image.description": "Flag rule ns2"},
    )

    # Config declares only ns1; the flag must override and span both.
    (project_dir / "grimoire.toml").write_text(
        f'[[registries]]\noci = "{REGISTRY_HOST}/{ns1}"\ndefault = true\n\n[skills]\n\n[rules]\n'
    )
    runner = grim_at(project_dir)

    reg1 = f"{REGISTRY_HOST}/{ns1}"
    reg2 = f"{REGISTRY_HOST}/{ns2}"
    if style == "comma":
        flag_args = ["--registry", f"{reg1},{reg2}"]
    else:
        flag_args = ["--registry", reg1, "--registry", reg2]

    result = runner.run("--format", "json", "search", *flag_args, "--refresh", check=False)
    assert result.returncode == 0, (
        f"multi-registry --registry ({style}) must exit 0, got {result.returncode}; "
        f"stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)
    repos = [r.get("repo", "") for r in rows]

    assert any("flag-skill-ns1" in repo for repo in repos), (
        f"--registry ({style}) must browse the first registry, got repos: {repos}"
    )
    assert any("flag-rule-ns2" in repo for repo in repos), (
        f"--registry ({style}) must browse the second registry too, got repos: {repos}"
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
        f'oci = "{REGISTRY_HOST}/{ns1}"\n'
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


# ---------------------------------------------------------------------------
# Test 4 — partial failure: one unreachable registry degrades gracefully
# ---------------------------------------------------------------------------


def test_search_partial_registry_failure_degrades_to_reachable(
    grim_at, project_dir: Path, registry: str
) -> None:
    """One unreachable ``[[registries]]`` entry must not fail the whole browse.

    ``grim search`` fans out one task per declared registry and catches a
    per-registry failure (degrading it to an empty group) rather than
    propagating it. With two registries declared — one reachable, one pointing
    at a dead port — the command must:

    - exit 0 (the per-registry failure never becomes the process exit code)
    - still surface the reachable registry's artifact

    The unreachable entry uses ``localhost:9999`` (nothing listening), which
    refuses the connection immediately, so the test stays fast and hermetic —
    that namespace is never published to the shared registry.
    """
    ns_good = f"grim-test/{uuid.uuid4().hex[:12]}"

    cfg_text = (
        f'[[registries]]\n'
        f'alias = "good"\n'
        f'oci = "{REGISTRY_HOST}/{ns_good}"\n'
        f'default = true\n'
        f'\n'
        f'[[registries]]\n'
        f'alias = "bad"\n'
        f'oci = "localhost:9999/grim-test/unreachable"\n'
        f'\n'
        f'[skills]\n'
        f'\n'
        f'[rules]\n'
    )
    (project_dir / "grimoire.toml").write_text(cfg_text)
    runner = grim_at(project_dir)

    make_artifact(
        f"{ns_good}/reachable-skill",
        "skill",
        {"reachable-skill/SKILL.md": "---\nname: reachable-skill\ndescription: works\n---\n# OK\n"},
        tag="latest",
        annotations={"org.opencontainers.image.description": "Reachable artifact"},
    )

    result = runner.run("--format", "json", "search", "--refresh", check=False)
    assert result.returncode == 0, (
        f"search with one unreachable registry must still exit 0, "
        f"got {result.returncode}; stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)
    assert isinstance(rows, list), f"search must return a JSON array, got {rows!r}"

    repos = [r.get("repo", "") for r in rows]
    assert any("reachable-skill" in repo for repo in repos), (
        f"search must surface the reachable registry's artifact despite the "
        f"unreachable one, got repos: {repos}"
    )


# ---------------------------------------------------------------------------
# Test 5 — no dedup: the same repo in two registries surfaces twice
# ---------------------------------------------------------------------------


def test_search_same_repo_in_two_registries_is_not_deduped(
    grim_at, project_dir: Path, registry: str
) -> None:
    """The same repo name in two registries surfaces as two distinct rows.

    The catalog is registry-grouped and flattened by fully-qualified
    ``registry/repository`` reference with no dedup or precedence step, so a
    repository published under the SAME bare name in two declared registries
    must appear TWICE — once per registry — never collapsed to one winner.
    This pins the "browse all, disambiguate by registry" contract: a future
    accidental dedup-by-bare-name would silently hide a registry's copy.
    """
    ns1 = f"grim-test/{uuid.uuid4().hex[:12]}"
    ns2 = f"grim-test/{uuid.uuid4().hex[:12]}"
    shared = "shared-tool"

    make_artifact(
        f"{ns1}/{shared}",
        "skill",
        {f"{shared}/SKILL.md": f"---\nname: {shared}\ndescription: from reg1\n---\n# R1\n"},
        tag="latest",
        annotations={"org.opencontainers.image.description": "Shared tool, registry 1"},
    )
    make_artifact(
        f"{ns2}/{shared}",
        "skill",
        {f"{shared}/SKILL.md": f"---\nname: {shared}\ndescription: from reg2\n---\n# R2\n"},
        tag="latest",
        annotations={"org.opencontainers.image.description": "Shared tool, registry 2"},
    )

    _two_namespace_config(project_dir, ns1, ns2)
    runner = grim_at(project_dir)

    result = runner.run("--format", "json", "search", "--refresh", check=False)
    assert result.returncode == 0, (
        f"multi-registry search must exit 0, got {result.returncode}; stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)
    assert isinstance(rows, list), f"search must return a JSON array, got {rows!r}"

    shared_repos = [r.get("repo", "") for r in rows if shared in r.get("repo", "")]
    assert len(shared_repos) == 2, (
        f"the same repo in two registries must surface twice (no dedup), "
        f"got: {shared_repos}"
    )
    assert any(f"{REGISTRY_HOST}/{ns1}" in repo for repo in shared_repos), (
        f"registry 1's copy of '{shared}' must appear, got: {shared_repos}"
    )
    assert any(f"{REGISTRY_HOST}/{ns2}" in repo for repo in shared_repos), (
        f"registry 2's copy of '{shared}' must appear, got: {shared_repos}"
    )
    assert shared_repos[0] != shared_repos[1], (
        f"the two copies must be distinct fully-qualified refs, got: {shared_repos}"
    )


# ---------------------------------------------------------------------------
# Test 6 — legacy [options].default_registry still resolves (back-compat lock)
# ---------------------------------------------------------------------------


def test_legacy_default_registry_still_resolves(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """A hand-written ``[options].default_registry`` config must still resolve
    short references — backward compatibility guard for the deprecation.

    After P2 migrates init to ``[[registries]]``, a pre-existing config using
    the legacy field must continue to work unchanged (deprecate-and-read, not
    remove).
    """
    make_artifact(
        f"{unique_repo}/legacy-tool",
        "skill",
        {"legacy-tool/SKILL.md": "---\nname: legacy-tool\ndescription: d\n---\n# L\n"},
        tag="1",
    )

    # Hand-write the legacy config shape.
    (project_dir / "grimoire.toml").write_text(
        f'[options]\ndefault_registry = "{REGISTRY_HOST}"\n\n[skills]\n\n[rules]\n'
    )

    runner = grim_at(project_dir)
    runner.env.pop("GRIM_DEFAULT_REGISTRY", None)

    short_ref = f"{unique_repo}/legacy-tool:1"
    out = runner.json("add", short_ref)
    assert out["kind"] == "skill"
    assert out["status"] == "added", f"add with legacy config must succeed: {out!r}"

    cfg = (project_dir / "grimoire.toml").read_text()
    assert f"{REGISTRY_HOST}/{unique_repo}/legacy-tool" in cfg, (
        f"legacy-resolved skill binding must use the legacy registry host; got:\n{cfg}"
    )
    # The legacy field must survive re-serialization (no destructive migration).
    assert f'default_registry = "{REGISTRY_HOST}"' in cfg, (
        f"write_config must preserve the legacy default_registry; got:\n{cfg}"
    )


# ---------------------------------------------------------------------------
# Test 7 — both fields present: array wins, legacy not used for short refs
# ---------------------------------------------------------------------------


def test_both_fields_array_wins(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """When both ``[options].default_registry`` and ``[[registries]]`` are
    present, ``[[registries]]`` is authoritative for short-ref expansion.

    A legacy ``default_registry`` pointing at the real registry and a
    ``[[registries]]`` entry pointing at a non-existent host: if the array
    wins, add fails (non-existent host); if the legacy wins, add succeeds.
    We assert that the array wins (add fails with a network error, not success
    with the legacy host).
    """
    make_artifact(
        f"{unique_repo}/both-tool",
        "skill",
        {"both-tool/SKILL.md": "---\nname: both-tool\ndescription: d\n---\n# B\n"},
        tag="1",
    )

    # Array points at a dead host; legacy points at the real registry.
    # If the resolver erroneously uses the legacy path, add would succeed.
    dead_host = "localhost:9999"
    (project_dir / "grimoire.toml").write_text(
        f'[options]\n'
        f'default_registry = "{REGISTRY_HOST}"\n'
        f'\n'
        f'[[registries]]\n'
        f'oci = "{dead_host}"\n'
        f'default = true\n'
        f'\n'
        f'[skills]\n'
        f'\n'
        f'[rules]\n'
    )

    runner = grim_at(project_dir)
    runner.env.pop("GRIM_DEFAULT_REGISTRY", None)

    # add must attempt the dead host (array wins), fail, and exit non-zero.
    result = runner.run("--format", "json", "add", f"{unique_repo}/both-tool:1", check=False)
    assert result.returncode != 0, (
        f"add must fail when [[registries]] points at an unreachable host; "
        f"if it succeeded, legacy default_registry was used instead of the array. "
        f"returncode={result.returncode}, stdout={result.stdout!r}"
    )


# ---------------------------------------------------------------------------
# Test 8 — two default = true entries are rejected with exit 78
# ---------------------------------------------------------------------------


def test_two_defaults_rejected(
    grim_at, project_dir: Path
) -> None:
    """A ``grimoire.toml`` with two ``[[registries]]`` entries both carrying
    ``default = true`` must be rejected at parse time with exit 78 (EX_CONFIG)
    and the error must mention "default".
    """
    (project_dir / "grimoire.toml").write_text(
        '[[registries]]\n'
        'oci = "ghcr.io/acme"\n'
        'default = true\n'
        '\n'
        '[[registries]]\n'
        'oci = "registry.corp/team"\n'
        'default = true\n'
        '\n'
        '[skills]\n'
        '\n'
        '[rules]\n'
    )

    runner = grim_at(project_dir)
    result = runner.run("status", check=False)
    assert result.returncode == 78, (
        f"two default = true entries must exit 78 (EX_CONFIG), "
        f"got {result.returncode}; stderr: {result.stderr}"
    )
    assert "default" in result.stderr.lower(), (
        f"error message must mention 'default'; got: {result.stderr!r}"
    )
