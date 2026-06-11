# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim search` acceptance tests — catalog query over the real registry.

The catalog is *bounded*: the query is a case-insensitive repository-name
prefilter (a registry-wide manifest walk is an explicit cut-line), so a
search uses a term present in the unique repo path. State is data:
`search` always exits 0 (no results ⇒ empty array). The interactive TUI
render loop is not acceptance-tested (its decision logic is covered by
headless Rust unit tests); only the non-TTY guard of `grim tui` is
smoke-checked here.
"""
from __future__ import annotations

import json
from pathlib import Path

from src.helpers import make_artifact, write_config
from src.registry import REGISTRY_HOST


def test_search_finds_matching_entries_with_kind_and_status(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\n---\n# CR\n"},
        tag="latest",
        annotations={
            "com.grimoire.keywords": "review,quality",
            "org.opencontainers.image.description": "Review code quality",
        },
    )
    make_artifact(
        f"{unique_repo}/rust-style",
        "rule",
        {"rust-style.md": "---\npaths: ['**/*.rs']\n---\n# rust\n"},
        tag="latest",
        annotations={
            "com.grimoire.keywords": "rust,lint",
            "org.opencontainers.image.description": "Rust style rules",
        },
    )
    runner = grim_at(project_dir)

    # The unique-repo segment is in the repo *name*, so the bounded
    # name-prefilter scopes the build to just this test's repos.
    rows = runner.json(
        "search", unique_repo, "--registry", REGISTRY_HOST, "--refresh"
    )
    by_repo = {r["repo"]: r for r in rows}
    sk = next(
        v for k, v in by_repo.items() if k.endswith(f"{unique_repo}/code-review")
    )
    ru = next(
        v for k, v in by_repo.items() if k.endswith(f"{unique_repo}/rust-style")
    )
    assert sk["kind"] == "skill"
    assert sk["status"] == "not-installed"
    assert sk["description"] == "Review code quality"
    assert ru["kind"] == "rule"


def test_search_exposes_summary_and_full_description(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """`com.grimoire.summary` surfaces in JSON alongside the full,
    untruncated `description`."""
    long_desc = (
        "A deliberately long description that would wrap a narrow "
        "terminal but must round-trip in full through the JSON output."
    )
    make_artifact(
        f"{unique_repo}/with-summary",
        "skill",
        {"with-summary/SKILL.md": "---\nname: with-summary\n---\n# WS\n"},
        tag="latest",
        annotations={
            "com.grimoire.summary": "short blurb",
            "org.opencontainers.image.description": long_desc,
        },
    )
    make_artifact(
        f"{unique_repo}/no-summary",
        "skill",
        {"no-summary/SKILL.md": "---\nname: no-summary\n---\n# NS\n"},
        tag="latest",
        annotations={
            "org.opencontainers.image.description": "only a description",
        },
    )
    runner = grim_at(project_dir)

    rows = runner.json(
        "search", unique_repo, "--registry", REGISTRY_HOST, "--refresh"
    )
    by_repo = {r["repo"]: r for r in rows}
    with_summary = next(
        v for k, v in by_repo.items() if k.endswith(f"{unique_repo}/with-summary")
    )
    no_summary = next(
        v for k, v in by_repo.items() if k.endswith(f"{unique_repo}/no-summary")
    )
    # Summary is exposed; the long description is preserved verbatim.
    assert with_summary["summary"] == "short blurb"
    assert with_summary["description"] == long_desc
    # Absent summary serializes as null, description still present.
    assert no_summary["summary"] is None
    assert no_summary["description"] == "only a description"


def test_search_query_miss_is_empty_array_exit_0(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    make_artifact(
        f"{unique_repo}/thing",
        "skill",
        {"thing/SKILL.md": "---\nname: thing\n---\n# t\n"},
        tag="latest",
    )
    runner = grim_at(project_dir)

    result = runner.run(
        "--format",
        "json",
        "search",
        "zzz-no-such-repo-zzz-",
        "--registry",
        REGISTRY_HOST,
        "--refresh",
        check=False,
    )
    assert result.returncode == 0
    arr = json.loads(result.stdout)
    assert isinstance(arr, list)
    assert arr == []


def test_search_refresh_repopulates(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    runner = grim_at(project_dir)
    # Cold catalog before the artifact exists (scoped to this repo).
    rows = runner.json(
        "search", unique_repo, "--registry", REGISTRY_HOST, "--refresh"
    )
    assert rows == []

    make_artifact(
        f"{unique_repo}/late",
        "skill",
        {"late/SKILL.md": "---\nname: late\n---\n# l\n"},
        tag="latest",
    )
    rows = runner.json(
        "search", unique_repo, "--registry", REGISTRY_HOST, "--refresh"
    )
    assert [r for r in rows if r["repo"].endswith(f"{unique_repo}/late")], (
        f"--refresh must repopulate the catalog, got {rows}"
    )


def test_search_status_flips_to_installed_after_install(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    art = make_artifact(
        f"{unique_repo}/installable",
        "skill",
        {"installable/SKILL.md": "---\nname: installable\n---\n# i\n"},
        tag="latest",
    )
    write_config(project_dir, skills={"installable": art.fq})
    runner = grim_at(project_dir)

    runner.run("lock", check=False)
    runner.run("install", check=False)

    rows = runner.json(
        "search", unique_repo, "--registry", REGISTRY_HOST, "--refresh"
    )
    match = [
        r for r in rows if r["repo"].endswith(f"{unique_repo}/installable")
    ]
    assert match, f"expected the installed repo in results, got {rows}"
    assert match[0]["status"] == "installed", match[0]


def test_search_offline_serves_cached_exit_0(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    make_artifact(
        f"{unique_repo}/cached",
        "skill",
        {"cached/SKILL.md": "---\nname: cached\n---\n# c\n"},
        tag="latest",
    )
    runner = grim_at(project_dir)
    # Warm the catalog cache online (scoped to this repo).
    warm = runner.json(
        "search", unique_repo, "--registry", REGISTRY_HOST, "--refresh"
    )
    assert [r for r in warm if r["repo"].endswith(f"{unique_repo}/cached")]

    result = runner.run(
        "--format",
        "json",
        "--offline",
        "search",
        unique_repo,
        "--registry",
        REGISTRY_HOST,
        check=False,
    )
    assert result.returncode == 0, (
        f"offline search must exit 0, got {result.returncode}; "
        f"{result.stderr}"
    )
    arr = json.loads(result.stdout)
    assert [r for r in arr if r["repo"].endswith(f"{unique_repo}/cached")], (
        f"offline must serve the warm cache, got {arr}"
    )


def test_search_multi_term_is_and(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """A whitespace-split query ANDs its terms: every term must match an
    entry (across repo / summary / description / keywords) for it to surface.

    A 2+ term query carries no single-term name prefilter (no substring can
    AND across terms), so an online ``--refresh`` would build the capped
    browse window and miss this test's UUID repos. We warm a catalog scoped
    to ``unique_repo`` with a single-term ``--refresh``, then run the
    two-term query ``--offline`` so the in-memory AND narrows the warm cache.
    """
    # Two repos under the same unique segment; only one carries `lint`.
    make_artifact(
        f"{unique_repo}/rust-lint",
        "rule",
        {"rust-lint.md": "---\npaths: ['**/*.rs']\n---\n# rust\n"},
        tag="latest",
        annotations={
            "com.grimoire.keywords": "rust,lint",
            "org.opencontainers.image.description": "Rust lint rules",
        },
    )
    make_artifact(
        f"{unique_repo}/rust-fmt",
        "rule",
        {"rust-fmt.md": "---\npaths: ['**/*.rs']\n---\n# fmt\n"},
        tag="latest",
        annotations={
            "com.grimoire.keywords": "rust,format",
            "org.opencontainers.image.description": "Rust format rules",
        },
    )
    runner = grim_at(project_dir)

    # Warm a catalog scoped to this test's repos (single-term prefilter).
    warm = runner.json(
        "search", unique_repo, "--registry", REGISTRY_HOST, "--refresh"
    )
    warm_repos = {r["repo"].split("/")[-1] for r in warm}
    assert {"rust-lint", "rust-fmt"} <= warm_repos, (
        f"warm catalog must hold both repos, got {warm_repos}"
    )

    # `<unique_repo> lint` over the warm cache: `lint` ANDs in memory and
    # matches only rust-lint (rust-fmt's keywords/description lack `lint`).
    rows = runner.json(
        "--offline", "search", f"{unique_repo} lint", "--registry", REGISTRY_HOST
    )
    repos = [r["repo"] for r in rows]
    assert any(r.endswith(f"{unique_repo}/rust-lint") for r in repos), (
        f"the entry matching both terms must surface, got {repos}"
    )
    assert not any(r.endswith(f"{unique_repo}/rust-fmt") for r in repos), (
        f"the entry missing the second term must be filtered out, got {repos}"
    )


def test_search_kind_keyword_filters(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """A bare kind keyword (`skill`/`rule`) filters by kind, not as a text
    term, ANDed with the rest of the query.

    A kind-keyword query carries no single-term name prefilter (no substring
    can express it), so an online ``--refresh`` would build the capped browse
    window over the whole shared registry and miss this test's UUID repos.
    We therefore warm a catalog scoped to ``unique_repo`` with a single-term
    ``--refresh`` first, then run the kind-filter query ``--offline`` so the
    in-memory matcher narrows the warm cache deterministically — exactly the
    offline-serves-cache path the matcher is designed to support.
    """
    make_artifact(
        f"{unique_repo}/a-skill",
        "skill",
        {"a-skill/SKILL.md": "---\nname: a-skill\n---\n# s\n"},
        tag="latest",
    )
    make_artifact(
        f"{unique_repo}/a-rule",
        "rule",
        {"a-rule.md": "---\npaths: ['**/*.rs']\n---\n# r\n"},
        tag="latest",
    )
    runner = grim_at(project_dir)

    # Warm a catalog scoped to this test's repos (single-term prefilter).
    warm = runner.json(
        "search", unique_repo, "--registry", REGISTRY_HOST, "--refresh"
    )
    warm_repos = {r["repo"].split("/")[-1] for r in warm}
    assert {"a-skill", "a-rule"} <= warm_repos, (
        f"warm catalog must hold both kinds, got {warm_repos}"
    )

    # `<unique_repo> rule` over the warm cache ⇒ only the rule survives.
    rule_rows = runner.json(
        "--offline", "search", f"{unique_repo} rule", "--registry", REGISTRY_HOST
    )
    rule_repos = [r["repo"] for r in rule_rows]
    assert all(r["kind"] == "rule" for r in rule_rows), rule_rows
    assert any(r.endswith(f"{unique_repo}/a-rule") for r in rule_repos), rule_repos
    assert not any(r.endswith(f"{unique_repo}/a-skill") for r in rule_repos), (
        f"the `rule` kind keyword must filter out the skill, got {rule_repos}"
    )

    # `<unique_repo> skill` over the warm cache ⇒ only the skill survives.
    skill_rows = runner.json(
        "--offline", "search", f"{unique_repo} skill", "--registry", REGISTRY_HOST
    )
    skill_repos = [r["repo"] for r in skill_rows]
    assert all(r["kind"] == "skill" for r in skill_rows), skill_rows
    assert any(r.endswith(f"{unique_repo}/a-skill") for r in skill_repos), skill_repos
    assert not any(r.endswith(f"{unique_repo}/a-rule") for r in skill_repos), (
        f"the `skill` kind keyword must filter out the rule, got {skill_repos}"
    )


def test_tui_non_tty_exits_0_with_message(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """`grim tui` under a non-TTY stdout must not enter raw mode."""
    runner = grim_at(project_dir)
    result = runner.run("tui", "--registry", REGISTRY_HOST, check=False)
    assert result.returncode == 0, (
        f"non-TTY tui must exit 0, got {result.returncode}; {result.stderr}"
    )
    assert (
        "not a TTY" in result.stdout
        or "interactive terminal" in result.stdout
    )


def test_tui_refresh_flag_non_tty_exits_0(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """`grim tui --refresh` parses and still honours the non-TTY guard: the
    new flag governs only the initial load and never changes the exit guard.
    """
    runner = grim_at(project_dir)
    result = runner.run(
        "tui", "--registry", REGISTRY_HOST, "--refresh", check=False
    )
    assert result.returncode == 0, (
        f"non-TTY tui --refresh must exit 0, got {result.returncode}; "
        f"{result.stderr}"
    )
    assert (
        "not a TTY" in result.stdout
        or "interactive terminal" in result.stdout
    )
