# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim uninstall` acceptance tests.

`uninstall` is the full inverse of `install`: it deletes the
materialized editor files, drops the install-state record, and
undeclares the entry from the config + lock (unlike `remove`, which
only undeclares and leaves files on disk). This is the acceptance
surface for the shared uninstall seam the TUI delete action also uses.
"""
from __future__ import annotations

from pathlib import Path

from src.helpers import make_artifact, write_config


def _install_one(grim_at, project_dir: Path, unique_repo: str):
    sk = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\ndescription: d\n---\n# CR\n"},
        tag="stable",
    )
    write_config(project_dir, skills={"code-review": sk.fq})
    runner = grim_at(project_dir)
    runner.run("lock", check=False)
    rows = runner.json("install")
    assert {r["status"] for r in rows} == {"installed"}
    return runner, sk


def test_uninstall_deletes_files_record_and_declaration(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    runner, _ = _install_one(grim_at, project_dir, unique_repo)
    skill_dir = project_dir / ".claude/skills/code-review"
    assert (skill_dir / "SKILL.md").is_file()

    out = runner.json("uninstall", "skill", "code-review")
    assert out["kind"] == "skill"
    assert out["name"] == "code-review"
    assert out["status"] == "uninstalled"

    # Files gone, declaration gone, lock entry gone.
    assert not skill_dir.exists()
    assert "code-review" not in (project_dir / "grimoire.toml").read_text()
    lock = project_dir / "grimoire.lock"
    if lock.is_file():
        assert "code-review" not in lock.read_text()

    # status no longer knows the artifact (record dropped).
    status = runner.json("status")
    assert all(r["name"] != "code-review" for r in status)


def test_uninstall_is_idempotent(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    runner, _ = _install_one(grim_at, project_dir, unique_repo)
    assert runner.json("uninstall", "skill", "code-review")["status"] == "uninstalled"
    # Second uninstall: nothing left to do, reported (not an error).
    again = runner.json("uninstall", "skill", "code-review")
    assert again["status"] == "not-installed"


def test_uninstall_never_installed_is_reported_not_error(
    grim_at, project_dir: Path, registry: str
) -> None:
    write_config(project_dir)
    runner = grim_at(project_dir)
    out = runner.json("uninstall", "rule", "never-declared")
    assert out["status"] == "not-installed"


def test_uninstall_leaves_other_artifacts_intact(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    sk = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\ndescription: d\n---\n# CR\n"},
        tag="stable",
    )
    ru = make_artifact(
        f"{unique_repo}/rust-style",
        "rule",
        {"rust-style.md": "---\npaths: ['**/*.rs']\n---\n# rust\n"},
        tag="v1",
    )
    write_config(
        project_dir,
        skills={"code-review": sk.fq},
        rules={"rust-style": ru.fq},
    )
    runner = grim_at(project_dir)
    runner.run("lock", check=False)
    runner.json("install")

    runner.json("uninstall", "skill", "code-review")

    # The rule is untouched: still on disk and still declared.
    assert (project_dir / ".claude/rules/rust-style.md").is_file()
    assert "rust-style" in (project_dir / "grimoire.toml").read_text()
    assert not (project_dir / ".claude/skills/code-review").exists()
