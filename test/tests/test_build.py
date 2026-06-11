# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim build` acceptance tests — validate + pack a local skill/rule."""
from __future__ import annotations

from pathlib import Path


def _write(p: Path, body: str) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(body)


def test_build_skill_dir(grim_at, project_dir: Path) -> None:
    skill = project_dir / "code-review"
    _write(
        skill / "SKILL.md",
        "---\nname: code-review\ndescription: Review code.\n---\n# Body\n",
    )
    _write(skill / "scripts/run.sh", "echo hi\n")

    runner = grim_at(project_dir)
    out = runner.json("build", str(skill))
    assert out["kind"] == "skill"
    assert out["name"] == "code-review"
    assert out["status"] == "built"
    assert out["layer_digest"].startswith("sha256:")
    assert out["annotation_count"] >= 1


def test_build_rule_file(grim_at, project_dir: Path) -> None:
    rule = project_dir / "rust-style.md"
    _write(rule, "---\npaths: ['**/*.rs']\n---\n# Rust Style\nUse 4 spaces.\n")

    runner = grim_at(project_dir)
    out = runner.json("build", str(rule))
    assert out["kind"] == "rule"
    assert out["name"] == "rust-style"
    assert out["status"] == "built"


def test_build_rejects_name_mismatch(grim_at, project_dir: Path) -> None:
    skill = project_dir / "code-review"
    _write(
        skill / "SKILL.md",
        "---\nname: wrong-name\ndescription: d\n---\n",
    )
    runner = grim_at(project_dir)
    result = runner.run("build", str(skill), check=False)
    assert result.returncode == 65, (
        f"name mismatch must exit 65, got {result.returncode}; {result.stderr}"
    )


def test_build_rejects_missing_skill_md(grim_at, project_dir: Path) -> None:
    skill = project_dir / "empty-skill"
    skill.mkdir(parents=True)
    runner = grim_at(project_dir)
    result = runner.run("build", str(skill), check=False)
    assert result.returncode in (65, 74), (
        f"missing SKILL.md must fail, got {result.returncode}; {result.stderr}"
    )


def test_build_rejects_non_https_repository(grim_at, project_dir: Path) -> None:
    """The repository publish gate fires at build time too (local
    pre-flight), for the rule's top-level authoring surface."""
    rule = project_dir / "bad-repo.md"
    _write(
        rule,
        "---\npaths: ['**/*.rs']\nrepository: http://github.com/acme/x\n---\n# R\nbody\n",
    )
    runner = grim_at(project_dir)
    result = runner.run("build", str(rule), check=False)
    assert result.returncode == 65, (
        f"non-HTTPS repository must exit 65, got {result.returncode}; {result.stderr}"
    )
    assert "repository" in result.stderr, result.stderr


def test_build_rejects_missing_description(grim_at, project_dir: Path) -> None:
    skill = project_dir / "code-review"
    _write(skill / "SKILL.md", "---\nname: code-review\n---\n# Body\n")
    runner = grim_at(project_dir)
    result = runner.run("build", str(skill), check=False)
    assert result.returncode == 65, (
        f"missing description must exit 65, got {result.returncode}; "
        f"{result.stderr}"
    )
