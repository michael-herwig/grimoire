# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""Catalog-metadata acceptance tests — frontmatter/TOML → manifest annotations.

These build a *real* local skill / rule / bundle and `grim release` it, then
fetch the pushed manifest off the live registry and assert the authored
metadata (summary, keywords, description) lands in the OCI annotations. This
exercises the full publish path (`annotations_for_*`) that the Rust unit tests
cover in isolation, for every artifact kind.

Authoring conventions under test:
- skill: `metadata.summary` / `metadata.keywords` / `metadata.repository`
  (the SKILL.md spec map), `description` is its own frontmatter field.
- rule:  top-level `summary` / `keywords` / `repository`; description
  derived from the body.
- bundle: top-level `summary` / `keywords` / `description` / `repository`
  in the TOML.
- agent: `metadata.repository` (like skills).
Keywords are a comma-separated string in every format (the OCI annotation is a
string); description on a bundle overrides the auto `bundle of N members`.
An authored `repository` HTTPS URL becomes `org.opencontainers.image.source`
(spec-correct), winning over the tagless release-ref fallback; a non-HTTPS
value hard-fails the release (exit 65).
"""
from __future__ import annotations

from pathlib import Path

from src.registry import fetch_manifest


def _write(p: Path, body: str) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(body)


def test_skill_metadata_becomes_annotations(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    skill = project_dir / "code-review"
    _write(
        skill / "SKILL.md",
        "---\nname: code-review\n"
        "description: A thorough multi-pass reviewer covering correctness and style.\n"
        "metadata:\n  summary: Multi-pass code reviewer\n  keywords: review,quality\n"
        "---\n# CR\n",
    )
    repo_path = f"{unique_repo}/code-review"
    runner = grim_at(project_dir)
    runner.json("release", str(skill), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert ann["com.grimoire.summary"] == "Multi-pass code reviewer"
    assert ann["com.grimoire.keywords"] == "review,quality"
    assert (
        ann["org.opencontainers.image.description"]
        == "A thorough multi-pass reviewer covering correctness and style."
    )


def test_rule_metadata_becomes_annotations(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    rule = project_dir / "rust-style.md"
    _write(
        rule,
        "---\npaths: ['**/*.rs']\n"
        "summary: Idiomatic Rust style\nkeywords: rust,lint\n"
        "---\n# Rust Style\nPrefer idiomatic patterns.\n",
    )
    repo_path = f"{unique_repo}/rust-style"
    runner = grim_at(project_dir)
    runner.json("release", str(rule), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert ann["com.grimoire.summary"] == "Idiomatic Rust style"
    assert ann["com.grimoire.keywords"] == "rust,lint"
    # A rule has no description field — it is derived from the body's heading.
    assert ann["org.opencontainers.image.description"] == "Rust Style"


def test_rule_keywords_list_is_rejected_string_only(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Keywords are string-only; a YAML list in a rule is silently ignored
    (not joined), so no keywords annotation is emitted."""
    rule = project_dir / "listy.md"
    _write(
        rule,
        "---\npaths: ['**/*.rs']\nkeywords:\n  - rust\n  - lint\n---\n# Listy\nbody\n",
    )
    repo_path = f"{unique_repo}/listy"
    runner = grim_at(project_dir)
    runner.json("release", str(rule), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert "com.grimoire.keywords" not in ann


def test_bundle_metadata_becomes_annotations(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    bundle = project_dir / "python-stack.toml"
    _write(
        bundle,
        'summary = "Python dev stack"\n'
        'keywords = "python,lint,test"\n'
        'description = "Skills and rules for Python work"\n\n'
        "[skills]\n"
        'code-review = "ghcr.io/acme/code-review:1"\n'
        "[rules]\n"
        'rust-style = "ghcr.io/acme/rust-style:2"\n',
    )
    repo_path = f"{unique_repo}/python-stack"
    runner = grim_at(project_dir)
    runner.json("release", str(bundle), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert ann["com.grimoire.summary"] == "Python dev stack"
    assert ann["com.grimoire.keywords"] == "python,lint,test"
    assert (
        ann["org.opencontainers.image.description"] == "Skills and rules for Python work"
    )


def test_skill_repository_becomes_source_annotation(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    skill = project_dir / "code-review"
    _write(
        skill / "SKILL.md",
        "---\nname: code-review\ndescription: d\n"
        "metadata:\n  repository: https://github.com/acme/code-review\n"
        "---\n# CR\n",
    )
    repo_path = f"{unique_repo}/code-review"
    runner = grim_at(project_dir)
    runner.json("release", str(skill), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert (
        ann["org.opencontainers.image.source"] == "https://github.com/acme/code-review"
    ), "authored repository must win the source annotation"


def test_rule_repository_becomes_source_annotation(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    rule = project_dir / "rust-style.md"
    _write(
        rule,
        "---\npaths: ['**/*.rs']\n"
        "repository: https://gitlab.com/acme/rust-style\n"
        "---\n# Rust Style\nbody\n",
    )
    repo_path = f"{unique_repo}/rust-style"
    runner = grim_at(project_dir)
    runner.json("release", str(rule), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert (
        ann["org.opencontainers.image.source"] == "https://gitlab.com/acme/rust-style"
    )


def test_agent_repository_becomes_source_annotation(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    agent = project_dir / "reviewer.md"
    _write(
        agent,
        "---\nname: reviewer\ndescription: d\n"
        "metadata:\n  repository: https://github.com/acme/reviewer\n"
        "---\nbody\n",
    )
    repo_path = f"{unique_repo}/reviewer"
    runner = grim_at(project_dir)
    runner.json("release", "--kind", "agent", str(agent), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert ann["org.opencontainers.image.source"] == "https://github.com/acme/reviewer"


def test_bundle_repository_becomes_source_annotation(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    bundle = project_dir / "python-stack.toml"
    _write(
        bundle,
        'repository = "https://github.com/acme/python-stack"\n\n'
        "[skills]\n"
        'code-review = "ghcr.io/acme/code-review:1"\n',
    )
    repo_path = f"{unique_repo}/python-stack"
    runner = grim_at(project_dir)
    runner.json("release", str(bundle), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert (
        ann["org.opencontainers.image.source"] == "https://github.com/acme/python-stack"
    )


def test_source_annotation_falls_back_to_release_ref(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """No authored repository ⇒ the tagless release ref is kept in the
    source annotation (pre-repository continuity behavior, pinned)."""
    skill = project_dir / "plain"
    _write(skill / "SKILL.md", "---\nname: plain\ndescription: d\n---\n# P\n")
    repo_path = f"{unique_repo}/plain"
    runner = grim_at(project_dir)
    runner.json("release", str(skill), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert ann["org.opencontainers.image.source"] == f"{registry}/{repo_path}"


def test_release_rejects_non_https_repository(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    skill = project_dir / "bad-repo"
    _write(
        skill / "SKILL.md",
        "---\nname: bad-repo\ndescription: d\n"
        "metadata:\n  repository: git@github.com:acme/bad-repo.git\n"
        "---\n# B\n",
    )
    runner = grim_at(project_dir)
    result = runner.run(
        "release", str(skill), f"{registry}/{unique_repo}/bad-repo:1.0.0", check=False
    )
    assert result.returncode == 65, (
        f"non-HTTPS repository must exit 65, got {result.returncode}; {result.stderr}"
    )
    assert "repository" in result.stderr, result.stderr


def test_bundle_without_metadata_uses_default_description(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    bundle = project_dir / "bare-stack.toml"
    _write(
        bundle,
        "[skills]\n" 'code-review = "ghcr.io/acme/code-review:1"\n',
    )
    repo_path = f"{unique_repo}/bare-stack"
    runner = grim_at(project_dir)
    runner.json("release", str(bundle), f"{registry}/{repo_path}:1.0.0")

    ann = fetch_manifest(repo_path, "1.0.0")["annotations"]
    assert ann["org.opencontainers.image.description"] == "grimoire bundle of 1 members"
    assert "com.grimoire.summary" not in ann
    assert "com.grimoire.keywords" not in ann
