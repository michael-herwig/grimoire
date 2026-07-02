# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""T8 acceptance tests: multi-registry data-layer seam consumed by the TUI.

The TUI delegates resolution and catalog loading to the same
``catalog_service::load_catalog`` seam that ``grim search`` uses (D-LOAD).
The honest integration proof is to exercise that seam against the real
``localhost:5000`` registry.

**TUI runtime stays pytest-excluded** (interactive TTY; no PTY harness).
These tests assert the data layer — resolution, projection, degrade, order —
that the TUI consumes, not the interactive terminal rendering.

Cases (T8 spec from plan_multi_repo_tui.md):

1. Two namespaced registries → catalog/search surfaces both with distinct
   fully-qualified ``registry/repository`` refs (proves D-TREE input is correct).
2. Partial failure (dead port) → exit 0, reachable registry still surfaces,
   offline registry degrades (not a total failure).
3. Same repo name in two registries → not deduped (distinct FQ rows).
4. ``--registry`` flag / ``GRIM_DEFAULT_REGISTRY`` env collapses to one even
   when ``[[registries]]`` array is configured (D-RESOLVE).
5. Resolved registry order matches declaration order (feeds F13 root order).

Tests 1-3 may already pass (regression guards for shipped logic). Tests 4-5
cover the collapse and ordering contracts that the TUI's elision logic depends
on. All tests are expected to PASS; failures indicate a regression in the
data-layer seam that would also break the TUI.
"""
from __future__ import annotations

import json
import uuid
from pathlib import Path

import pytest

from src.helpers import make_artifact
from src.registry import REGISTRY_HOST


# ---------------------------------------------------------------------------
# Helpers (mirror _two_namespace_config from test_registries.py)
# ---------------------------------------------------------------------------


def _two_ns_config(project_dir: Path, ns1: str, ns2: str) -> None:
    """Write a grimoire.toml with two ``[[registries]]`` entries (ns1 declared first)."""
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
# T8.1 — fully-qualified registry/repository in search output
# ---------------------------------------------------------------------------


def test_tui_seam_two_registries_repo_field_is_fully_qualified(
    grim_at, project_dir: Path, registry: str
) -> None:
    """D-TREE: the ``repo`` field in search output must be the fully-qualified
    ``registry/repository`` reference (including the registry URL), not just the
    bare repository path.

    This proves that the data layer supplies correct authoritative ``registry``
    and ``repository`` values for the TUI tree grouper (D-TREE): if ``repo``
    were just the bare name (without the registry prefix), namespaced
    multi-registry grouping would be impossible.
    """
    ns1 = f"grim-test/{uuid.uuid4().hex[:12]}"
    ns2 = f"grim-test/{uuid.uuid4().hex[:12]}"

    make_artifact(
        f"{ns1}/skill-a",
        "skill",
        {"skill-a/SKILL.md": "---\nname: skill-a\ndescription: from reg1\n---\n# A\n"},
        tag="latest",
    )
    make_artifact(
        f"{ns2}/skill-b",
        "skill",
        {"skill-b/SKILL.md": "---\nname: skill-b\ndescription: from reg2\n---\n# B\n"},
        tag="latest",
    )

    _two_ns_config(project_dir, ns1, ns2)
    runner = grim_at(project_dir)

    result = runner.run("--format", "json", "search", "--refresh", check=False)
    assert result.returncode == 0, (
        f"multi-registry search must exit 0; stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)

    # Collect the `repo` fields from both artifacts
    skill_a_rows = [r for r in rows if "skill-a" in r.get("repo", "")]
    skill_b_rows = [r for r in rows if "skill-b" in r.get("repo", "")]

    assert skill_a_rows, "skill-a must appear in search results"
    assert skill_b_rows, "skill-b must appear in search results"

    # Each `repo` field must include the registry prefix — not just the bare name.
    # This proves the seam delivers a fully-qualified reference to the TUI.
    repo_a = skill_a_rows[0]["repo"]
    repo_b = skill_b_rows[0]["repo"]

    assert REGISTRY_HOST in repo_a, (
        f"skill-a repo must include the registry host '{REGISTRY_HOST}'; got: {repo_a!r}"
    )
    assert REGISTRY_HOST in repo_b, (
        f"skill-b repo must include the registry host '{REGISTRY_HOST}'; got: {repo_b!r}"
    )
    assert ns1 in repo_a, (
        f"skill-a repo must include namespace '{ns1}'; got: {repo_a!r}"
    )
    assert ns2 in repo_b, (
        f"skill-b repo must include namespace '{ns2}'; got: {repo_b!r}"
    )
    # The two repos must be distinct fully-qualified refs (different namespaces)
    assert repo_a != repo_b, (
        f"repos from distinct namespaces must be distinct FQ refs; got: {repo_a!r} == {repo_b!r}"
    )


# ---------------------------------------------------------------------------
# T8.2 — partial failure (dead port) degrades gracefully (regression guard)
# ---------------------------------------------------------------------------


def test_tui_seam_partial_registry_failure_exit_zero_and_healthy_surfaces(
    grim_at, project_dir: Path, registry: str
) -> None:
    """D-DEGRADE: one unreachable registry must not fail the whole browse.

    The TUI relies on the same degradation contract: a failed registry must
    produce an empty group so other registries still render, and the process
    must exit 0 so the user can still use the TUI.

    This is a regression guard for the shipped ``load_catalog`` seam.
    """
    ns_good = f"grim-test/{uuid.uuid4().hex[:12]}"
    make_artifact(
        f"{ns_good}/healthy-skill",
        "skill",
        {"healthy-skill/SKILL.md": "---\nname: healthy-skill\ndescription: works\n---\n# H\n"},
        tag="latest",
    )

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

    result = runner.run("--format", "json", "search", "--refresh", check=False)
    assert result.returncode == 0, (
        f"partial failure must exit 0 (healthy registry still surfaces); "
        f"got rc={result.returncode}; stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)
    repos = [r.get("repo", "") for r in rows]
    assert any("healthy-skill" in repo for repo in repos), (
        f"healthy registry must surface its artifact despite the unreachable one; "
        f"got repos: {repos}"
    )


# ---------------------------------------------------------------------------
# T8.3 — same repo in two registries is NOT deduped (regression guard)
# ---------------------------------------------------------------------------


def test_tui_seam_same_repo_name_in_two_registries_not_deduped(
    grim_at, project_dir: Path, registry: str
) -> None:
    """No dedup: same bare repo name in two registries → two distinct FQ rows.

    The TUI must not merge these — each gets its own tree node under its
    own registry root. This is a regression guard for ``load_catalog``'s
    registry-grouped output.
    """
    ns1 = f"grim-test/{uuid.uuid4().hex[:12]}"
    ns2 = f"grim-test/{uuid.uuid4().hex[:12]}"
    shared = "duplicate-tool"

    make_artifact(
        f"{ns1}/{shared}",
        "skill",
        {f"{shared}/SKILL.md": f"---\nname: {shared}\ndescription: reg1\n---\n# R1\n"},
        tag="latest",
    )
    make_artifact(
        f"{ns2}/{shared}",
        "skill",
        {f"{shared}/SKILL.md": f"---\nname: {shared}\ndescription: reg2\n---\n# R2\n"},
        tag="latest",
    )

    _two_ns_config(project_dir, ns1, ns2)
    runner = grim_at(project_dir)

    result = runner.run("--format", "json", "search", "--refresh", check=False)
    assert result.returncode == 0, (
        f"multi-registry search must exit 0; stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)
    dup_repos = [r.get("repo", "") for r in rows if shared in r.get("repo", "")]
    assert len(dup_repos) == 2, (
        f"same repo name in two registries must surface twice (no dedup); "
        f"got: {dup_repos}"
    )
    assert dup_repos[0] != dup_repos[1], (
        f"two rows for '{shared}' must have distinct FQ refs; got: {dup_repos}"
    )
    assert any(f"{REGISTRY_HOST}/{ns1}" in r for r in dup_repos), (
        f"registry 1 copy must appear; got: {dup_repos}"
    )
    assert any(f"{REGISTRY_HOST}/{ns2}" in r for r in dup_repos), (
        f"registry 2 copy must appear; got: {dup_repos}"
    )


# ---------------------------------------------------------------------------
# T8.4 — --registry flag collapses to one even with [[registries]] array
# ---------------------------------------------------------------------------


def test_tui_seam_registry_flag_collapses_to_single(
    grim_at, project_dir: Path, registry: str
) -> None:
    """D-RESOLVE: ``--registry <url>`` collapses the browse set to exactly one
    registry, even when a multi-entry ``[[registries]]`` array is configured.

    The TUI passes ``--registry`` through ``resolve_registries_for_tui`` (C1),
    which must collapse to a single-element set. This test proves the search
    seam enforces the same collapse, so the TUI's elision logic (D-ELIDE) can
    rely on ``len(registries) == 1`` whenever ``--registry`` is given.
    """
    ns1 = f"grim-test/{uuid.uuid4().hex[:12]}"
    ns2 = f"grim-test/{uuid.uuid4().hex[:12]}"

    make_artifact(
        f"{ns1}/only-reg1",
        "skill",
        {"only-reg1/SKILL.md": "---\nname: only-reg1\ndescription: from reg1\n---\n# 1\n"},
        tag="latest",
    )
    make_artifact(
        f"{ns2}/only-reg2",
        "skill",
        {"only-reg2/SKILL.md": "---\nname: only-reg2\ndescription: from reg2\n---\n# 2\n"},
        tag="latest",
    )

    _two_ns_config(project_dir, ns1, ns2)
    runner = grim_at(project_dir)

    # Pass --registry pointing at ns1 only; ns2 must NOT appear in results.
    reg1_url = f"{REGISTRY_HOST}/{ns1}"
    result = runner.run(
        "--format", "json", "search", "--refresh", "--registry", reg1_url,
        check=False,
    )
    assert result.returncode == 0, (
        f"search with --registry collapse must exit 0; stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)
    repos = [r.get("repo", "") for r in rows]

    assert any("only-reg1" in repo for repo in repos), (
        f"--registry {reg1_url!r} must surface reg1 artifact; got repos: {repos}"
    )
    assert not any("only-reg2" in repo for repo in repos), (
        f"--registry must collapse to one; reg2 artifact must NOT appear when --registry "
        f"points at reg1; got repos: {repos}"
    )


# ---------------------------------------------------------------------------
# T8.4b — GRIM_DEFAULT_REGISTRY env does NOT override [[registries]] array
# ---------------------------------------------------------------------------


def test_tui_seam_grim_default_registry_env_does_not_override_registries(
    grim_at, project_dir: Path, registry: str
) -> None:
    """D-RESOLVE: ``GRIM_DEFAULT_REGISTRY`` must NOT collapse the browse set when
    ``[[registries]]`` is configured.

    The env var is a tier-3 single-default fallback, not a collapse trigger.
    Only the explicit ``--registry`` flag collapses to one registry. When
    ``[[registries]]`` entries are declared they are authoritative regardless of
    whether ``GRIM_DEFAULT_REGISTRY`` is set — the env is used only as the
    default when NO ``[[registries]]`` exist.
    """
    ns1 = f"grim-test/{uuid.uuid4().hex[:12]}"
    ns2 = f"grim-test/{uuid.uuid4().hex[:12]}"

    make_artifact(
        f"{ns1}/env-reg1",
        "skill",
        {"env-reg1/SKILL.md": "---\nname: env-reg1\ndescription: from reg1\n---\n# E1\n"},
        tag="latest",
    )
    make_artifact(
        f"{ns2}/env-reg2",
        "skill",
        {"env-reg2/SKILL.md": "---\nname: env-reg2\ndescription: from reg2\n---\n# E2\n"},
        tag="latest",
    )

    _two_ns_config(project_dir, ns1, ns2)
    runner = grim_at(project_dir)

    # Set GRIM_DEFAULT_REGISTRY to ns1 only — with [[registries]] declared the
    # env must be ignored for the browse set; BOTH registries must surface.
    reg1_url = f"{REGISTRY_HOST}/{ns1}"
    runner.env["GRIM_DEFAULT_REGISTRY"] = reg1_url

    result = runner.run("--format", "json", "search", "--refresh", check=False)
    assert result.returncode == 0, (
        f"search with GRIM_DEFAULT_REGISTRY set must exit 0; stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)
    repos = [r.get("repo", "") for r in rows]

    assert any("env-reg1" in repo for repo in repos), (
        f"reg1 artifact must still surface when GRIM_DEFAULT_REGISTRY={reg1_url!r}; "
        f"got repos: {repos}"
    )
    assert any("env-reg2" in repo for repo in repos), (
        f"reg2 artifact must ALSO surface — GRIM_DEFAULT_REGISTRY must NOT collapse "
        f"[[registries]] to one entry; got repos: {repos}"
    )


# ---------------------------------------------------------------------------
# T8.4c — GRIM_DEFAULT_REGISTRY env IS the single registry when no array
# ---------------------------------------------------------------------------


def test_tui_seam_grim_default_registry_env_selects_single_when_no_registries(
    grim_at, project_dir: Path, registry: str
) -> None:
    """D-RESOLVE: ``GRIM_DEFAULT_REGISTRY`` selects the single browse registry
    when no ``[[registries]]`` array is configured (tier-3 head).

    This is the intended use-case for the env var: a user without any
    ``[[registries]]`` in their config can override the built-in fallback by
    setting ``GRIM_DEFAULT_REGISTRY``. With two namespaces and the env pointing
    at ns1, only ns1 must appear in results.
    """
    ns1 = f"grim-test/{uuid.uuid4().hex[:12]}"
    ns2 = f"grim-test/{uuid.uuid4().hex[:12]}"

    make_artifact(
        f"{ns1}/env-only-reg1",
        "skill",
        {"env-only-reg1/SKILL.md": "---\nname: env-only-reg1\ndescription: from reg1\n---\n# E1\n"},
        tag="latest",
    )
    make_artifact(
        f"{ns2}/env-only-reg2",
        "skill",
        {"env-only-reg2/SKILL.md": "---\nname: env-only-reg2\ndescription: from reg2\n---\n# E2\n"},
        tag="latest",
    )

    # Config with NO [[registries]] — only minimal required sections.
    cfg_text = "[skills]\n\n[rules]\n"
    (project_dir / "grimoire.toml").write_text(cfg_text)
    runner = grim_at(project_dir)

    # Env points at ns1 only; with no [[registries]] this IS the browse target.
    reg1_url = f"{REGISTRY_HOST}/{ns1}"
    runner.env["GRIM_DEFAULT_REGISTRY"] = reg1_url

    result = runner.run("--format", "json", "search", "--refresh", check=False)
    assert result.returncode == 0, (
        f"search with GRIM_DEFAULT_REGISTRY and no [[registries]] must exit 0; "
        f"stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)
    repos = [r.get("repo", "") for r in rows]

    assert any("env-only-reg1" in repo for repo in repos), (
        f"GRIM_DEFAULT_REGISTRY={reg1_url!r} must select ns1 as the single browse "
        f"registry when no [[registries]] are declared; got repos: {repos}"
    )
    assert not any("env-only-reg2" in repo for repo in repos), (
        f"ns2 must NOT appear — env selected ns1 as the single default registry; "
        f"got repos: {repos}"
    )


# ---------------------------------------------------------------------------
# T8.5 — resolved registry order matches declaration order (F13)
# ---------------------------------------------------------------------------


def test_tui_seam_registry_result_order_matches_declaration_order(
    grim_at, project_dir: Path, registry: str
) -> None:
    """F13: `grim search` flat output preserves registry declaration order.

    ``into_flat_rows`` concatenates the per-registry groups in resolution
    precedence order (each group already sorted by repository) rather than
    merging globally by name. So reg1 (declared first, ``default=true``) must
    appear before reg2 in the result list, deterministically — regardless of
    how the two registries' repository names sort against each other.

    (The TUI tree derives its own root order from the grouped results plus its
    F13 sort; it does not consume this flat order. This test guards the search
    seam's own contract.)
    """
    ns1 = f"grim-test/{uuid.uuid4().hex[:12]}"
    ns2 = f"grim-test/{uuid.uuid4().hex[:12]}"

    # Publish artifacts in both registries
    make_artifact(
        f"{ns1}/first-reg-skill",
        "skill",
        {"first-reg-skill/SKILL.md": "---\nname: first-reg-skill\ndescription: reg1\n---\n# F\n"},
        tag="latest",
    )
    make_artifact(
        f"{ns2}/second-reg-skill",
        "skill",
        {"second-reg-skill/SKILL.md": "---\nname: second-reg-skill\ndescription: reg2\n---\n# S\n"},
        tag="latest",
    )

    _two_ns_config(project_dir, ns1, ns2)
    runner = grim_at(project_dir)

    result = runner.run("--format", "json", "search", "--refresh", check=False)
    assert result.returncode == 0, (
        f"multi-registry search must exit 0; stderr: {result.stderr}"
    )
    rows = json.loads(result.stdout)
    repos = [r.get("repo", "") for r in rows]

    # Find the position of each registry's artifact in the result list
    idx_reg1 = next(
        (i for i, r in enumerate(repos) if "first-reg-skill" in r), None
    )
    idx_reg2 = next(
        (i for i, r in enumerate(repos) if "second-reg-skill" in r), None
    )

    assert idx_reg1 is not None, (
        f"first-reg-skill (declared-first registry) must appear in results; got repos: {repos}"
    )
    assert idx_reg2 is not None, (
        f"second-reg-skill (declared-second registry) must appear in results; got repos: {repos}"
    )
    assert idx_reg1 < idx_reg2, (
        f"declaration order must be preserved: reg1 artifact (idx={idx_reg1}) must appear "
        f"before reg2 artifact (idx={idx_reg2}) in search results (F13); repos: {repos}"
    )
