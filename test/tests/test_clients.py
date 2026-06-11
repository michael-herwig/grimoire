# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 The Grimoire Authors
"""`grim install` multi-client acceptance tests — config `clients` array.

The ``[options].clients`` TOML array drives which client layouts receive
the materialized artifacts when ``--client`` is absent.  The ``--client``
flag overrides the config array for a single invocation.
"""
from __future__ import annotations

from pathlib import Path

from src.assertions import assert_not_exists, assert_path_exists
from src.helpers import make_artifact


def _build_toml(
    project_dir: Path,
    skill_ref: str,
    rule_ref: str,
    clients: list[str] | None,
) -> None:
    """Write a grimoire.toml with one skill+rule.

    ``clients`` writes ``[options].clients`` when a list is given; ``None``
    omits the ``[options]`` table entirely so default-client detection runs.
    """
    options = ""
    if clients is not None:
        clients_toml = ", ".join(f'"{c}"' for c in clients)
        options = f"[options]\nclients = [{clients_toml}]\n\n"
    toml = (
        f"{options}"
        "[skills]\n"
        f'code-review = "{skill_ref}"\n'
        "\n"
        "[rules]\n"
        f'rust-style = "{rule_ref}"\n'
    )
    (project_dir / "grimoire.toml").write_text(toml)


def _publish_skill_and_rule(unique_repo: str):
    """Publish a single skill + rule pair and return ``(skill, rule)``."""
    sk = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {
            "code-review/SKILL.md": "---\nname: code-review\ndescription: d\n---\n# CR\n",
            "code-review/scripts/run.sh": "echo hi\n",
        },
        tag="stable",
    )
    ru = make_artifact(
        f"{unique_repo}/rust-style",
        "rule",
        {"rust-style.md": "---\npaths: ['**/*.rs']\n---\n# Rust Style\nUse 4 spaces.\n"},
        tag="v1",
    )
    return sk, ru


def test_no_clients_config_installs_to_detected_clients(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """No ``--client`` and no ``[options].clients`` installs to the detected
    clients only.

    With ``.opencode`` and ``.github/instructions`` present (but no
    ``.claude``), the materialized artifacts land in those two layouts and
    NOT in ``.claude``.
    """
    sk, ru = _publish_skill_and_rule(unique_repo)
    # Pre-create the OpenCode + Copilot markers (NOT .claude). A bare
    # `.github/instructions` dir is the Copilot detection signal.
    (project_dir / ".opencode").mkdir(parents=True, exist_ok=True)
    (project_dir / ".github" / "instructions").mkdir(parents=True, exist_ok=True)
    _build_toml(project_dir, sk.fq, ru.fq, clients=None)

    runner = grim_at(project_dir)
    runner.run("lock", check=False)
    rows = runner.json("install")
    assert rows, "install must return a non-empty result set"
    assert all(r["status"] in ("installed", "unchanged") for r in rows), (
        f"all entries must be installed/unchanged, got: {rows}"
    )

    # Detected clients (OpenCode + Copilot) received the artifacts.
    assert_path_exists(project_dir / ".opencode/skills/code-review/SKILL.md")
    assert_path_exists(project_dir / ".opencode/rules/rust-style.md")
    assert_path_exists(project_dir / ".github/skills/code-review/SKILL.md")
    assert_path_exists(project_dir / ".github/instructions/rust-style.instructions.md")

    # Claude was NOT detected ⇒ no `.claude` artifacts.
    assert_not_exists(project_dir / ".claude/skills/code-review")
    assert_not_exists(project_dir / ".claude/rules/rust-style.md")


def test_no_detected_clients_falls_back_to_claude(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """No ``--client``, no ``[options].clients``, and no vendor dirs present
    falls back to ``[claude]``."""
    sk, ru = _publish_skill_and_rule(unique_repo)
    _build_toml(project_dir, sk.fq, ru.fq, clients=None)

    runner = grim_at(project_dir)
    runner.run("lock", check=False)
    rows = runner.json("install")
    assert rows, "install must return a non-empty result set"

    # The Claude fallback received the artifacts.
    assert_path_exists(project_dir / ".claude/skills/code-review/SKILL.md")
    assert_path_exists(project_dir / ".claude/rules/rust-style.md")
    # No other client layout was written.
    assert_not_exists(project_dir / ".opencode/skills/code-review")
    assert_not_exists(project_dir / ".github/instructions/rust-style.instructions.md")


def test_config_clients_array_installs_to_all_declared_clients(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """``clients = ["claude", "copilot"]`` in config installs to both without --client."""
    sk = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {
            "code-review/SKILL.md": "---\nname: code-review\ndescription: d\n---\n# CR\n",
            "code-review/scripts/run.sh": "echo hi\n",
        },
        tag="stable",
    )
    ru = make_artifact(
        f"{unique_repo}/rust-style",
        "rule",
        {"rust-style.md": "---\npaths: ['**/*.rs']\n---\n# Rust Style\nUse 4 spaces.\n"},
        tag="v1",
    )
    _build_toml(project_dir, sk.fq, ru.fq, ["claude", "copilot"])
    runner = grim_at(project_dir)
    runner.run("lock", check=False)

    rows = runner.json("install")
    assert rows, "install must return a non-empty result set"
    assert all(r["status"] in ("installed", "unchanged") for r in rows), (
        f"all entries must be installed/unchanged, got: {rows}"
    )

    # Claude layout.
    assert_path_exists(project_dir / ".claude/skills/code-review/SKILL.md")
    assert_path_exists(project_dir / ".claude/rules/rust-style.md")

    # Copilot layout — skill verbatim, rule transformed.
    assert_path_exists(project_dir / ".github/skills/code-review/SKILL.md")
    assert_path_exists(
        project_dir / ".github/instructions/rust-style.instructions.md"
    )


def test_client_flag_overrides_config_clients(
    grim_at, project_dir: Path, registry: str, unique_repo: str
) -> None:
    """``--client opencode`` overrides the config ``clients`` list."""
    sk = make_artifact(
        f"{unique_repo}/code-review",
        "skill",
        {"code-review/SKILL.md": "---\nname: code-review\ndescription: d\n---\n# CR\n"},
        tag="stable",
    )
    ru = make_artifact(
        f"{unique_repo}/rust-style",
        "rule",
        {"rust-style.md": "---\npaths: ['**/*.rs']\n---\n# Rust Style\nUse 4 spaces.\n"},
        tag="v1",
    )
    # Config declares claude+copilot; the test overrides to opencode only.
    _build_toml(project_dir, sk.fq, ru.fq, ["claude", "copilot"])
    runner = grim_at(project_dir)
    runner.run("lock", check=False)

    rows = runner.json("install", "--client", "opencode")
    assert rows, "install must return a non-empty result set"

    # OpenCode layout must exist.
    assert_path_exists(project_dir / ".opencode/skills/code-review/SKILL.md")
    assert_path_exists(project_dir / ".opencode/rules/rust-style.md")
