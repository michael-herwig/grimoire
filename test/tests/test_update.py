# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim update` acceptance tests (rolling release)."""
from __future__ import annotations

from pathlib import Path

from src.helpers import make_artifact, write_config
from src.registry import retag


def test_update_rewrites_lock_and_rematerializes(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    repo = f"{unique_repo}/code-review"
    v1 = make_artifact(
        repo, "skill", {"code-review/SKILL.md": "v1\n"}, tag="1.0.0"
    )
    make_artifact(  # floating tag initially points at v1
        repo, "skill", {"code-review/SKILL.md": "v1\n"}, tag="stable"
    )
    write_config(
        project_dir, skills={"code-review": f"{registry}/{repo}:stable"}
    )
    runner = grim_at(project_dir)
    runner.run("lock", check=False)
    runner.run("install", check=False)
    installed = project_dir / ".claude/skills/code-review/SKILL.md"
    assert installed.read_text() == "v1\n"

    # Publish v2 and move the floating tag onto it (rolling release).
    v2 = make_artifact(
        repo, "skill", {"code-review/SKILL.md": "v2\n"}, tag="2.0.0"
    )
    retag(repo, "stable", v2.digest)
    assert v1.digest != v2.digest

    rows = runner.json("update")
    assert rows[0]["action"] == "updated"
    assert installed.read_text() == "v2\n"
    assert v2.digest in (project_dir / "grimoire.lock").read_text()


def test_update_named_only_touches_that_artifact(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    a_repo = f"{unique_repo}/a"
    b_repo = f"{unique_repo}/b"
    make_artifact(a_repo, "rule", {"a.md": "a1\n"}, tag="latest")
    make_artifact(b_repo, "rule", {"b.md": "b1\n"}, tag="latest")
    write_config(
        project_dir,
        rules={
            "a": f"{registry}/{a_repo}:latest",
            "b": f"{registry}/{b_repo}:latest",
        },
    )
    runner = grim_at(project_dir)
    runner.run("lock", check=False)
    runner.run("install", check=False)

    a2 = make_artifact(a_repo, "rule", {"a.md": "a2\n"}, tag="latest")
    assert a2  # a's floating tag advanced; b unchanged

    rows = runner.json("update", "a")
    by_name = {r["name"]: r for r in rows}
    assert by_name["a"]["action"] == "updated"
    assert by_name["b"]["action"] == "unchanged"
    # A partial update carries non-named entries forward in the lock, so the
    # prune pass must not treat "b" as an orphan and delete it.
    assert all(r["action"] != "removed" for r in rows), "partial update must not prune the unnamed entry"
    assert (project_dir / ".claude/rules/a.md").read_text() == "a2\n"
    assert (project_dir / ".claude/rules/b.md").read_text() == "b1\n"


def test_partial_update_with_stale_lock_exits_65(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    a_repo = f"{unique_repo}/a"
    make_artifact(a_repo, "rule", {"a.md": "a1\n"}, tag="latest")
    write_config(project_dir, rules={"a": f"{registry}/{a_repo}:latest"})
    runner = grim_at(project_dir)
    runner.run("lock", check=False)

    # Mutate the declaration (add a rule) without re-locking, then ask for
    # a *partial* update — the stale-lock guard must refuse with 65.
    b_repo = f"{unique_repo}/b"
    make_artifact(b_repo, "rule", {"b.md": "b1\n"}, tag="latest")
    write_config(
        project_dir,
        rules={
            "a": f"{registry}/{a_repo}:latest",
            "b": f"{registry}/{b_repo}:latest",
        },
    )
    result = runner.run("update", "a", check=False)
    assert result.returncode == 65, (
        f"partial update on a stale lock must exit 65, got "
        f"{result.returncode}; {result.stderr}"
    )
