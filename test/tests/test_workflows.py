# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""End-to-end standard workflows, mirroring the documented walkthroughs.

Each test chains the exact command sequence a documentation page walks a
user through, so a regression in any step of a *standard workflow* fails
CI instead of silently rotting the docs and the manual rig. Unit-level
behavior of the individual commands lives in the per-command modules
(``test_add_*``, ``test_install``, ``test_update``, ``test_bundles``, …);
these tests assert the chains, not the details.

Covered chains:

- ``docs/src/quickstart.md`` — init → add → install → status → rolling
  release → update → uninstall (the canonical consumer walkthrough).
- ``docs/src/quickstart.md`` step 3 variant — one published rule installed
  into several clients at once via ``--client``.

The bundle lifecycle chain (release a ``.toml`` → add → expand → update
v1→v2 member add/remove) is already exercised end to end by
``test_bundles.py`` (``test_add_bundle_declares_and_locks``,
``test_update_adds_and_removes_in_one_upgrade``,
``test_release_bundle_pin_freezes_members``) and is deliberately not
repeated here.
"""
from __future__ import annotations

from pathlib import Path


def _write(p: Path, body: str) -> None:
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(body)


def _skill_md(marker: str) -> str:
    return (
        "---\n"
        "name: code-review\n"
        "description: Review a diff for risky changes.\n"
        "metadata:\n"
        "  summary: Diff reviewer\n"
        "  keywords: review,quality\n"
        "---\n"
        f"# Code Review {marker}\n"
    )


def test_quickstart_walkthrough(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """The full quickstart chain from ``docs/src/quickstart.md``."""
    repo = f"{registry}/{unique_repo}/code-review"
    runner = grim_at(project_dir)

    # A publisher ships 1.0.0; the cascade creates the floating :1 the
    # quickstart declares.
    skill = project_dir / "author" / "code-review"
    _write(skill / "SKILL.md", _skill_md("v1"))
    out = runner.json("release", str(skill), f"{repo}:1.0.0")
    assert out["pushed"] is True

    # 1. Create a project config.
    result = runner.run("init", "--registry", registry, check=False)
    assert result.returncode == 0, result.stderr
    assert (project_dir / "grimoire.toml").is_file()

    # 2. Declare an artifact — kind and name are inferred, and the
    #    floating tag is pinned immediately.
    out = runner.json("add", f"{repo}:1")
    assert out["kind"] == "skill"
    assert out["name"] == "code-review"
    assert out["status"] == "added"
    assert "@sha256:" in out["pinned"]

    # 3. Install into the AI client.
    rows = runner.json("install")
    assert {r["status"] for r in rows} == {"installed"}
    installed_index = project_dir / ".claude/skills/code-review/SKILL.md"
    assert "Code Review v1" in installed_index.read_text()

    # 4. Check the state.
    rows = runner.json("status")
    assert [r["state"] for r in rows] == ["installed"]

    # 5. Upgrade: the publisher ships 1.1.0 behind the same floating :1;
    #    a plain `grim update` rolls the lock forward and rematerializes.
    _write(skill / "SKILL.md", _skill_md("v2"))
    runner.json("release", str(skill), f"{repo}:1.1.0")
    rows = runner.json("update")
    by_name = {r["name"]: r for r in rows}
    assert by_name["code-review"]["action"] == "updated", (
        f"floating :1 must roll forward on update, got {by_name}"
    )
    assert "Code Review v2" in installed_index.read_text()
    rows = runner.json("status")
    assert [r["state"] for r in rows] == ["installed"]

    # Undo: uninstall removes files, install record, and declaration.
    out = runner.json("uninstall", "skill", "code-review")
    assert out["status"] == "uninstalled"
    assert not (project_dir / ".claude/skills/code-review").exists()
    assert "code-review" not in (project_dir / "grimoire.toml").read_text()


def test_quickstart_multiclient_install(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """Quickstart step 3 variant: ``grim install --client claude,copilot``.

    Publishes a real rule via ``grim release`` (not a raw registry push)
    and asserts the documented multi-client materialization: Claude gets
    the canonical rule, Copilot gets the transformed instructions file.
    """
    repo = f"{registry}/{unique_repo}/rust-style"
    rule = project_dir / "author" / "rust-style.md"
    _write(
        rule,
        "---\npaths: ['**/*.rs']\nsummary: Rust style\n---\n# Rust Style\n",
    )
    runner = grim_at(project_dir)
    runner.json("release", str(rule), f"{repo}:1.0.0")

    result = runner.run("init", "--registry", registry, check=False)
    assert result.returncode == 0, result.stderr
    runner.json("add", f"{repo}:1")

    rows = runner.json("install", "--client", "claude,copilot")
    assert {r["status"] for r in rows} == {"installed"}
    assert (project_dir / ".claude/rules/rust-style.md").is_file()
    assert (
        project_dir / ".github/instructions/rust-style.instructions.md"
    ).is_file(), "copilot must receive the transformed instructions file"
